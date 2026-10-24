//! Stable NDI storage shared by the platform capture workers.
use crate::{
    model::{Config, PixelFormat},
    ndi::{BGRX, NV12, Sender, VideoFrame},
    pixels::FrameBuffers,
};

/// Field drop order is part of the ownership contract: the sender flushes and
/// is destroyed before either pixel storage or descriptors can be released.
pub(super) struct Stream<'a> {
    pub(super) sender: Sender<'a>,
    pub(super) buffers: FrameBuffers,
    descriptors: Box<[VideoFrame; 2]>,
    descriptor: usize,
}
impl<'a> Stream<'a> {
    pub(super) fn new(sender: Sender<'a>, buffers: FrameBuffers, config: &Config) -> Self {
        let frame = VideoFrame {
            width: config.region.width as i32,
            height: config.region.height as i32,
            fourcc: if config.pixel == PixelFormat::Nv12 {
                NV12
            } else {
                BGRX
            },
            rate_n: config.fps as i32,
            rate_d: 1,
            aspect: config.region.width as f32 / config.region.height as f32,
            format: 1,
            timecode: i64::MAX,
            data: std::ptr::null(),
            stride: config.region.width as i32
                * if config.pixel == PixelFormat::Nv12 {
                    1
                } else {
                    4
                },
            metadata: std::ptr::null(),
            timestamp: 0,
        };
        Self {
            sender,
            buffers,
            descriptors: Box::new([frame; 2]),
            descriptor: 0,
        }
    }
    pub(super) fn submit(&mut self, slot: usize, rate: u32) {
        self.descriptor ^= 1;
        let frame = &mut self.descriptors[self.descriptor];
        frame.data = self.buffers.data(slot);
        frame.rate_n = rate as i32;
        frame.timecode = timecode();
        // The descriptor alternates even for repeated pixels, so its
        // timecode is never changed while NDI can still be reading it.
        unsafe { self.sender.submit(frame) };
    }
}

#[derive(Default)]
pub(super) struct Measurements {
    pub(super) capture: f64,
    pub(super) conversion: f64,
    pub(super) submit: f64,
    pub(super) wait: f64,
    pub(super) count: u64,
}

fn timecode() -> i64 {
    #[cfg(windows)]
    {
        let time =
            unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
        let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        ticks.saturating_sub(116_444_736_000_000_000) as i64
    }
    #[cfg(target_os = "macos")]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| (elapsed.as_nanos() / 100).min(i64::MAX as u128) as i64)
            .unwrap_or(0)
    }
}
