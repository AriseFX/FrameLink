//! Optional sender-side traces. These timings never measure receiver latency.
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct FrameTiming {
    pub frame: u64,
    pub refreshed: bool,
    pub desktop_updated: bool,
    pub release_ms: f64,
    pub acquire_ms: f64,
    pub map_wait_ms: f64,
    pub conversion_ms: f64,
    pub ndi_submit_ms: f64,
    pub desktop_age_before_submit_ms: Option<f64>,
    pub desktop_age_after_submit_ms: Option<f64>,
    pub accumulated_desktop_frames: u32,
}

#[cfg(any(windows, test))]
mod statistics {
    use super::FrameTiming;
    use crate::model::Phase;
    use serde::Serialize;
    use std::{collections::BTreeMap, time::Duration};

    pub(super) fn duration(seconds: f64) -> Result<Duration, String> {
        if !seconds.is_finite() || !(1.0..=60.0).contains(&seconds) {
            return Err("发送诊断运行时间需为 1 至 60 秒".into());
        }
        Ok(Duration::from_secs_f64(seconds))
    }

    // Auto capture can remain Recovering while its GDI fallback is sending.
    // A positive frame count distinguishes that active fallback from startup.
    pub(super) fn sender_active(phase: Phase, frames: u64) -> bool {
        phase == Phase::Sending || (phase == Phase::Recovering && frames > 0)
    }

    #[derive(Default)]
    pub(super) struct TraceCollection {
        samples: BTreeMap<u64, FrameTiming>,
        duplicate_sample_observations: u64,
        ignored_zero_frame_samples: u64,
    }

    #[derive(Debug, Serialize, PartialEq, Eq)]
    pub(super) struct FrameGap {
        first: u64,
        last: u64,
    }

    #[derive(Serialize)]
    pub(super) struct CollectedTrace {
        pub samples: Vec<FrameTiming>,
        pub duplicate_sample_observations: u64,
        pub ignored_zero_frame_samples: u64,
        pub out_of_range_samples: u64,
        pub missing_diagnostic_samples: u64,
        pub missing_frame_ranges: Vec<FrameGap>,
    }

    impl TraceCollection {
        pub(super) fn extend(&mut self, samples: &[FrameTiming]) {
            for sample in samples {
                if sample.frame == 0 {
                    self.ignored_zero_frame_samples += 1;
                    continue;
                }
                match self.samples.entry(sample.frame) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(sample.clone());
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {
                        self.duplicate_sample_observations += 1;
                    }
                }
            }
        }

        pub(super) fn finish(self, frames: u64) -> CollectedTrace {
            let mut samples = Vec::with_capacity(self.samples.len());
            let mut missing_frame_ranges = Vec::new();
            let mut next = Some(1u64);
            let mut out_of_range_samples = 0;
            for (frame, sample) in self.samples {
                if frame > frames {
                    out_of_range_samples += 1;
                    continue;
                }
                if let Some(first) = next.filter(|&first| first < frame) {
                    missing_frame_ranges.push(FrameGap {
                        first,
                        last: frame - 1,
                    });
                }
                next = frame.checked_add(1);
                samples.push(sample);
            }
            if let Some(first) = next.filter(|&first| first <= frames) {
                missing_frame_ranges.push(FrameGap {
                    first,
                    last: frames,
                });
            }
            CollectedTrace {
                missing_diagnostic_samples: frames.saturating_sub(samples.len() as u64),
                samples,
                duplicate_sample_observations: self.duplicate_sample_observations,
                ignored_zero_frame_samples: self.ignored_zero_frame_samples,
                out_of_range_samples,
                missing_frame_ranges,
            }
        }
    }

    #[derive(Debug, Serialize)]
    pub(super) struct Distribution {
        pub samples: usize,
        pub invalid_samples: usize,
        pub p50_ms: Option<f64>,
        pub p95_ms: Option<f64>,
        pub p99_ms: Option<f64>,
        pub max_ms: Option<f64>,
    }

    fn distribution(values: impl Iterator<Item = f64>) -> Distribution {
        let mut invalid_samples = 0;
        let mut sorted: Vec<_> = values
            .filter(|value| {
                let valid = value.is_finite() && *value >= 0.0;
                if !valid {
                    invalid_samples += 1;
                }
                valid
            })
            .collect();
        sorted.sort_unstable_by(f64::total_cmp);
        // Nearest-rank percentiles: ceil(n * percentile / 100), one-based.
        let percentile = |percent: usize| {
            (!sorted.is_empty()).then(|| {
                let rank = (sorted.len() * percent).div_ceil(100);
                sorted[rank - 1]
            })
        };
        Distribution {
            samples: sorted.len(),
            invalid_samples,
            p50_ms: percentile(50),
            p95_ms: percentile(95),
            p99_ms: percentile(99),
            max_ms: sorted.last().copied(),
        }
    }

    #[derive(Serialize)]
    pub(super) struct SegmentDistributions {
        release_ms: Distribution,
        acquire_ms: Distribution,
        map_wait_ms: Distribution,
        conversion_ms: Distribution,
        ndi_submit_ms: Distribution,
        desktop_age_before_submit_ms: Distribution,
        desktop_age_after_submit_ms: Distribution,
    }

    pub(super) fn summarize(samples: &[FrameTiming]) -> SegmentDistributions {
        SegmentDistributions {
            release_ms: distribution(samples.iter().map(|sample| sample.release_ms)),
            acquire_ms: distribution(samples.iter().map(|sample| sample.acquire_ms)),
            map_wait_ms: distribution(samples.iter().map(|sample| sample.map_wait_ms)),
            conversion_ms: distribution(samples.iter().map(|sample| sample.conversion_ms)),
            ndi_submit_ms: distribution(samples.iter().map(|sample| sample.ndi_submit_ms)),
            desktop_age_before_submit_ms: distribution(
                samples
                    .iter()
                    .filter(|sample| sample.desktop_updated)
                    .filter_map(|sample| sample.desktop_age_before_submit_ms),
            ),
            desktop_age_after_submit_ms: distribution(
                samples
                    .iter()
                    .filter(|sample| sample.desktop_updated)
                    .filter_map(|sample| sample.desktop_age_after_submit_ms),
            ),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sample(frame: u64) -> FrameTiming {
            FrameTiming {
                frame,
                refreshed: true,
                desktop_updated: true,
                release_ms: 0.5,
                acquire_ms: 1.0,
                map_wait_ms: 2.0,
                conversion_ms: 3.0,
                ndi_submit_ms: 4.0,
                desktop_age_before_submit_ms: Some(5.0),
                desktop_age_after_submit_ms: Some(6.0),
                accumulated_desktop_frames: 1,
            }
        }

        #[test]
        fn measurement_duration_rejects_invalid_and_unbounded_runs() {
            for value in [f64::NAN, f64::INFINITY, -1.0, 0.0, 0.999, 60.001] {
                assert!(duration(value).is_err());
            }
            assert_eq!(duration(1.0).unwrap(), Duration::from_secs(1));
            assert_eq!(duration(60.0).unwrap(), Duration::from_secs(60));
        }

        #[test]
        fn active_sender_accepts_sending_and_recovering_with_sent_frames() {
            assert!(sender_active(Phase::Sending, 0));
            assert!(sender_active(Phase::Recovering, 1));
            assert!(!sender_active(Phase::Recovering, 0));
            assert!(!sender_active(Phase::Error, 1));
        }

        #[test]
        fn percentiles_use_finite_nonnegative_nearest_rank_values() {
            let result = distribution(
                [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0]
                    .into_iter()
                    .chain((1..=100).map(f64::from)),
            );
            assert_eq!(result.samples, 100);
            assert_eq!(result.invalid_samples, 4);
            assert_eq!(result.p50_ms, Some(50.0));
            assert_eq!(result.p95_ms, Some(95.0));
            assert_eq!(result.p99_ms, Some(99.0));
            assert_eq!(result.max_ms, Some(100.0));
            let empty = distribution([f64::NAN].into_iter());
            assert_eq!(empty.samples, 0);
            assert_eq!(empty.p50_ms, None);
            assert_eq!(empty.max_ms, None);
        }

        #[test]
        fn repeated_batches_do_not_hide_missing_frames_or_add_duplicate_samples() {
            let mut collected = TraceCollection::default();
            collected.extend(&[sample(0), sample(1), sample(3)]);
            collected.extend(&[sample(1), sample(3), sample(7)]);
            let result = collected.finish(5);
            assert_eq!(result.samples.len(), 2);
            assert_eq!(result.duplicate_sample_observations, 2);
            assert_eq!(result.ignored_zero_frame_samples, 1);
            assert_eq!(result.out_of_range_samples, 1);
            assert_eq!(result.missing_diagnostic_samples, 3);
            assert_eq!(
                result.missing_frame_ranges,
                [
                    FrameGap { first: 2, last: 2 },
                    FrameGap { first: 4, last: 5 }
                ]
            );
            assert_eq!(
                TraceCollection::default()
                    .finish(0)
                    .missing_diagnostic_samples,
                0
            );
        }

        #[test]
        fn desktop_age_summaries_require_a_desktop_update() {
            let first = sample(1);
            let mut repeated = sample(2);
            repeated.refreshed = false;
            repeated.desktop_updated = false;
            repeated.desktop_age_before_submit_ms = Some(100.0);
            repeated.desktop_age_after_submit_ms = Some(200.0);
            let mut cursor_only = sample(3);
            cursor_only.desktop_updated = false;
            cursor_only.desktop_age_before_submit_ms = Some(300.0);
            let mut unknown = sample(4);
            unknown.desktop_age_before_submit_ms = None;
            unknown.desktop_age_after_submit_ms = None;
            let summary = summarize(&[first, repeated, cursor_only, unknown]);
            assert_eq!(summary.acquire_ms.samples, 4);
            assert_eq!(summary.desktop_age_before_submit_ms.samples, 1);
            assert_eq!(summary.desktop_age_before_submit_ms.p99_ms, Some(5.0));
            assert_eq!(summary.desktop_age_after_submit_ms.p99_ms, Some(6.0));
        }
    }
}

#[cfg(windows)]
mod windows_sender {
    use super::statistics::{
        CollectedTrace, SegmentDistributions, TraceCollection, duration, sender_active, summarize,
    };
    use crate::{
        capture::monitors,
        engine::Engine,
        model::{Config, Phase, Stats},
        settings,
    };
    use serde::Serialize;
    use std::{
        fs,
        path::Path,
        thread,
        time::{Duration, Instant},
    };

    #[derive(Serialize)]
    struct ReceiverObservation {
        sender_uptime_seconds: f64,
        receivers: i32,
    }

    #[derive(Serialize)]
    struct Report {
        scope: &'static str,
        excluded: [&'static str; 5],
        notes: Vec<&'static str>,
        measurement_succeeded: bool,
        final_phase: Phase,
        error: Option<String>,
        build: &'static str,
        version: &'static str,
        settings_warning: Option<String>,
        monitor_warning: Option<String>,
        config: Config,
        diagnostics_enabled: bool,
        requested_seconds: f64,
        actual_measurement_seconds: f64,
        startup_seconds: f64,
        shutdown_seconds: f64,
        sender_uptime_seconds: f64,
        source: String,
        backend: String,
        ndi_version: String,
        frames: u64,
        repeats: u64,
        receiver_observed: bool,
        maximum_observed_receivers: i32,
        receiver_observations: Vec<ReceiverObservation>,
        segment_distributions: SegmentDistributions,
        #[serde(flatten)]
        trace: CollectedTrace,
    }

    fn collect(
        stats: &Stats,
        traces: &mut TraceCollection,
        receivers: &mut Vec<ReceiverObservation>,
    ) {
        traces.extend(&stats.timings);
        if sender_active(stats.phase, stats.frames) {
            receivers.push(ReceiverObservation {
                sender_uptime_seconds: stats.uptime_sec,
                receivers: stats.receivers,
            });
        }
    }

    pub(super) fn measure_send(seconds: f64, output: &Path) -> Result<(), String> {
        let limit = duration(seconds)?;
        let (mut config, settings_warning) = settings::load();
        let displays = monitors()?;
        let requested_monitor = config.monitor.clone();
        let monitor = displays
            .iter()
            .find(|monitor| monitor.id == requested_monitor)
            .or_else(|| displays.iter().find(|monitor| monitor.primary))
            .or_else(|| displays.first())
            .ok_or("未找到可用于发送诊断的显示器")?;
        let monitor_warning = if !requested_monitor.is_empty() && monitor.id != requested_monitor {
            Some(format!("已保存的显示器不可用，诊断改用 {}", monitor.name))
        } else {
            None
        };
        config.select_monitor(monitor)?;
        if !config.region.fits(monitor) {
            return Err("诊断配置中的自定义区域超出显示器范围，请先在界面调整采集区域".into());
        }
        config.preview = false;
        config.diagnostics = true;
        config.validate()?;

        let mut engine = Engine::new();
        let mut traces = TraceCollection::default();
        let mut receiver_observations = Vec::new();
        let startup = Instant::now();
        let mut error = engine.start(config.clone()).err();
        let mut sending = false;
        while error.is_none() {
            let stats = engine.snapshot();
            collect(&stats, &mut traces, &mut receiver_observations);
            if sender_active(stats.phase, stats.frames) {
                sending = true;
                break;
            }
            if stats.phase == Phase::Error || !engine.is_running() {
                error = Some(stats.error.unwrap_or_else(|| "发送诊断未能启动".into()));
                break;
            }
            if startup.elapsed() >= Duration::from_secs(15) {
                error = Some("等待发送线程就绪超过 15 秒".into());
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let startup_seconds = startup.elapsed().as_secs_f64();
        let measurement = Instant::now();
        if sending {
            while measurement.elapsed() < limit {
                thread::sleep(
                    (limit - measurement.elapsed().min(limit)).min(Duration::from_millis(100)),
                );
                let stats = engine.snapshot();
                collect(&stats, &mut traces, &mut receiver_observations);
                if stats.phase == Phase::Error || !engine.is_running() {
                    error = Some(stats.error.unwrap_or_else(|| "发送诊断提前结束".into()));
                    break;
                }
            }
        }
        let actual_measurement_seconds = if sending {
            measurement.elapsed().as_secs_f64()
        } else {
            0.0
        };
        let shutdown = Instant::now();
        engine.shutdown();
        let shutdown_seconds = shutdown.elapsed().as_secs_f64();
        let stats = engine.snapshot();
        collect(&stats, &mut traces, &mut receiver_observations);
        if error.is_none() {
            error = stats.error.clone();
        }
        let trace = traces.finish(stats.frames);
        let maximum_observed_receivers = receiver_observations
            .iter()
            .map(|observation| observation.receivers)
            .max()
            .unwrap_or(0);
        let receiver_observed = maximum_observed_receivers > 0;
        let mut notes = vec![
            "All frame counts and traces cover this sender session, including frames sent before the first active sender status was observed and during shutdown.",
            "Desktop age uses the local Windows QPC clock and DXGI LastPresentTime. It describes desktop presentation age, not ROI content age or end-to-end latency.",
            "Desktop age percentiles include only desktop_updated samples with a known age. Static-desktop repeats and pointer-only updates are excluded from those percentiles.",
            "NDI submit duration may include waiting for the previous asynchronous send; it does not prove receiver delivery or decoding time.",
            "Diagnostic tracing adds timing and status collection overhead. Compare runs with the same diagnostics mode and configuration.",
        ];
        if !receiver_observed {
            notes.push(
                "No NDI receiver was observed. This run cannot be used as a transmission test.",
            );
        }
        if trace.missing_diagnostic_samples > 0 {
            notes.push("Diagnostic samples are missing. Percentiles describe collected frames only; inspect missing_frame_ranges before comparing runs.");
        }
        let report = Report {
            scope: "windows_sender_only",
            excluded: [
                "network_latency",
                "receiver_decode",
                "inference",
                "KMBox",
                "end_to_end_latency",
            ],
            notes,
            measurement_succeeded: sending && error.is_none(),
            final_phase: stats.phase,
            error: error.clone(),
            build: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            version: env!("CARGO_PKG_VERSION"),
            settings_warning,
            monitor_warning,
            config,
            diagnostics_enabled: true,
            requested_seconds: seconds,
            actual_measurement_seconds,
            startup_seconds,
            shutdown_seconds,
            sender_uptime_seconds: stats.uptime_sec,
            source: stats.source,
            backend: stats.backend,
            ndi_version: stats.ndi_version,
            frames: stats.frames,
            repeats: stats.repeats,
            receiver_observed,
            maximum_observed_receivers,
            receiver_observations,
            segment_distributions: summarize(&trace.samples),
            trace,
        };
        let json = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| format!("无法创建诊断报告目录：{error}"))?;
        }
        fs::write(output, json).map_err(|error| format!("无法写入诊断报告：{error}"))?;
        error.map_or(Ok(()), Err)
    }
}

/// Run a Windows sender session without changing the saved GUI configuration.
#[cfg(windows)]
pub fn measure_send(seconds: f64, output: &std::path::Path) -> Result<(), String> {
    windows_sender::measure_send(seconds, output)
}
