//! Stable NDI storage shared by the platform capture workers.
use crate::{
    model::{Config, PixelFormat},
    ndi::{BGRX, NV12, Sender, VideoFrame},
    pixels::FrameBuffers,
};

/// Field drop order is part of the ownership contract: the sender flushes and
/// is destroyed before either pixel storage or descriptors can be released.
pub(super) struct Stream<'a> {
    sender: Output<'a>,
    pub(super) buffers: FrameBuffers,
    descriptors: Box<[VideoFrame; 2]>,
    descriptor: usize,
}
pub(super) enum Output<'a> {
    Ndi(Sender<'a>),
    Jpeg(Box<crate::transport::SocketSender>),
}
impl<'a> Stream<'a> {
    pub(super) fn new(sender: Output<'a>, buffers: FrameBuffers, config: &Config) -> Self {
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
    pub(super) fn encode(&mut self, slot: usize) -> Result<(), String> {
        if let Output::Jpeg(sender) = &mut self.sender {
            sender.encode(self.buffers.bytes(slot))?;
        }
        Ok(())
    }
    pub(super) fn connections(&self) -> i32 {
        match &self.sender {
            Output::Ndi(sender) => sender.connections(),
            Output::Jpeg(sender) => sender.connections(),
        }
    }
    pub(super) fn dropped(&self) -> u64 {
        match &self.sender {
            Output::Ndi(_) => 0,
            Output::Jpeg(sender) => sender.dropped,
        }
    }
    pub(super) fn last_timecode(&self) -> Option<i64> {
        match &self.sender {
            Output::Ndi(_) => Some(self.descriptors[self.descriptor].timecode),
            Output::Jpeg(_) => None,
        }
    }
    pub(super) fn submit(
        &mut self,
        slot: usize,
        rate: u32,
        stop: &std::sync::atomic::AtomicBool,
    ) -> Result<bool, String> {
        let sender = match &mut self.sender {
            Output::Jpeg(sender) => return sender.submit(stop),
            Output::Ndi(sender) => sender,
        };
        self.descriptor ^= 1;
        let frame = &mut self.descriptors[self.descriptor];
        frame.data = self.buffers.data(slot);
        frame.rate_n = rate as i32;
        frame.timecode = crate::diagnostics::utc_timecode_100ns();
        // The descriptor alternates even for repeated pixels, so its
        // timecode is never changed while NDI can still be reading it.
        unsafe { sender.submit(frame) };
        Ok(true)
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
