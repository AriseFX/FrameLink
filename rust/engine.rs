//! The GUI only shares small status records and control flags with the worker.
//! Capture objects, NDI handles, timers, and pixel pointers stay on that worker.
use crate::model::{Config, Phase, Stats};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

struct Shared {
    stop: Arc<AtomicBool>,
    running: AtomicBool,
    stats: Mutex<Stats>,
}
impl Shared {
    fn stats(&self, update: impl FnOnce(&mut Stats)) {
        update(&mut self.stats.lock().unwrap_or_else(|e| e.into_inner()));
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
impl Engine {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                stop: Arc::new(AtomicBool::new(false)),
                running: AtomicBool::new(false),
                stats: Mutex::new(Stats::default()),
            }),
            worker: None,
        }
    }
    /// Stable for this Engine's entire lifetime, including subsequent sessions.
    pub fn stop_token(&self) -> Arc<AtomicBool> {
        self.shared.stop.clone()
    }
    pub fn start(&mut self, config: Config) -> Result<(), String> {
        config.validate()?;
        #[cfg(windows)]
        if config.transport == crate::model::Transport::Ndi {
            crate::network::validate_selection(&config.network_adapter)?;
        }
        self.poll();
        if self.worker.is_some() || self.is_running() {
            return Err("上一发送会话仍在结束，请稍候".into());
        }
        #[cfg(not(windows))]
        {
            let _ = config;
            let error = "屏幕采集需要Windows x64".to_string();
            self.shared.stats(|s| {
                s.phase = Phase::Error;
                s.error = Some(error.clone());
            });
            Err(error)
        }
        #[cfg(windows)]
        {
            self.shared.stop.store(false, Ordering::Release);
            self.shared.stats(|s| {
                *s = Stats {
                    phase: Phase::Starting,
                    ..Stats::default()
                }
            });
            self.shared.running.store(true, Ordering::Release);
            let shared = self.shared.clone();
            match std::thread::Builder::new()
                .name("framelink-latest-frame".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(windows)]
                        {
                            windows_worker::run(&shared, &config)
                        }
                    }));
                    let error = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(error),
                        Err(payload) => Some(format!(
                            "发送线程异常：{}",
                            payload
                                .downcast_ref::<&str>()
                                .copied()
                                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                                .unwrap_or("未知错误")
                        )),
                    };
                    shared.stats(|s| {
                        s.phase = if error.is_some() {
                            Phase::Error
                        } else {
                            Phase::Idle
                        };
                        s.error = error;
                        s.fps = 0.0;
                        s.updates_fps = 0.0;
                        s.receivers = 0;
                    });
                    shared.running.store(false, Ordering::Release);
                }) {
                Ok(worker) => {
                    self.worker = Some(worker);
                    Ok(())
                }
                Err(error) => {
                    self.shared.running.store(false, Ordering::Release);
                    let message = format!("无法启动发送线程：{error}");
                    self.shared.stats(|s| {
                        s.phase = Phase::Error;
                        s.error = Some(message.clone());
                    });
                    Err(message)
                }
            }
        }
    }
    /// Nonblocking: NDI drain and capture teardown happen on the worker.
    pub fn request_stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }
    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }
    pub fn snapshot(&self) -> Stats {
        let mut stats = self
            .shared
            .stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if self.is_running() && self.shared.stop.load(Ordering::Acquire) {
            stats.phase = Phase::Stopping;
        }
        stats
    }
    /// Reap a finished worker without ever joining a live worker on a UI update.
    pub fn poll(&mut self) {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
    pub fn shutdown(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(windows)]
mod scheduling;
#[cfg(windows)]
mod send_pipeline;

#[cfg(windows)]
mod windows_worker {
    use super::{
        send_pipeline::{Measurements, Output, Stream},
        *,
    };
    use crate::{
        capture::{Capture, CaptureError, monitors},
        diagnostics::FrameTiming,
        model::{Backend, Transport},
        ndi::Ndi,
        pixels::FrameBuffers,
        policy::{
            CaptureRetry, LatestFrameSlots, next_frame_deadline, region_in_desktop, timer_delay,
        },
    };
    use std::time::{Duration, Instant};
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, WAIT_FAILED},
            Media::{timeBeginPeriod, timeEndPeriod},
            System::Threading::{
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, SetWaitableTimer,
                TIMER_ALL_ACCESS, WaitForSingleObject,
            },
        },
        core::PCWSTR,
    };

    struct Pacer {
        timer: HANDLE,
        resolution: bool,
        precision: bool,
    }
    impl Pacer {
        fn new(precision: bool) -> Result<Self, String> {
            let high = unsafe {
                CreateWaitableTimerExW(
                    None,
                    PCWSTR::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS.0,
                )
            };
            let (timer, resolution) = match high {
                Ok(timer) => (timer, false),
                Err(_) => {
                    let timer = unsafe {
                        CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0)
                    }
                    .map_err(|e| format!("无法创建帧定时器：{e}"))?;
                    (timer, unsafe { timeBeginPeriod(1) } == 0)
                }
            };
            Ok(Self {
                timer,
                resolution,
                precision,
            })
        }
        fn until(&self, deadline: Instant, stop: &AtomicBool) -> Result<bool, String> {
            loop {
                if stop.load(Ordering::Acquire) {
                    return Ok(false);
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok(true);
                }
                let delay =
                    timer_delay(deadline - now, self.precision).min(Duration::from_millis(20));
                if delay.is_zero() {
                    // Only precision mode spins, and only in the final <=0.5 ms.
                    std::hint::spin_loop();
                } else {
                    let due = -((delay.as_nanos() / 100).max(1) as i64);
                    unsafe { SetWaitableTimer(self.timer, &due, 0, None, None, false) }
                        .map_err(|e| format!("帧定时器设置失败：{e}"))?;
                    if unsafe { WaitForSingleObject(self.timer, 20) } == WAIT_FAILED {
                        return Err("帧定时器等待失败".into());
                    }
                }
            }
        }
    }
    impl Drop for Pacer {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.timer) };
            if self.resolution {
                unsafe { timeEndPeriod(1) };
            }
        }
    }

    struct Capturer {
        capture: Option<Capture>,
        retry: CaptureRetry,
    }
    impl Capturer {
        fn new(config: &Config, shared: &Shared) -> Result<Self, String> {
            let mut result = Self {
                capture: None,
                retry: CaptureRetry::default(),
            };
            if config.backend == Backend::Gdi {
                result.capture = Some(Capture::gdi(config).map_err(capture_message)?);
                shared.stats(|s| {
                    s.backend = "GDI".into();
                    s.phase = Phase::Sending;
                });
            } else {
                result.dxgi(config, shared)?;
            }
            Ok(result)
        }
        fn dxgi(&mut self, config: &Config, shared: &Shared) -> Result<(), String> {
            match Capture::dxgi(config) {
                Ok(capture) => {
                    let backend = capture.backend_label();
                    self.capture = Some(capture);
                    self.retry.recovered();
                    shared.stats(|s| {
                        s.backend = backend;
                        s.phase = Phase::Sending;
                    });
                    Ok(())
                }
                Err(CaptureError::Recoverable(reason)) => self.recover(config, shared, &reason),
                Err(CaptureError::Unsupported(reason))
                    if config.backend == Backend::Auto || self.retry.pending() =>
                {
                    self.recover(config, shared, &reason)
                }
                Err(error) => Err(capture_message(error)),
            }
        }
        fn recover(
            &mut self,
            config: &Config,
            shared: &Shared,
            reason: &str,
        ) -> Result<(), String> {
            self.capture = None;
            self.retry.failed(Instant::now());
            if config.backend == Backend::Auto {
                match Capture::gdi(config) {
                    Ok(capture) => self.capture = Some(capture),
                    Err(CaptureError::Recoverable(_) | CaptureError::Unsupported(_)) => {}
                    Err(error) => return Err(capture_message(error)),
                }
            }
            let backend = if self.capture.is_some() {
                "GDI · 等待DXGI恢复"
            } else {
                "DXGI · 等待恢复"
            };
            shared.stats(|s| {
                s.phase = Phase::Recovering;
                s.backend = format!("{backend} · {reason}");
            });
            Ok(())
        }
    }
    fn capture_message(error: CaptureError) -> String {
        match error {
            CaptureError::Recoverable(s)
            | CaptureError::Unsupported(s)
            | CaptureError::Fatal(s) => s,
        }
    }

    /// Sender processing excludes the time the loop spent blocked inside the
    /// compositor, so both frame-pacing modes report the same kind of cost.
    fn processing_time(began: Instant, captured: Instant, wait: Duration) -> Duration {
        captured.duration_since(began).saturating_sub(wait)
    }

    fn validate_desktop_region(config: &Config) -> Result<(), String> {
        // Enumeration can temporarily fail on desktop/session transitions.
        // A real nonempty display layout provides enough evidence to stop an
        // out-of-bounds region and ask the user to select it again.
        if let Ok(displays) = monitors()
            && region_in_desktop(config.region, &displays) == Some(false)
        {
            return Err("显示器布局已变化，采集区域超出当前桌面，请重新选择区域".into());
        }
        Ok(())
    }
    pub(super) fn run(shared: &Shared, config: &Config) -> Result<(), String> {
        validate_desktop_region(config)?;
        let priority = super::scheduling::CapturePriority::new();
        shared.stats(|s| s.worker_priority = priority.level());
        let ndi = if config.transport == Transport::Ndi {
            Some(Ndi::load()?)
        } else {
            None
        };
        if shared.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let buffers = FrameBuffers::new(
            config.region.width,
            config.region.height,
            config.capture_pixel(),
        )?;
        let sender = if let Some(ndi) = &ndi {
            let sender = ndi.create_sender(&config.name)?;
            shared.stats(|s| {
                s.source = sender.source_name();
                s.ndi_version = ndi.version();
            });
            Output::Ndi(sender)
        } else {
            let sender = crate::transport::SocketSender::connect(config)?;
            shared.stats(|s| {
                s.source = format!("{}://{}", config.transport.label(), config.destination);
            });
            Output::Jpeg(Box::new(sender))
        };
        let mut stream = Stream::new(sender, buffers, config);
        let pacer = Pacer::new(config.precision)?;
        let mut capture = Capturer::new(config, shared)?;
        let mut slots = LatestFrameSlots::default();
        let started = Instant::now();
        let mut next = started;
        let interval = Duration::from_secs_f64(1.0 / config.fps as f64);
        let mut last_stats = started;
        let mut last_bounds_check = started;
        let (mut frames, mut repeats, mut window_frames, mut window_updates) =
            (0u64, 0u64, 0u64, 0u64);
        let mut measurements = Measurements::default();
        // Optional scalar timings only; no captured video is queued here.
        let mut timings = Vec::with_capacity(if config.diagnostics { 2048 } else { 0 });
        while !shared.stop.load(Ordering::Acquire) {
            if !pacer.until(next, &shared.stop)? {
                break;
            }
            if last_bounds_check.elapsed() >= Duration::from_secs(1) {
                validate_desktop_region(config)?;
                last_bounds_check = Instant::now();
            }
            if capture.retry.due(Instant::now()) {
                capture.dxgi(config, shared)?;
            }
            if capture.capture.is_none() {
                let receivers = stream.connections();
                shared.stats(|s| {
                    s.fps = 0.0;
                    s.updates_fps = 0.0;
                    s.capture_ms = 0.0;
                    s.conversion_ms = 0.0;
                    s.submit_ms = 0.0;
                    s.wait_ms = 0.0;
                    s.receivers = receivers;
                    s.uptime_sec = started.elapsed().as_secs_f64();
                });
                if let Some(deadline) = capture.retry.deadline()
                    && !pacer.until(deadline, &shared.stop)?
                {
                    break;
                }
                next = Instant::now();
                last_stats = next;
                window_frames = 0;
                window_updates = 0;
                measurements = Measurements::default();
                continue;
            }
            let current = capture.capture.as_mut().unwrap();
            let is_dxgi = current.is_dxgi();
            let began = Instant::now();
            let capture_started_100ns = config
                .diagnostics
                .then(crate::diagnostics::utc_timecode_100ns)
                .unwrap_or_default();
            let result = (|| -> Result<(), CaptureError> {
                // The pacer owns timing; sample the latest desktop without waiting for a refresh.
                let acquired = current.acquire(0)?;
                let Some(acquired) = acquired else {
                    return Ok(());
                };
                // Reuse the last converted pixels when the desktop has not changed.
                let plan = slots.prepare(acquired.changed);
                let mut captured = Instant::now();
                let mut converted = captured;
                if plan.refresh {
                    let result = if current.uses_gpu_nv12() {
                        current.with_nv12(|src, stride| {
                            captured = Instant::now();
                            let result = stream.buffers.copy_nv12(plan.slot, src, stride);
                            converted = Instant::now();
                            result
                        })?
                    } else {
                        current.with_bgra(|src, stride| {
                            captured = Instant::now();
                            let result = stream.buffers.convert(plan.slot, src, stride);
                            converted = Instant::now();
                            result
                        })?
                    };
                    result.map_err(CaptureError::Fatal)?;
                    if config.transport != Transport::Ndi {
                        // Release the mapped GPU surface before CPU JPEG work.
                        stream.encode(plan.slot).map_err(CaptureError::Fatal)?;
                        converted = Instant::now();
                    }
                }
                let source_timestamp = config
                    .diagnostics
                    .then(|| current.source_timestamp())
                    .flatten();
                let desktop_age_before_submit_ms = config
                    .diagnostics
                    .then(|| current.source_age_ms())
                    .flatten();
                let submit_started = config.diagnostics.then(Instant::now);
                let sent = stream
                    .submit(plan.slot, config.fps, &shared.stop)
                    .map_err(CaptureError::Fatal)?;
                slots.submitted(plan.slot);
                if !sent {
                    return Ok(());
                }
                let submitted = Instant::now();
                let desktop_age_after_submit_ms = config
                    .diagnostics
                    .then(|| current.source_age_ms())
                    .flatten();
                frames += 1;
                window_frames += 1;
                if plan.refresh {
                    window_updates += 1;
                } else {
                    repeats += 1;
                }
                measurements.capture +=
                    processing_time(began, captured, acquired.wait).as_secs_f64() * 1000.0;
                measurements.wait += acquired.wait.as_secs_f64() * 1000.0;
                measurements.conversion += (converted - captured).as_secs_f64() * 1000.0;
                measurements.submit += (submitted - converted).as_secs_f64() * 1000.0;
                measurements.count += 1;
                if let Some(submit_started) = submit_started
                    && timings.len() < timings.capacity()
                {
                    let capture = current.diagnostics();
                    timings.push(FrameTiming {
                        frame: frames,
                        ndi_timecode_100ns: stream.last_timecode(),
                        capture_started_100ns,
                        source_present_100ns: source_timestamp.map(|value| value.unix_100ns),
                        source_clock_uncertainty_100ns: source_timestamp
                            .map(|value| value.uncertainty_100ns),
                        refreshed: plan.refresh,
                        gpu_conversion: current.uses_gpu_nv12(),
                        desktop_updated: capture.desktop_updated,
                        release_ms: capture.release_ms,
                        acquire_ms: capture.acquire_ms,
                        map_wait_ms: capture.map_wait_ms,
                        conversion_ms: (converted - captured).as_secs_f64() * 1000.0,
                        ndi_submit_ms: (submitted - submit_started).as_secs_f64() * 1000.0,
                        desktop_age_before_submit_ms,
                        desktop_age_after_submit_ms,
                        accumulated_desktop_frames: capture.accumulated_frames,
                    });
                }
                Ok(())
            })();
            if let Err(error) = result {
                match error {
                    CaptureError::Recoverable(reason) if is_dxgi => {
                        capture.recover(config, shared, &reason)?
                    }
                    CaptureError::Recoverable(reason)
                        if config.backend == Backend::Auto && capture.retry.pending() =>
                    {
                        capture.capture = None;
                        capture.retry.failed(Instant::now());
                        shared.stats(|s| {
                            s.phase = Phase::Recovering;
                            s.backend = format!("DXGI · 等待桌面恢复 · {reason}");
                        });
                    }
                    error => return Err(capture_message(error)),
                }
            }
            let now = Instant::now();
            if now.duration_since(last_stats) >= Duration::from_millis(500) {
                let elapsed = now.duration_since(last_stats).as_secs_f64();
                let n = measurements.count.max(1) as f64;
                let receivers = stream.connections();
                shared.stats(|s| {
                    s.fps = window_frames as f64 / elapsed;
                    s.updates_fps = window_updates as f64 / elapsed;
                    s.receivers = receivers;
                    s.frames = frames;
                    s.repeats = repeats;
                    s.dropped = stream.dropped();
                    s.capture_ms = measurements.capture / n;
                    s.conversion_ms = measurements.conversion / n;
                    s.submit_ms = measurements.submit / n;
                    s.wait_ms = measurements.wait / n;
                    s.uptime_sec = now.duration_since(started).as_secs_f64();
                    if config.diagnostics {
                        s.timings.clone_from(&timings);
                    }
                });
                timings.clear();
                window_frames = 0;
                window_updates = 0;
                measurements = Measurements::default();
                last_stats = now;
            }
            next = next_frame_deadline(next, interval, now);
        }
        shared.stats(|s| {
            s.phase = Phase::Stopping;
            s.frames = frames;
            s.repeats = repeats;
            s.dropped = stream.dropped();
            s.uptime_sec = started.elapsed().as_secs_f64();
            if config.diagnostics && !timings.is_empty() {
                s.timings.clone_from(&timings);
            }
        });
        // Stream's sender is its first field: draining and destroy complete
        // before stable pixel buffers/descriptors are dropped, including unwind.
        drop(stream);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn processing_time_excludes_compositor_wait() {
            let began = Instant::now();
            let captured = began + Duration::from_micros(5_000);
            assert_eq!(
                processing_time(began, captured, Duration::from_micros(4_000)),
                Duration::from_micros(1_000)
            );
            assert_eq!(
                processing_time(began, captured, Duration::ZERO),
                Duration::from_micros(5_000)
            );
            // Clock granularity can report a longer wait than the span; never underflow.
            assert_eq!(
                processing_time(began, captured, Duration::from_micros(9_000)),
                Duration::ZERO
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_token_is_stable_and_does_not_block() {
        let engine = Engine::new();
        let token = engine.stop_token();
        assert!(Arc::ptr_eq(&token, &engine.stop_token()));
        engine.request_stop();
        assert!(token.load(Ordering::Acquire));
        assert!(!engine.is_running());
    }
    #[test]
    #[cfg(not(windows))]
    fn unsupported_platform_cannot_claim_to_send_a_stream() {
        let mut engine = Engine::new();
        assert!(
            engine
                .start(Config::default())
                .unwrap_err()
                .contains("Windows x64")
        );
        assert_eq!(engine.snapshot().phase, Phase::Error);
        assert!(!engine.is_running());
    }
}
