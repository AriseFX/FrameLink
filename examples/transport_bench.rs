//! Synthetic localhost JPEG encode/transport benchmark. Excludes capture, conversion,
//! physical NICs, decoding and inference. Never describes end-to-end latency.
use framelink::{
    model::{Config, Transport},
    transport::SocketSender,
    wire::{JpegStream, MAX_UDP_JPEG, validate_jpeg},
};
use std::{
    io::Read,
    net::{TcpListener, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err("Usage: transport_bench tcp|udp size fps seconds".into());
    }
    let transport = match args[1].as_str() {
        "tcp" => Transport::Tcp,
        "udp" => Transport::Udp,
        _ => return Err("expected tcp|udp".into()),
    };
    let size: u32 = args[2].parse()?;
    let fps: u32 = args[3].parse()?;
    let seconds: u64 = args[4].parse()?;
    if !(1..=60).contains(&seconds) {
        return Err("seconds must be 1..60".into());
    }
    let mut config = Config {
        transport,
        fps,
        ..Config::default()
    };
    config.region.width = size;
    config.region.height = size;
    config.validate()?;
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let tcp = TcpListener::bind("127.0.0.1:0")?;
    let udp = UdpSocket::bind("127.0.0.1:0")?;
    config.destination = if transport == Transport::Tcp {
        tcp.local_addr()?
    } else {
        udp.local_addr()?
    }
    .to_string();
    let mut sender = SocketSender::connect(&config)?;
    let receiver = std::thread::spawn(move || -> Result<u64, String> {
        let mut count = 0;
        if transport == Transport::Tcp {
            let (mut stream, _) = tcp.accept().map_err(|e| e.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(|e| e.to_string())?;
            let mut parser = JpegStream::default();
            let mut bytes = [0; 16384];
            loop {
                match stream.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => parser.push(&bytes[..n], |_| count += 1)?,
                    Err(e) => return Err(e.to_string()),
                }
            }
        } else {
            udp.set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(|e| e.to_string())?;
            let mut packet = [0; MAX_UDP_JPEG + 1];
            loop {
                match udp.recv(&mut packet) {
                    Ok(n) => {
                        if validate_jpeg(&packet[..n], MAX_UDP_JPEG).is_ok() {
                            count += 1;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        if done.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
        Ok(count)
    });
    // Deterministic changing, textured BGRA; unlike a flat frame it exercises
    // DCT/entropy coding and changes every frame. Not a real game workload.
    let mut pixels = vec![0; size as usize * size as usize * 4];
    for y in 0..size as usize {
        for x in 0..size as usize {
            let i = (y * size as usize + x) * 4;
            pixels[i..i + 4].copy_from_slice(&[
                ((x * 3 + y) % 256) as u8,
                ((x + y * 2) % 256) as u8,
                ((x ^ y) % 256) as u8,
                255,
            ]);
        }
    }
    let started = Instant::now();
    let mut next = started;
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut sent = 0;
    let mut submits = Vec::new();
    let mut error = None;
    let mut sent_bytes = 0u64;
    while started.elapsed() < Duration::from_secs(seconds) {
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let began = Instant::now();
        pixels[0] = pixels[0].wrapping_add(1);
        if let Err(e) = sender.encode(&pixels) {
            error = Some(e);
            break;
        }
        match sender.submit(&stop) {
            Ok(true) => {
                sent += 1;
                sent_bytes += sender.jpeg_len() as u64;
            }
            Ok(false) => {}
            Err(e) => {
                error = Some(e);
                break;
            }
        }
        submits.push(began.elapsed().as_secs_f64() * 1000.0);
        next = (next + interval).max(Instant::now());
    }
    let elapsed = started.elapsed().as_secs_f64();
    let dropped = sender.dropped;
    drop(sender);
    stop.store(true, Ordering::Release);
    let received = receiver.join().map_err(|_| "receiver panic")??;
    submits.sort_by(f64::total_cmp);
    let p95 = submits.get((submits.len() as f64 * 0.95) as usize).copied();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "scope":"synthetic_localhost_jpeg_encode_no_capture_or_decode", "transport":config.transport,
            "size":size,"requested_fps":fps,"seconds":elapsed,"sent":sent,"received":received,
            "sender_dropped":dropped,"received_fps":received as f64/elapsed,
            "sent_jpeg_mbps":sent_bytes as f64*8.0/elapsed/1e6,
            "encode_and_submit_p95_ms":p95,"error":error,
        }))?
    );
    if error.is_some() {
        return Err("sender failed; inspect report".into());
    }
    Ok(())
}
