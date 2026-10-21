//! CPU-only pixel conversion benchmark; it never captures or sends a screen.
use crate::{model::PixelFormat, pixels::FrameBuffers};
use serde::Serialize;
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

#[derive(Serialize)]
struct Report {
    scope: &'static str,
    platform: &'static str,
    arch: &'static str,
    build: &'static str,
    seconds_per_case: f64,
    excluded: [&'static str; 4],
    cases: Vec<Case>,
}

#[derive(Serialize)]
struct Case {
    width: u32,
    height: u32,
    input: &'static str,
    output: &'static str,
    buffers: usize,
    frames: u64,
    elapsed_seconds: f64,
    average_conversion_us: f64,
    average_conversion_ms: f64,
    throughput_fps: f64,
}

fn duration(seconds: f64) -> Result<Duration, String> {
    if !seconds.is_finite() || !(0.1..=60.0).contains(&seconds) {
        return Err("像素转换基准每项运行时间需为 0.1 至 60 秒".into());
    }
    Ok(Duration::from_secs_f64(seconds))
}

/// Benchmark the same double-buffered conversion path used by the sender.
/// Results describe CPU conversion/copy throughput, not video latency.
pub fn pixels(seconds: f64) -> Result<(), String> {
    let limit = duration(seconds)?;
    let mut cases = Vec::with_capacity(4);
    for size in [512, 640] {
        for format in [PixelFormat::Nv12, PixelFormat::Bgrx] {
            cases.push(run_case(size, format, limit)?);
        }
    }
    let report = Report {
        scope: "pixel_conversion_only",
        platform: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        build: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        seconds_per_case: seconds,
        excluded: [
            "screen_capture",
            "ndi_encoding",
            "network",
            "end_to_end_latency",
        ],
        cases,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn run_case(size: u32, format: PixelFormat, limit: Duration) -> Result<Case, String> {
    let stride = size as usize * 4;
    let mut input = vec![0; stride * size as usize];
    for y in 0..size as usize {
        for x in 0..size as usize {
            let offset = y * stride + x * 4;
            input[offset..offset + 4].copy_from_slice(&[
                (x * 17 + y * 29) as u8,
                (x * 7 + y * 13) as u8,
                (x * 31 + y * 3) as u8,
                255,
            ]);
        }
    }
    let mut buffers = FrameBuffers::new(size, size, format)?;
    let addresses = [buffers.data(0), buffers.data(1)];
    let mut slot = 0;
    let warmup = Instant::now();
    while warmup.elapsed() < Duration::from_millis(25) {
        buffers.convert(slot, black_box(&input), stride)?;
        black_box(buffers.data(slot));
        slot ^= 1;
    }
    let mut frames = 0u64;
    let mut conversion_time = Duration::ZERO;
    let started = Instant::now();
    while frames == 0 || started.elapsed() < limit {
        let conversion_started = Instant::now();
        buffers.convert(slot, black_box(&input), stride)?;
        conversion_time += conversion_started.elapsed();
        black_box(buffers.data(slot));
        frames += 1;
        slot ^= 1;
    }
    let elapsed = started.elapsed().as_secs_f64();
    if addresses != [buffers.data(0), buffers.data(1)] {
        return Err("基准运行期间帧缓冲地址发生变化".into());
    }
    let average_seconds = conversion_time.as_secs_f64() / frames as f64;
    Ok(Case {
        width: size,
        height: size,
        input: "BGRA8",
        output: match format {
            PixelFormat::Nv12 => "NV12",
            PixelFormat::Bgrx => "BGRX",
        },
        buffers: 2,
        frames,
        elapsed_seconds: elapsed,
        average_conversion_us: average_seconds * 1_000_000.0,
        average_conversion_ms: average_seconds * 1_000.0,
        throughput_fps: frames as f64 / elapsed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_duration_rejects_unbounded_or_invalid_runs() {
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            0.099,
            60.001,
        ] {
            assert!(duration(invalid).is_err());
        }
        assert_eq!(duration(0.1).unwrap(), Duration::from_millis(100));
        assert_eq!(duration(60.0).unwrap(), Duration::from_secs(60));
    }
}
