use crate::model::{PixelFormat, Preview};

#[cfg(windows)]
unsafe extern "C" {
    fn ARGBToNV12Matrix(
        src: *const u8,
        src_stride: i32,
        y: *mut u8,
        y_stride: i32,
        uv: *mut u8,
        uv_stride: i32,
        matrix: *const std::ffi::c_void,
        width: i32,
        height: i32,
    ) -> i32;
    // Opaque C data: only its address is passed back to libyuv; Rust never
    // dereferences it or assumes the architecture-dependent SIMD struct layout.
    static kArgbI601Constants: u8;
    static kArgbH709Constants: u8;
    static kArgbU2020Constants: u8;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
    Bt2020,
}
/// Match the original sender and NDI's resolution-dependent YUV colorimetry.
pub fn color_matrix(width: usize, height: usize) -> ColorMatrix {
    if width > 1920 || height > 1080 {
        ColorMatrix::Bt2020
    } else if width > 720 || height > 576 {
        ColorMatrix::Bt709
    } else {
        ColorMatrix::Bt601
    }
}

pub fn frame_len(width: u32, height: u32, format: PixelFormat) -> Result<usize, String> {
    if width < 2
        || height < 2
        || !width.is_multiple_of(2)
        || u64::from(width) * u64::from(height) > 33_177_600
    {
        return Err("无效的帧缓冲尺寸".into());
    }
    let pixels = width as usize * height as usize;
    match format {
        PixelFormat::Bgrx => Ok(pixels * 4),
        PixelFormat::Nv12 if height.is_multiple_of(2) => Ok(pixels * 3 / 2),
        PixelFormat::Nv12 => Err("NV12的高度需为偶数".into()),
    }
}

fn validate_bgra(src: &[u8], stride: usize, width: usize, height: usize) -> Result<(), String> {
    let row = width.checked_mul(4).ok_or("帧尺寸溢出")?;
    let needed = height
        .checked_sub(1)
        .and_then(|h| h.checked_mul(stride))
        .and_then(|n| n.checked_add(row))
        .ok_or("帧尺寸溢出")?;
    if width == 0 || height == 0 || stride < row || src.len() < needed || stride > i32::MAX as usize
    {
        return Err("采集缓冲区长度或行距无效".into());
    }
    Ok(())
}

/// Exactly two stable allocations. Never resize them after NDI receives a pointer.
pub struct FrameBuffers {
    width: usize,
    height: usize,
    format: PixelFormat,
    pixels: [Vec<u8>; 2],
}
impl FrameBuffers {
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Result<Self, String> {
        let len = frame_len(width, height, format)?;
        let alloc = || {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(len)
                .map_err(|_| "无法分配帧缓冲内存".to_string())?;
            bytes.resize(len, 0);
            Ok::<_, String>(bytes)
        };
        Ok(Self {
            width: width as usize,
            height: height as usize,
            format,
            pixels: [alloc()?, alloc()?],
        })
    }
    pub fn bytes(&self, slot: usize) -> &[u8] {
        &self.pixels[slot]
    }
    pub fn data(&self, slot: usize) -> *const u8 {
        self.pixels[slot].as_ptr()
    }
    /// Copy GPU readback into the inactive, stable NDI buffer; strip row padding.
    pub fn copy_nv12(&mut self, slot: usize, src: &[u8], stride: usize) -> Result<(), String> {
        let rows = self.height + self.height / 2;
        let needed = (rows - 1)
            .checked_mul(stride)
            .and_then(|n| n.checked_add(self.width));
        if self.format != PixelFormat::Nv12
            || stride < self.width
            || needed.is_none_or(|needed| src.len() < needed)
        {
            return Err("NV12读取缓冲区长度或行距无效".into());
        }
        for (y, row) in self.pixels[slot].chunks_exact_mut(self.width).enumerate() {
            row.copy_from_slice(&src[y * stride..y * stride + self.width]);
        }
        Ok(())
    }
    pub fn convert(&mut self, slot: usize, src: &[u8], stride: usize) -> Result<(), String> {
        validate_bgra(src, stride, self.width, self.height)?;
        let dst = &mut self.pixels[slot];
        match self.format {
            PixelFormat::Bgrx => {
                let bytes = self.width * 4;
                for (row, output) in dst.chunks_exact_mut(bytes).enumerate() {
                    output.copy_from_slice(&src[row * stride..row * stride + bytes]);
                }
                Ok(())
            }
            PixelFormat::Nv12 => {
                #[cfg(windows)]
                {
                    // DXGI and GDI expose BGRA byte order. libyuv's ARGB name
                    // describes little-endian words, so ARGBToNV12 is correct.
                    let matrix = match color_matrix(self.width, self.height) {
                        ColorMatrix::Bt601 => std::ptr::addr_of!(kArgbI601Constants),
                        ColorMatrix::Bt709 => std::ptr::addr_of!(kArgbH709Constants),
                        ColorMatrix::Bt2020 => std::ptr::addr_of!(kArgbU2020Constants),
                    }
                    .cast();
                    let result = unsafe {
                        ARGBToNV12Matrix(
                            src.as_ptr(),
                            stride as i32,
                            dst.as_mut_ptr(),
                            self.width as i32,
                            dst.as_mut_ptr().add(self.width * self.height),
                            self.width as i32,
                            matrix,
                            self.width as i32,
                            self.height as i32,
                        )
                    };
                    if result == 0 {
                        Ok(())
                    } else {
                        Err("libyuv NV12转换失败".into())
                    }
                }
                #[cfg(not(windows))]
                {
                    Err("当前平台不支持NV12转换".into())
                }
            }
        }
    }
}

/// A small, independent UI image; never handed to NDI or queued by the worker.
pub fn preview_bgra(
    src: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    sequence: u64,
) -> Result<Preview, String> {
    validate_bgra(src, stride, width, height)?;
    let scale = (320.0 / width as f64).min(320.0 / height as f64).min(1.0);
    let w = ((width as f64 * scale) as usize).max(1);
    let h = ((height as f64 * scale) as usize).max(1);
    let mut rgba = vec![0; w * h * 4];
    for y in 0..h {
        let source_y = y * height / h;
        for x in 0..w {
            let input = source_y * stride + (x * width / w) * 4;
            let output = (y * w + x) * 4;
            rgba[output..output + 4].copy_from_slice(&[
                src[input + 2],
                src[input + 1],
                src[input],
                255,
            ]);
        }
    }
    Ok(Preview {
        width: w,
        height: h,
        rgba,
        sequence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn buffers_have_exact_ndi_payload_sizes() {
        assert_eq!(frame_len(512, 512, PixelFormat::Nv12).unwrap(), 393_216);
        assert_eq!(frame_len(640, 640, PixelFormat::Nv12).unwrap(), 614_400);
        assert_eq!(frame_len(512, 512, PixelFormat::Bgrx).unwrap(), 1_048_576);
        assert!(frame_len(513, 512, PixelFormat::Nv12).is_err());
        assert!(frame_len(512, 511, PixelFormat::Nv12).is_err());
        assert!(frame_len(32768, 32768, PixelFormat::Bgrx).is_err());
    }
    #[test]
    fn nv12_readback_strips_padding_without_reallocating_or_touching_inflight_slot() {
        let mut buffers = FrameBuffers::new(2, 2, PixelFormat::Nv12).unwrap();
        let pointers = [buffers.data(0), buffers.data(1)];
        let src = [16, 235, 99, 99, 81, 145, 99, 99, 128, 128];
        buffers.copy_nv12(1, &src, 4).unwrap();
        assert_eq!(buffers.bytes(1), [16, 235, 81, 145, 128, 128]);
        assert_eq!(buffers.bytes(0), [0; 6]);
        assert_eq!(pointers, [buffers.data(0), buffers.data(1)]);
        assert!(buffers.copy_nv12(0, &src[..9], 4).is_err());
        assert!(buffers.copy_nv12(0, &src, 1).is_err());
        assert!(buffers.copy_nv12(0, &src, usize::MAX).is_err());
        assert_eq!(buffers.bytes(0), [0; 6]);
        assert!(
            FrameBuffers::new(2, 2, PixelFormat::Bgrx)
                .unwrap()
                .copy_nv12(0, &src, 4)
                .is_err()
        );
    }
    #[test]
    fn square_capture_preserves_the_original_color_matrix() {
        assert_eq!(color_matrix(512, 512), ColorMatrix::Bt601);
        assert_eq!(color_matrix(640, 640), ColorMatrix::Bt709);
        assert_eq!(color_matrix(1920, 1080), ColorMatrix::Bt709);
        assert_eq!(color_matrix(1922, 1080), ColorMatrix::Bt2020);
    }
    #[test]
    fn padded_capture_rows_are_copied_without_padding() {
        let mut buffers = FrameBuffers::new(2, 2, PixelFormat::Bgrx).unwrap();
        let input = [
            1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 99, 99, 9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let pointers = [buffers.data(0), buffers.data(1)];
        buffers.convert(0, &input, 12).unwrap();
        assert_eq!(
            buffers.pixels[0],
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(pointers, [buffers.data(0), buffers.data(1)]);
        assert!(buffers.convert(1, &input[..10], 12).is_err());
    }

    #[test]
    #[cfg(windows)]
    fn nv12_black_white_and_padded_rows_match_with_stable_buffers() {
        // The production crop sizes exercise SIMD rows and both NDI matrices.
        // Neutral black/white have the same limited-range reference values in
        // BT.601 and BT.709: Y=16/235, U=128, V=128.
        for size in [512u32, 640] {
            let width = size as usize;
            let row = width * 4;
            let stride = row + 28;
            let mut tight = vec![0u8; row * width];
            let mut padded = vec![0xa5; stride * width];
            for y in 0..width {
                for x in 0..width {
                    let value = if x < width / 2 { 0 } else { 255 };
                    let pixel = [value, value, value, 255];
                    tight[y * row + x * 4..y * row + x * 4 + 4].copy_from_slice(&pixel);
                    padded[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&pixel);
                }
            }
            let mut buffers = FrameBuffers::new(size, size, PixelFormat::Nv12).unwrap();
            let addresses = [buffers.data(0), buffers.data(1)];
            buffers.convert(0, &tight, row).unwrap();
            buffers.convert(1, &padded, stride).unwrap();
            assert_eq!(buffers.pixels[0], buffers.pixels[1]);
            let (luma, chroma) = buffers.pixels[0].split_at(width * width);
            for line in luma.chunks_exact(width) {
                assert!(line[..width / 2].iter().all(|&value| value == 16));
                assert!(line[width / 2..].iter().all(|&value| value == 235));
            }
            assert!(chroma.iter().all(|&value| value == 128));

            // Converting one slot must leave the previous async-send slot intact.
            let previous = buffers.pixels[0].clone();
            padded.fill(0);
            buffers.convert(1, &padded, stride).unwrap();
            assert_eq!(buffers.pixels[0], previous);
            assert_eq!(addresses, [buffers.data(0), buffers.data(1)]);
        }
    }
    #[test]
    fn preview_uses_rgb_channels_and_ignores_uninitialized_alpha() {
        let p = preview_bgra(
            &[1, 2, 3, 0, 5, 6, 7, 0, 9, 10, 11, 0, 13, 14, 15, 0],
            8,
            2,
            2,
            5,
        )
        .unwrap();
        assert_eq!((p.width, p.height, p.sequence), (2, 2, 5));
        assert_eq!(p.rgba[..8], [3, 2, 1, 255, 7, 6, 5, 255]);
        assert!(preview_bgra(&[0; 4], 4, 2, 2, 0).is_err());
    }
}
