//! ScreenCaptureKit delivers into a single replaceable slot. Pixel readback stays
//! on the sending worker; a slow sender never builds a queue of old frames.
use crate::model::{Config, Monitor, Region};
use screencapturekit::{
    cg::CGRect,
    cm::{CMSampleBuffer, CMSampleBufferExt, CMSampleBufferSCExt},
    cv::CVPixelBufferLockFlags,
    error::SCError,
    prelude::{
        PixelFormat, SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration,
        SCStreamOutputType,
    },
    stream::delegate_trait::ErrorHandler,
};
use std::{
    ffi::{CStr, c_char, c_void},
    fmt, ptr,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const START_TIMEOUT: Duration = Duration::from_secs(10);
const WAIT_SLICE: Duration = Duration::from_millis(20);
const PERMISSION_HELP: &str = "请在系统设置 → 隐私与安全性 → 屏幕与系统音频录制中允许 FrameLink（旧版系统为“屏幕录制”），然后退出并重新打开软件";

#[derive(Debug)]
pub enum CaptureError {
    Recoverable(String),
    Unsupported(String),
    Fatal(String),
}
impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recoverable(s) | Self::Unsupported(s) | Self::Fatal(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for CaptureError {}

fn screen_error(operation: &str, error: SCError) -> CaptureError {
    if !has_permission() || matches!(error, SCError::PermissionDenied(_)) {
        CaptureError::Unsupported(format!("没有屏幕录制权限。{PERMISSION_HELP}"))
    } else {
        match error {
            SCError::InvalidConfiguration(_)
            | SCError::InvalidDimension { .. }
            | SCError::InvalidPixelFormat(_)
            | SCError::FeatureNotAvailable { .. } => {
                CaptureError::Unsupported(format!("{operation}失败：{error}"))
            }
            _ => CaptureError::Recoverable(format!("{operation}失败：{error}")),
        }
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGDisplayRotation(display: u32) -> f64;
    fn CGDisplayIsBuiltin(display: u32) -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void;
    fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize;
    fn CGDisplayModeGetRefreshRate(mode: *const c_void) -> f64;
    fn CGDisplayModeRelease(mode: *const c_void);
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *const c_void;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDCreateString(allocator: *const c_void, uuid: *const c_void) -> *const c_void;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> bool;
    fn CFRelease(value: *const c_void);
}

/// Does not display a dialog or change permission state.
pub fn has_permission() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Call only from an explicit user action. The system owns the permission dialog.
pub fn request_permission() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

fn display_identifier(display: u32) -> String {
    unsafe {
        let uuid = CGDisplayCreateUUIDFromDisplayID(display);
        if uuid.is_null() {
            return format!("mac-display-{display}");
        }
        let string = CFUUIDCreateString(ptr::null(), uuid);
        CFRelease(uuid);
        if string.is_null() {
            return format!("mac-display-{display}");
        }
        let mut bytes = [0 as c_char; 64];
        let valid = CFStringGetCString(
            string,
            bytes.as_mut_ptr(),
            bytes.len() as isize,
            0x0800_0100,
        );
        CFRelease(string);
        if valid {
            format!(
                "mac-display-{}",
                CStr::from_ptr(bytes.as_ptr()).to_string_lossy()
            )
        } else {
            format!("mac-display-{display}")
        }
    }
}

struct NativeMonitor {
    display: u32,
    monitor: Monitor,
}

fn native_monitors() -> Result<Vec<NativeMonitor>, String> {
    let mut count = 0;
    let error = unsafe { CGGetActiveDisplayList(0, ptr::null_mut(), &mut count) };
    if error != 0 {
        return Err(format!("无法枚举显示器（CoreGraphics {error}）"));
    }
    let mut displays = vec![0; count as usize];
    let error = unsafe { CGGetActiveDisplayList(count, displays.as_mut_ptr(), &mut count) };
    if error != 0 {
        return Err(format!("无法读取显示器列表（CoreGraphics {error}）"));
    }
    displays.truncate(count as usize);
    let primary = unsafe { CGMainDisplayID() };
    let mut result = Vec::with_capacity(displays.len());
    for display in displays {
        let bounds = unsafe { CGDisplayBounds(display) };
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            continue;
        }
        let (mut width, mut height) =
            unsafe { (CGDisplayPixelsWide(display), CGDisplayPixelsHigh(display)) };
        let mut refresh_hz = 0;
        let mode = unsafe { CGDisplayCopyDisplayMode(display) };
        if !mode.is_null() {
            unsafe {
                width = CGDisplayModeGetPixelWidth(mode);
                height = CGDisplayModeGetPixelHeight(mode);
                let rotation = CGDisplayRotation(display).rem_euclid(180.0);
                if (rotation - 90.0).abs() < 1.0 {
                    std::mem::swap(&mut width, &mut height);
                }
                let rate = CGDisplayModeGetRefreshRate(mode);
                if rate.is_finite() && (1.0..=1000.0).contains(&rate) {
                    refresh_hz = rate.round() as u32;
                }
                CGDisplayModeRelease(mode);
            }
        }
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            continue;
        };
        if width == 0 || height == 0 {
            continue;
        }
        // The selected display defines its own backing-pixel scale. Always
        // subtract this same origin before converting an ROI to local points.
        let x = (bounds.origin.x * f64::from(width) / bounds.size.width).round() as i32;
        let y = (bounds.origin.y * f64::from(height) / bounds.size.height).round() as i32;
        result.push(NativeMonitor {
            display,
            monitor: Monitor {
                id: display_identifier(display),
                name: if unsafe { CGDisplayIsBuiltin(display) } != 0 {
                    "内置显示器".into()
                } else {
                    format!("显示器 {}", result.len() + 1)
                },
                x,
                y,
                width,
                height,
                refresh_hz,
                primary: display == primary,
            },
        });
    }
    result.sort_by_key(|m| (!m.monitor.primary, m.monitor.x, m.monitor.y));
    if result.is_empty() {
        Err("未检测到活动显示器".into())
    } else {
        Ok(result)
    }
}

/// CoreGraphics enumeration needs no screen-recording permission. Dimensions
/// are backing pixels; a variable-refresh display may report refresh_hz = 0.
pub fn monitors() -> Result<Vec<Monitor>, String> {
    Ok(native_monitors()?.into_iter().map(|m| m.monitor).collect())
}

fn source_rect(
    monitor: &Monitor,
    region: Region,
    logical_width: f64,
    logical_height: f64,
) -> Result<CGRect, CaptureError> {
    if !region.fits(monitor)
        || !logical_width.is_finite()
        || !logical_height.is_finite()
        || logical_width <= 0.0
        || logical_height <= 0.0
    {
        return Err(CaptureError::Unsupported(
            "采集区域超出所选显示器，请重新选择区域".into(),
        ));
    }
    let scale_x = logical_width / f64::from(monitor.width);
    let scale_y = logical_height / f64::from(monitor.height);
    Ok(CGRect::new(
        (i64::from(region.x) - i64::from(monitor.x)) as f64 * scale_x,
        (i64::from(region.y) - i64::from(monitor.y)) as f64 * scale_y,
        f64::from(region.width) * scale_x,
        f64::from(region.height) * scale_y,
    ))
}

struct Latest<T> {
    frame: Option<T>,
    error: Option<String>,
    closed: bool,
}
impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self {
            frame: None,
            error: None,
            closed: false,
        }
    }
}
impl<T> Latest<T> {
    fn publish(&mut self, frame: T) {
        if !self.closed && self.error.is_none() {
            self.frame = Some(frame);
        }
    }
    fn close(&mut self) {
        self.closed = true;
        self.frame = None;
    }
}
struct PendingFrame {
    sample: CMSampleBuffer,
    sequence: u64,
    received_at: Instant,
}
#[derive(Default)]
struct Mailbox {
    latest: Mutex<Latest<PendingFrame>>,
    ready: Condvar,
    sequence: AtomicU64,
}

pub struct Frame {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub sequence: u64,
    /// Local callback arrival time; this is not a cross-machine timestamp.
    pub received_at: Instant,
    /// CPU mapping and copy time after a complete sample is available.
    pub readback_duration: Duration,
}

pub struct Capture {
    stream: SCStream,
    mailbox: Arc<Mailbox>,
    cancel: Arc<AtomicBool>,
    width: u32,
    height: u32,
    started_at: Instant,
    received_first: bool,
    reusable: Vec<u8>,
}
impl Capture {
    pub fn new(config: &Config) -> Result<Self, CaptureError> {
        Self::new_cancellable(config, Arc::new(AtomicBool::new(false)))
    }

    pub fn new_cancellable(config: &Config, cancel: Arc<AtomicBool>) -> Result<Self, CaptureError> {
        config.validate().map_err(CaptureError::Unsupported)?;
        if !has_permission() {
            return Err(CaptureError::Unsupported(format!(
                "没有屏幕录制权限。{PERMISSION_HELP}"
            )));
        }
        let config = config.clone();
        let abort = Arc::new(AtomicBool::new(false));
        let init_abort = abort.clone();
        let init_cancel = cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("screen-capture-start".into())
            .spawn(move || {
                let result = Self::open(&config, init_cancel.clone(), &init_abort);
                // A timed-out or cancelled caller drops the receiver. A late stream
                // is then stopped here, rather than becoming an orphan capture.
                let _ = sender.send(result);
            })
            .map_err(|error| CaptureError::Fatal(format!("无法创建采集线程：{error}")))?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if cancel.load(Ordering::Acquire) {
                abort.store(true, Ordering::Release);
                return Err(CaptureError::Recoverable("屏幕采集已取消".into()));
            }
            let now = Instant::now();
            if now >= deadline {
                abort.store(true, Ordering::Release);
                return Err(CaptureError::Recoverable(format!(
                    "屏幕采集启动超时。{PERMISSION_HELP}"
                )));
            }
            match receiver.recv_timeout((deadline - now).min(WAIT_SLICE)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CaptureError::Fatal("屏幕采集初始化线程意外退出".into()));
                }
            }
        }
    }

    fn open(
        config: &Config,
        cancel: Arc<AtomicBool>,
        abort: &AtomicBool,
    ) -> Result<Self, CaptureError> {
        let check_cancel = || {
            if cancel.load(Ordering::Acquire) || abort.load(Ordering::Acquire) {
                Err(CaptureError::Recoverable("屏幕采集已取消".into()))
            } else {
                Ok(())
            }
        };
        check_cancel()?;
        let monitors = native_monitors().map_err(CaptureError::Recoverable)?;
        let selected = monitors
            .iter()
            .find(|m| m.monitor.id == config.monitor)
            .or_else(|| {
                config
                    .monitor
                    .is_empty()
                    .then(|| monitors.first())
                    .flatten()
            })
            .ok_or_else(|| {
                CaptureError::Recoverable("所选显示器已断开，请重新选择显示器".into())
            })?;
        let content = SCShareableContent::get().map_err(|e| screen_error("读取屏幕内容", e))?;
        check_cancel()?;
        let display = content
            .displays()
            .into_iter()
            .find(|d| d.display_id() == selected.display)
            .ok_or_else(|| {
                CaptureError::Recoverable("无法访问所选显示器，请检查连接和屏幕录制权限".into())
            })?;
        let frame = display.frame();
        let rect = source_rect(
            &selected.monitor,
            config.region,
            frame.size.width,
            frame.size.height,
        )?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()
            .map_err(|e| screen_error("设置采集屏幕", e))?;
        let configuration = SCStreamConfiguration::new()
            .with_width(config.region.width)
            .with_height(config.region.height)
            .with_source_rect(rect)
            .with_pixel_format(PixelFormat::BGRA)
            .with_shows_cursor(config.cursor)
            .with_captures_audio(false)
            .with_queue_depth(3)
            .with_fps(if config.unlimited { 0 } else { config.fps });
        let mailbox = Arc::new(Mailbox::default());
        let errors = mailbox.clone();
        let delegate = ErrorHandler::new(move |error| {
            let mut state = errors.latest.lock().unwrap_or_else(|e| e.into_inner());
            if !state.closed {
                state.frame = None;
                state.error = Some(screen_error("屏幕采集中断", error).to_string());
                errors.ready.notify_one();
            }
        });
        let mut stream = SCStream::new_with_delegate(&filter, &configuration, delegate)
            .map_err(|e| screen_error("创建屏幕采集", e))?;
        let output = mailbox.clone();
        stream
            .add_output_handler(
                move |sample: CMSampleBuffer, kind| {
                    if !matches!(kind, SCStreamOutputType::Screen)
                        || !sample.is_valid()
                        || !sample
                            .frame_status()
                            .is_some_and(|status| status.is_complete())
                    {
                        return;
                    }
                    let sequence = output.sequence.fetch_add(1, Ordering::Relaxed) + 1;
                    let pending = PendingFrame {
                        sample,
                        sequence,
                        received_at: Instant::now(),
                    };
                    output
                        .latest
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .publish(pending);
                    output.ready.notify_one();
                },
                SCStreamOutputType::Screen,
            )
            .map_err(|e| screen_error("注册屏幕采集回调", e))?;
        check_cancel()?;
        stream
            .start_capture()
            .map_err(|e| screen_error("启动屏幕采集", e))?;
        let capture = Self {
            stream,
            mailbox,
            cancel: cancel.clone(),
            width: config.region.width,
            height: config.region.height,
            started_at: Instant::now(),
            received_first: false,
            reusable: Vec::new(),
        };
        check_cancel()?;
        Ok(capture)
    }

    /// Waits at most 20 ms so the sending worker can observe its stop flag.
    /// Idle desktop notifications are skipped instead of counted as new frames.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        if self.cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let state = self
            .mailbox
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut state, _) = self
            .mailbox
            .ready
            .wait_timeout_while(state, timeout.min(WAIT_SLICE), |state| {
                state.frame.is_none() && state.error.is_none() && !state.closed
            })
            .unwrap_or_else(|e| e.into_inner());
        if let Some(error) = state.error.take() {
            return Err(CaptureError::Recoverable(error));
        }
        let pending = state.frame.take();
        drop(state);
        let Some(pending) = pending else {
            if !self.received_first && self.started_at.elapsed() >= START_TIMEOUT {
                return Err(CaptureError::Recoverable(format!(
                    "10 秒内没有收到屏幕画面。{PERMISSION_HELP}"
                )));
            }
            return Ok(None);
        };
        let readback_started = Instant::now();
        let pixel = pending
            .sample
            .pixel_buffer()
            .ok_or_else(|| CaptureError::Recoverable("屏幕采集返回了无图像数据的帧".into()))?;
        if pixel.pixel_format() != u32::from_be_bytes(*b"BGRA") {
            return Err(CaptureError::Unsupported(
                "屏幕采集返回了不支持的像素格式".into(),
            ));
        }
        let guard = pixel
            .lock(CVPixelBufferLockFlags::READ_ONLY)
            .map_err(|e| CaptureError::Recoverable(format!("读取屏幕像素失败：{e}")))?;
        if guard.width() != self.width as usize || guard.height() != self.height as usize {
            return Err(CaptureError::Recoverable(
                "显示器尺寸发生变化，请重新开始发送".into(),
            ));
        }
        // The complete sample is retained until this read is finished. The
        // ScreenCaptureKit pool cannot recycle its IOSurface in that interval;
        // READ_ONLY synchronizes CPU mapping, and we never mutate its bytes.
        let source = unsafe { guard.as_slice() }
            .ok_or_else(|| CaptureError::Recoverable("无法映射屏幕像素".into()))?;
        let stride = self.width as usize * 4;
        let mut bytes = std::mem::take(&mut self.reusable);
        bytes.resize(stride * self.height as usize, 0);
        copy_rows(
            source,
            guard.bytes_per_row(),
            &mut bytes,
            stride,
            self.height as usize,
        )?;
        self.received_first = true;
        Ok(Some(Frame {
            bytes,
            width: self.width,
            height: self.height,
            stride,
            sequence: pending.sequence,
            received_at: pending.received_at,
            readback_duration: readback_started.elapsed(),
        }))
    }

    /// Return a consumed frame after conversion/submission has finished.
    pub fn recycle(&mut self, frame: Frame) {
        self.reusable = frame.bytes;
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.mailbox
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .close();
        self.mailbox.ready.notify_all();
        // Stop before releasing the callback state. The dependency also owns
        // its callback contexts until the native asynchronous stop completes.
        let _ = self.stream.stop_capture();
    }
}

fn copy_rows(
    source: &[u8],
    source_stride: usize,
    destination: &mut [u8],
    stride: usize,
    height: usize,
) -> Result<(), CaptureError> {
    let required = height
        .checked_sub(1)
        .and_then(|rows| rows.checked_mul(source_stride))
        .and_then(|offset| offset.checked_add(stride));
    if source_stride < stride
        || required.is_none_or(|size| size > source.len())
        || stride.checked_mul(height) != Some(destination.len())
    {
        return Err(CaptureError::Fatal("屏幕像素行跨度或缓冲区长度无效".into()));
    }
    for (row, output) in destination.chunks_exact_mut(stride).enumerate() {
        output.copy_from_slice(&source[row * source_stride..row * source_stride + stride]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_slot_overwrites_old_frames_and_rejects_after_close() {
        let mut slot = Latest::default();
        slot.publish(1);
        slot.publish(2);
        slot.publish(3);
        assert_eq!(slot.frame.take(), Some(3));
        assert_eq!(slot.frame.take(), None);
        slot.publish(4);
        slot.close();
        slot.publish(5);
        assert_eq!(slot.frame, None);
    }

    #[test]
    fn retina_region_uses_display_local_points_and_physical_output() {
        let monitor = Monitor {
            id: "retina".into(),
            name: "Retina".into(),
            x: -3840,
            y: 200,
            width: 3840,
            height: 2160,
            refresh_hz: 120,
            primary: false,
        };
        let region = Region {
            x: -2240,
            y: 960,
            width: 640,
            height: 640,
        };
        let rect = source_rect(&monitor, region, 1920.0, 1080.0).unwrap();
        assert_eq!(rect, CGRect::new(800.0, 380.0, 320.0, 320.0));
        assert!(source_rect(&monitor, Region { x: 0, ..region }, 1920.0, 1080.0).is_err());
    }

    #[test]
    fn copy_rows_ignores_native_padding_and_checks_bounds() {
        let source = [1, 2, 3, 4, 99, 99, 5, 6, 7, 8];
        let mut destination = [0; 8];
        copy_rows(&source, 6, &mut destination, 4, 2).unwrap();
        assert_eq!(destination, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(copy_rows(&source[..9], 6, &mut destination, 4, 2).is_err());
        assert!(copy_rows(&source, 3, &mut destination, 4, 2).is_err());
    }
}
