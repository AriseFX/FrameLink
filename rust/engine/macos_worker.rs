use super::{
    Shared,
    send_pipeline::{Measurements, Stream},
};
use crate::{
    capture_macos::{Capture, CaptureError},
    model::{Config, Phase},
    ndi::Ndi,
    pixels::{FrameBuffers, preview_bgra},
    policy::LatestFrameSlots,
};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn capture_message(error: CaptureError) -> String {
    match error {
        CaptureError::Recoverable(message)
        | CaptureError::Unsupported(message)
        | CaptureError::Fatal(message) => message,
    }
}

pub(super) fn run(shared: &Shared, config: &Config) -> Result<(), String> {
    let ndi = Ndi::load()?;
    if shared.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let buffers = FrameBuffers::new(config.region.width, config.region.height, config.pixel)?;
    let sender = ndi.create_sender(&config.name)?;
    shared.stats(|stats| {
        stats.source = sender.source_name();
        stats.ndi_version = ndi.version();
        stats.backend = "ScreenCaptureKit".into();
    });
    let mut stream = Stream::new(sender, buffers, config);
    let mut capture = match Capture::new_cancellable(config, shared.stop.clone()) {
        Ok(capture) => capture,
        Err(_) if shared.stop.load(Ordering::Acquire) => return Ok(()),
        Err(error) => return Err(capture_message(error)),
    };
    shared.stats(|stats| stats.phase = Phase::Sending);
    let started = Instant::now();
    let mut last_stats = started;
    let mut last_preview = started
        .checked_sub(Duration::from_secs(1))
        .unwrap_or(started);
    let mut slots = LatestFrameSlots::default();
    let mut measurements = Measurements::default();
    let mut frames = 0u64;
    let mut window_frames = 0u64;
    while !shared.stop.load(Ordering::Acquire) {
        let polled_at = Instant::now();
        let frame = capture
            .next_frame(Duration::from_millis(20))
            .map_err(capture_message)?;
        // Sample callbacks wait for the compositor, which is idle time rather
        // than processing work; readback still counts as capture cost.
        let polled = polled_at.elapsed();
        if let Some(frame) = frame {
            if shared.stop.load(Ordering::Acquire) {
                break;
            }
            if frame.width != config.region.width || frame.height != config.region.height {
                return Err("采集画面尺寸已变化，请停止后重新选择区域".into());
            }
            let captured = Instant::now();
            let plan = slots.prepare(true);
            stream
                .buffers
                .convert(plan.slot, &frame.bytes, frame.stride)?;
            let converted = Instant::now();
            stream.submit(plan.slot, config.fps);
            let submitted = Instant::now();
            slots.submitted(plan.slot);
            frames += 1;
            window_frames += 1;
            measurements.capture += frame.readback_duration.as_secs_f64() * 1000.0;
            measurements.conversion += (converted - captured).as_secs_f64() * 1000.0;
            measurements.submit += (submitted - converted).as_secs_f64() * 1000.0;
            measurements.wait +=
                polled.saturating_sub(frame.readback_duration).as_secs_f64() * 1000.0;
            measurements.count += 1;
            if shared.preview_enabled.load(Ordering::Relaxed)
                && submitted.duration_since(last_preview) >= Duration::from_millis(500)
            {
                let preview = preview_bgra(
                    &frame.bytes,
                    frame.stride,
                    frame.width as usize,
                    frame.height as usize,
                    frames,
                )?;
                *shared
                    .preview
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(preview);
                last_preview = Instant::now();
            }
            capture.recycle(frame);
        }
        let now = Instant::now();
        if now.duration_since(last_stats) >= Duration::from_millis(500) {
            let elapsed = now.duration_since(last_stats).as_secs_f64();
            let count = measurements.count.max(1) as f64;
            let receivers = stream.sender.connections();
            shared.stats(|stats| {
                stats.fps = window_frames as f64 / elapsed;
                stats.updates_fps = stats.fps;
                stats.receivers = receivers;
                stats.frames = frames;
                stats.repeats = 0;
                stats.capture_ms = measurements.capture / count;
                stats.conversion_ms = measurements.conversion / count;
                stats.submit_ms = measurements.submit / count;
                stats.wait_ms = measurements.wait / count;
                stats.uptime_sec = now.duration_since(started).as_secs_f64();
            });
            last_stats = now;
            window_frames = 0;
            measurements = Measurements::default();
        }
    }
    shared.stats(|stats| {
        stats.phase = Phase::Stopping;
        stats.frames = frames;
        stats.uptime_sec = started.elapsed().as_secs_f64();
    });
    drop(capture);
    drop(stream);
    Ok(())
}
