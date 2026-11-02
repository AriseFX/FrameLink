//! JPEG framing used by the supplied 霜雪 7.18 APK (static analysis).
//! TCP: concatenated SOI..EOI images, no length or custom header.
//! UDP: exactly one entire JPEG per datagram, no application fragmentation.
pub const MAX_TCP_JPEG: usize = 2 * 1024 * 1024;
/// Conservative IPv4 UDP payload limit; also safe for IPv6 without extensions.
pub const MAX_UDP_JPEG: usize = 65_507;

pub fn validate_jpeg(jpeg: &[u8], max: usize) -> Result<(), String> {
    if jpeg.len() > max {
        return Err(format!(
            "JPEG 为 {} 字节，超过接收协议上限 {}；请降低 JPEG 质量或采集尺寸，UDP 可改用 TCP",
            jpeg.len(),
            max
        ));
    }
    if jpeg.len() < 4 || !jpeg.starts_with(&[255, 216]) || !jpeg.ends_with(&[255, 217]) {
        return Err("无效的完整 JPEG 图片".into());
    }
    Ok(())
}

/// Bounded reference receiver matching the APK's marker-based framing. Generated
/// JPEGs contain no custom metadata; arbitrary metadata can contain marker bytes.
#[derive(Default)]
pub struct JpegStream {
    bytes: Vec<u8>,
    scan: usize,
}
impl JpegStream {
    pub fn push(&mut self, bytes: &[u8], mut frame: impl FnMut(&[u8])) -> Result<(), String> {
        // Process even a read containing multiple maximum-sized frames without
        // allocating an unbounded input buffer.
        for &byte in bytes {
            if self.bytes.len() == MAX_TCP_JPEG {
                self.bytes.clear();
                self.scan = 0;
                return Err("JPEG 超过 TCP 接收上限".into());
            }
            self.bytes.push(byte);
            if self.bytes.len() == 1 {
                continue;
            }
            if self.scan == 0 {
                if self.bytes[..2] == [255, 216] {
                    self.scan = 2;
                } else {
                    self.bytes[0] = byte;
                    self.bytes.truncate(1);
                }
            } else if self.bytes.ends_with(&[255, 217]) {
                frame(&self.bytes);
                self.bytes.clear();
                self.scan = 0;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concatenated_jpegs_survive_arbitrary_tcp_boundaries_and_noise() {
        let a = [255, 216, 1, 2, 255, 0, 3, 255, 217];
        let b = [255, 216, 6, 7, 255, 217];
        let stream = [vec![1, 2, 3, 255], a.to_vec(), b.to_vec()].concat();
        for chunk_size in 1..=stream.len() {
            let mut receiver = JpegStream::default();
            let mut frames = Vec::new();
            for chunk in stream.chunks(chunk_size) {
                receiver
                    .push(chunk, |bytes| frames.push(bytes.to_vec()))
                    .unwrap();
            }
            assert_eq!(frames, [a.to_vec(), b.to_vec()]);
        }
    }
    #[test]
    fn limits_and_incomplete_images_are_rejected() {
        assert!(validate_jpeg(&[255, 216, 255, 217], 4).is_ok());
        assert!(validate_jpeg(&[255, 216, 255, 217], 3).is_err());
        assert!(validate_jpeg(&[255, 216, 1], 10).is_err());
        let mut receiver = JpegStream::default();
        let mut oversized = vec![1; MAX_TCP_JPEG + 1];
        oversized[..2].copy_from_slice(&[255, 216]);
        assert!(
            receiver
                .push(&oversized, |_| panic!("incomplete frame delivered"))
                .is_err()
        );
    }
}
