//! Reference receiver for FrameLink JPEG streams over TCP and UDP.
use framelink::wire::{JpegStream, MAX_UDP_JPEG, validate_jpeg};
use std::{
    io::{self, Read},
    net::{TcpListener, UdpSocket},
    time::{Duration, Instant},
};
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || !matches!(args[1].as_str(), "tcp" | "udp") {
        eprintln!("Usage: cargo run --release --example receive -- tcp|udp 0.0.0.0:7878");
        std::process::exit(2);
    }
    let mut frames = 0u64;
    let mut bytes = 0u64;
    let mut last = Instant::now();
    let mut report = |jpeg: &[u8]| {
        // Decode or hand off to a bounded latest-frame consumer here.
        frames += 1;
        bytes += jpeg.len() as u64;
        if last.elapsed() >= Duration::from_secs(1) {
            println!(
                "{:.1} fps, {:.1} Mbps JPEG payload, last JPEG {} bytes",
                frames as f64 / last.elapsed().as_secs_f64(),
                bytes as f64 * 8.0 / last.elapsed().as_secs_f64() / 1e6,
                jpeg.len()
            );
            frames = 0;
            bytes = 0;
            last = Instant::now();
        }
    };
    if args[1] == "tcp" {
        let listener = TcpListener::bind(&args[2])?;
        println!("TCP listening on {}", listener.local_addr()?);
        loop {
            let (mut stream, peer) = listener.accept()?;
            println!("TCP connected: {peer}");
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut parser = JpegStream::default();
            let mut bytes = [0; 16384];
            loop {
                match stream.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => {
                        if parser.push(&bytes[..n], &mut report).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            println!("TCP session ended; waiting for reconnect");
        }
    } else {
        let socket = UdpSocket::bind(&args[2])?;
        println!("UDP listening on {}", socket.local_addr()?);
        let mut packet = [0; MAX_UDP_JPEG + 1];
        loop {
            let n = socket.recv(&mut packet)?;
            if validate_jpeg(&packet[..n], MAX_UDP_JPEG).is_ok() {
                report(&packet[..n]);
            }
        }
    }
}
