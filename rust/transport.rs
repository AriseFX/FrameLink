//! APK-compatible JPEG socket transport; no application video queue.
use crate::{
    model::{Config, Transport},
    wire::{MAX_TCP_JPEG, MAX_UDP_JPEG, validate_jpeg},
};
use std::{
    io::{self, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

enum Socket {
    Tcp(TcpStream),
    Udp(UdpSocket),
}
pub struct SocketSender {
    socket: Socket,
    jpeg: Vec<u8>,
    width: u16,
    height: u16,
    quality: u8,
    timeout: Duration,
    pub dropped: u64,
}
impl SocketSender {
    pub fn connect(config: &Config) -> Result<Self, String> {
        config.validate()?;
        let destination: SocketAddr = config
            .destination
            .trim()
            .parse()
            .map_err(|_| "无效的目标地址")?;
        let socket = match config.transport {
            Transport::Tcp => {
                let stream = TcpStream::connect_timeout(&destination, Duration::from_millis(500))
                    .map_err(|e| format!("TCP 连接失败，请先启动手机网络接收：{e}"))?;
                stream.set_nodelay(true).map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(Duration::from_millis(5)))
                    .map_err(|e| e.to_string())?;
                Socket::Tcp(stream)
            }
            Transport::Udp => {
                let bind = if destination.is_ipv4() {
                    "0.0.0.0:0"
                } else {
                    "[::]:0"
                };
                let socket = UdpSocket::bind(bind).map_err(|e| e.to_string())?;
                // Some hosts default to a UDP send buffer smaller than one JPEG.
                // Set only this socket, keeping a bounded two-datagram budget.
                socket2::SockRef::from(&socket)
                    .set_send_buffer_size(128 * 1024)
                    .map_err(|e| format!("无法设置 UDP 发送缓冲：{e}"))?;
                socket.connect(destination).map_err(|e| e.to_string())?;
                socket.set_nonblocking(true).map_err(|e| e.to_string())?;
                Socket::Udp(socket)
            }
            Transport::Ndi => return Err("SocketSender 不支持 NDI".into()),
        };
        Ok(Self {
            socket,
            jpeg: Vec::with_capacity(256 * 1024),
            width: config.region.width as u16,
            height: config.region.height as u16,
            quality: config.jpeg_quality,
            timeout: Duration::from_millis(config.send_timeout_ms as u64),
            dropped: 0,
        })
    }
    /// Only encode changed images. Repeated desktop frames reuse this JPEG.
    /// Output capacity is retained; the encoder can allocate its internal rows.
    pub fn encode(&mut self, bgra: &[u8]) -> Result<(), String> {
        self.jpeg.clear();
        let mut encoder = jpeg_encoder::Encoder::new(&mut self.jpeg, self.quality);
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
        encoder
            .encode(bgra, self.width, self.height, jpeg_encoder::ColorType::Bgra)
            .map_err(|error| format!("JPEG 编码失败：{error}"))?;
        validate_jpeg(
            &self.jpeg,
            match self.socket {
                Socket::Tcp(_) => MAX_TCP_JPEG,
                Socket::Udp(_) => MAX_UDP_JPEG,
            },
        )
    }
    pub fn jpeg_len(&self) -> usize {
        self.jpeg.len()
    }
    /// UDP has no acknowledgement. TCP connection count does not imply decoding.
    pub fn connections(&self) -> i32 {
        match self.socket {
            Socket::Tcp(_) => 1,
            Socket::Udp(_) => -1,
        }
    }
    pub fn submit(&mut self, stop: &AtomicBool) -> Result<bool, String> {
        validate_jpeg(
            &self.jpeg,
            match self.socket {
                Socket::Tcp(_) => MAX_TCP_JPEG,
                Socket::Udp(_) => MAX_UDP_JPEG,
            },
        )?;
        if stop.load(Ordering::Acquire) {
            return Ok(false);
        }
        let deadline = Instant::now() + self.timeout;
        match &mut self.socket {
            Socket::Tcp(stream) => match write_frame(stream, &self.jpeg, deadline, stop) {
                Ok(()) => Ok(true),
                Err(_) if stop.load(Ordering::Acquire) => Ok(false),
                Err(error) => Err(format!(
                    "TCP 发送失败或超时，连接已停止，请检查带宽和手机接收后重新开始：{error}"
                )),
            },
            Socket::Udp(socket) => loop {
                match socket.send(&self.jpeg) {
                    Ok(n) if n == self.jpeg.len() => return Ok(true),
                    Ok(_) => return Err("UDP 数据报未完整提交".into()),
                    Err(e)
                        if e.kind() == io::ErrorKind::Interrupted
                            && Instant::now() < deadline
                            && !stop.load(Ordering::Acquire) =>
                    {
                        continue;
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                        ) =>
                    {
                        self.dropped += 1;
                        return Ok(false);
                    }
                    Err(e) => return Err(format!("UDP 发送失败：{e}")),
                }
            },
        }
    }
}

fn write_frame(
    writer: &mut impl Write,
    pixels: &[u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < pixels.len() {
        if stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = writer.write(&pixels[offset..]);
        match result {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => offset += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::JpegStream;
    use std::{io::Read, net::TcpListener};
    #[test]
    fn short_writes_interrupts_deadlines_and_cancellation() {
        #[derive(Default)]
        struct ShortWriter {
            bytes: Vec<u8>,
            calls: usize,
        }
        impl Write for ShortWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls == 2 {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let n = bytes.len().min(3);
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = ShortWriter::default();
        let stop = AtomicBool::new(false);
        write_frame(
            &mut writer,
            b"jpeg stream",
            Instant::now() + Duration::from_secs(1),
            &stop,
        )
        .unwrap();
        assert_eq!(writer.bytes, b"jpeg stream");
        assert_eq!(
            write_frame(&mut writer, b"a", Instant::now(), &stop)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        stop.store(true, Ordering::Release);
        assert_eq!(
            write_frame(
                &mut writer,
                b"a",
                Instant::now() + Duration::from_secs(1),
                &stop
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        struct Backpressure;
        impl Write for Backpressure {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        stop.store(false, Ordering::Release);
        assert_eq!(
            write_frame(
                &mut Backpressure,
                b"a",
                Instant::now() + Duration::from_millis(5),
                &stop
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }
    fn config(transport: Transport, destination: SocketAddr) -> Config {
        let mut config = Config {
            transport,
            destination: destination.to_string(),
            send_timeout_ms: 1000,
            ..Config::default()
        };
        config.region.width = 32;
        config.region.height = 32;
        config
    }
    fn decode_red(jpeg: &[u8]) {
        let image = image::load_from_memory(jpeg).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (32, 32));
        let p = image.get_pixel(16, 16).0;
        assert!(
            p[0] > 240 && p[1] < 15 && p[2] < 15,
            "BGRA must preserve red: {p:?}"
        );
    }
    #[test]
    fn tcp_apk_marker_parser_decodes_two_frames_and_reused_repeat() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = config(Transport::Tcp, listener.local_addr().unwrap());
        let receiver = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut parser = JpegStream::default();
            let mut bytes = [0; 113];
            let mut count = 0;
            while count < 3 {
                let n = stream.read(&mut bytes).unwrap();
                assert!(n > 0);
                parser
                    .push(&bytes[..n], |jpeg| {
                        decode_red(jpeg);
                        count += 1;
                    })
                    .unwrap();
            }
        });
        let mut sender = SocketSender::connect(&config).unwrap();
        let red = [0, 0, 255, 0].repeat(32 * 32);
        sender.encode(&red).unwrap();
        assert!(sender.submit(&AtomicBool::new(false)).unwrap());
        assert!(sender.submit(&AtomicBool::new(false)).unwrap());
        sender.encode(&red).unwrap();
        assert!(sender.submit(&AtomicBool::new(false)).unwrap());
        receiver.join().unwrap();
    }
    #[test]
    fn udp_contains_one_whole_jpeg_no_header_or_fragments() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut sender =
            SocketSender::connect(&config(Transport::Udp, socket.local_addr().unwrap())).unwrap();
        sender.encode(&[0, 0, 255, 255].repeat(32 * 32)).unwrap();
        assert_eq!(sender.connections(), -1);
        assert!(sender.submit(&AtomicBool::new(false)).unwrap());
        let mut packet = [0; MAX_UDP_JPEG + 1];
        let n = socket.recv(&mut packet).unwrap();
        assert_eq!(&packet[..n], sender.jpeg);
        decode_red(&packet[..n]);
        assert!(!sender.submit(&AtomicBool::new(true)).unwrap());
    }
    #[test]
    fn udp_socket_accepts_the_protocol_maximum_as_one_datagram() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut sender =
            SocketSender::connect(&config(Transport::Udp, socket.local_addr().unwrap())).unwrap();
        // Framing-only fixture exercises OS datagram limits, not JPEG decoding.
        sender.jpeg = vec![1; MAX_UDP_JPEG];
        sender.jpeg[..2].copy_from_slice(&[255, 216]);
        sender.jpeg[MAX_UDP_JPEG - 2..].copy_from_slice(&[255, 217]);
        assert!(sender.submit(&AtomicBool::new(false)).unwrap());
        let mut received = vec![0; MAX_UDP_JPEG + 1];
        let n = socket.recv(&mut received).unwrap();
        assert_eq!(n, MAX_UDP_JPEG);
        assert_eq!(&received[..n], sender.jpeg);
    }
    #[test]
    fn oversized_udp_jpeg_fails_before_send_instead_of_splitting_or_truncating() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut config = config(Transport::Udp, socket.local_addr().unwrap());
        config.region.width = 512;
        config.region.height = 512;
        config.jpeg_quality = 100;
        let mut sender = SocketSender::connect(&config).unwrap();
        let mut value = 1234567u32;
        let pixels: Vec<_> = (0..512 * 512 * 4)
            .map(|_| {
                value ^= value << 13;
                value ^= value >> 17;
                value ^= value << 5;
                value as u8
            })
            .collect();
        assert!(sender.encode(&pixels).unwrap_err().contains("降低 JPEG"));
        assert!(sender.submit(&AtomicBool::new(false)).is_err());
        socket.set_nonblocking(true).unwrap();
        assert_eq!(
            socket.recv(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
