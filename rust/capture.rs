//! Windows desktop acquisition. All GPU and GDI state stays on the capture thread.
//! The hot DXGI path maps only the requested crop and exposes it until the callback returns.
use crate::model::{Config, Monitor, Region};
use std::{
    ffi::c_void,
    fmt,
    mem::size_of,
    ptr,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::*,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
    core::{BOOL, Interface, PCWSTR},
};

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
fn error(operation: &str, value: windows::core::Error) -> CaptureError {
    let code = value.code().0 as u32;
    let message = format!("{operation}失败 (0x{code:08X})：{value}");
    match code {
        0x887a0005 | 0x887a0007 | 0x887a0022 | 0x887a0026 | 0x887a0028 | 0x887a0025
        | 0x80070005 => CaptureError::Recoverable(message),
        _ => CaptureError::Fatal(message),
    }
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn from_wide(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|&v| v == 0).unwrap_or(value.len())])
}
fn contains(bounds: RECT, region: Region) -> bool {
    i64::from(region.x) >= i64::from(bounds.left)
        && i64::from(region.y) >= i64::from(bounds.top)
        && i64::from(region.x) + i64::from(region.width) <= i64::from(bounds.right)
        && i64::from(region.y) + i64::from(region.height) <= i64::from(bounds.bottom)
}

struct DpiGuard(DPI_AWARENESS_CONTEXT);
impl DpiGuard {
    fn physical() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for DpiGuard {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}

/// Device names are persistent identifiers; geometry uses physical desktop pixels.
pub fn monitors() -> Result<Vec<Monitor>, String> {
    unsafe extern "system" fn enumerate(
        handle: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(handle, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
            return true.into();
        }
        let bounds = info.monitorInfo.rcMonitor;
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let refresh_hz = if unsafe {
            EnumDisplaySettingsW(
                PCWSTR(info.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
        }
        .as_bool()
            && (2..=1000).contains(&mode.dmDisplayFrequency)
        {
            mode.dmDisplayFrequency
        } else {
            60
        };
        let id = from_wide(&info.szDevice);
        let mut display = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        let title =
            if unsafe { EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()), 0, &mut display, 0) }
                .as_bool()
            {
                from_wide(&display.DeviceString)
            } else {
                String::new()
            };
        list.push(Monitor {
            id,
            name: if title.is_empty() {
                format!("显示器 {}", list.len() + 1)
            } else {
                title
            },
            x: bounds.left,
            y: bounds.top,
            width: (bounds.right - bounds.left) as u32,
            height: (bounds.bottom - bounds.top) as u32,
            refresh_hz,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
        true.into()
    }
    let _dpi = DpiGuard::physical();
    let mut result: Vec<Monitor> = Vec::new();
    if !unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enumerate),
            LPARAM(&mut result as *mut _ as isize),
        )
    }
    .as_bool()
    {
        return Err(format!(
            "无法枚举显示器：{}",
            windows::core::Error::from_thread()
        ));
    }
    result.sort_by_key(|m| (!m.primary, m.x, m.y));
    if result.is_empty() {
        Err("未检测到活动显示器".into())
    } else {
        Ok(result)
    }
}

struct Dib {
    screen: HDC,
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u8,
    width: usize,
    height: usize,
}
impl Dib {
    fn new(width: u32, height: u32) -> Result<Self, CaptureError> {
        let mut result = Self {
            screen: HDC::default(),
            dc: HDC::default(),
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
            pixels: ptr::null_mut(),
            width: width as usize,
            height: height as usize,
        };
        unsafe {
            result.screen = GetDC(None);
            if result.screen.is_invalid() {
                return Err(CaptureError::Recoverable(
                    "无法访问桌面，请检查显示器和桌面锁定状态".into(),
                ));
            }
            result.dc = CreateCompatibleDC(Some(result.screen));
            if result.dc.is_invalid() {
                return Err(error("分配桌面DC", windows::core::Error::from_thread()));
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = ptr::null_mut();
            result.bitmap = CreateDIBSection(
                Some(result.screen),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )
            .map_err(|e| error("分配桌面位图", e))?;
            result.pixels = bits.cast();
            result.previous = SelectObject(result.dc, result.bitmap.into());
            if result.previous.is_invalid()
                || result.previous.0 as isize == -1
                || result.pixels.is_null()
            {
                return Err(CaptureError::Fatal("桌面位图初始化失败".into()));
            }
        }
        Ok(result)
    }
    fn stride(&self) -> usize {
        self.width * 4
    }
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pixels, self.stride() * self.height) }
    }
    fn capture(&mut self, region: Region, cursor: bool) -> Result<(), CaptureError> {
        unsafe {
            if BitBlt(
                self.dc,
                0,
                0,
                self.width as i32,
                self.height as i32,
                Some(self.screen),
                region.x,
                region.y,
                SRCCOPY | CAPTUREBLT,
            )
            .is_err()
            {
                return Err(CaptureError::Recoverable(
                    "桌面采集失败，请检查显示器和桌面锁定状态".into(),
                ));
            }
            if cursor {
                self.draw_cursor(region);
            }
            let _ = GdiFlush();
        }
        Ok(())
    }
    fn copy(&mut self, source: &[u8], pitch: usize) {
        let stride = self.stride();
        unsafe {
            for row in 0..self.height {
                ptr::copy_nonoverlapping(
                    source.as_ptr().add(row * pitch),
                    self.pixels.add(row * stride),
                    stride,
                );
            }
        }
    }
    fn draw_cursor(&mut self, region: Region) {
        let _dpi = DpiGuard::physical();
        unsafe {
            let mut cursor = CURSORINFO {
                cbSize: size_of::<CURSORINFO>() as u32,
                ..Default::default()
            };
            if GetCursorInfo(&mut cursor).is_err() || cursor.flags != CURSOR_SHOWING {
                return;
            }
            let mut icon = ICONINFO::default();
            if GetIconInfo(HICON(cursor.hCursor.0), &mut icon).is_err() {
                return;
            }
            let _ = DrawIconEx(
                self.dc,
                cursor.ptScreenPos.x - region.x - icon.xHotspot as i32,
                cursor.ptScreenPos.y - region.y - icon.yHotspot as i32,
                HICON(cursor.hCursor.0),
                0,
                0,
                0,
                None,
                DI_NORMAL,
            );
            if !icon.hbmMask.is_invalid() {
                let _ = DeleteObject(icon.hbmMask.into());
            }
            if !icon.hbmColor.is_invalid() {
                let _ = DeleteObject(icon.hbmColor.into());
            }
            let _ = GdiFlush();
        }
    }
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            if !self.dc.is_invalid()
                && !self.previous.is_invalid()
                && self.previous.0 as isize != -1
            {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
            if !self.screen.is_invalid() {
                ReleaseDC(None, self.screen);
            }
        }
    }
}

struct DxgiCapture {
    context: ID3D11DeviceContext,
    staging: ID3D11Texture2D,
    duplication: IDXGIOutputDuplication,
    region: Region,
    bounds: RECT,
    nominal: u32,
    has_image: bool,
    cursor: bool,
    separate_cursor: bool,
    cursor_buffer: Option<Dib>,
    unlimited: bool,
    last_yield: Instant,
}
struct FrameGuard<'a>(&'a IDXGIOutputDuplication);
impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.ReleaseFrame();
        }
    }
}
struct MapGuard<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}
impl Drop for MapGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.context.Unmap(self.texture, 0);
        }
    }
}
impl DxgiCapture {
    fn new(config: &Config) -> Result<Self, CaptureError> {
        let _dpi = DpiGuard::physical();
        config.validate().map_err(CaptureError::Fatal)?;
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| error("创建DXGI工厂", e))?;
            let mut ai = 0;
            loop {
                let adapter = match factory.EnumAdapters1(ai) {
                    Ok(value) => value,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(error("枚举显卡", e)),
                };
                ai += 1;
                let mut oi = 0;
                loop {
                    let output = match adapter.EnumOutputs(oi) {
                        Ok(value) => value,
                        Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                        Err(e) => return Err(error("枚举显示输出", e)),
                    };
                    oi += 1;
                    let description = output.GetDesc().map_err(|e| error("读取显示输出", e))?;
                    if !description.AttachedToDesktop.as_bool()
                        || !contains(description.DesktopCoordinates, config.region)
                    {
                        continue;
                    }
                    if description.Rotation != DXGI_MODE_ROTATION_IDENTITY
                        && description.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED
                    {
                        return Err(CaptureError::Unsupported(
                            "旋转显示器需要使用兼容采集（GDI）".into(),
                        ));
                    }
                    let mut device = None;
                    let mut context = None;
                    D3D11CreateDevice(
                        &adapter,
                        D3D_DRIVER_TYPE_UNKNOWN,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    )
                    .map_err(|e| error("创建D3D11设备", e))?;
                    let device =
                        device.ok_or_else(|| CaptureError::Fatal("D3D11没有返回设备".into()))?;
                    let context =
                        context.ok_or_else(|| CaptureError::Fatal("D3D11没有返回上下文".into()))?;
                    let output: IDXGIOutput1 =
                        output.cast().map_err(|e| error("获取DXGI 1.2接口", e))?;
                    let duplication = output.DuplicateOutput(&device).map_err(|e| {
                        if e.code() == DXGI_ERROR_UNSUPPORTED {
                            CaptureError::Recoverable("当前显示模式暂不支持DXGI采集".into())
                        } else {
                            error("创建桌面复制", e)
                        }
                    })?;
                    let desc = D3D11_TEXTURE2D_DESC {
                        Width: config.region.width,
                        Height: config.region.height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_STAGING,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        ..Default::default()
                    };
                    let mut staging = None;
                    device
                        .CreateTexture2D(&desc, None, Some(&mut staging))
                        .map_err(|e| error("创建区域读取纹理", e))?;
                    let staging = staging
                        .ok_or_else(|| CaptureError::Fatal("D3D11没有返回读取纹理".into()))?;
                    let mut mode = DEVMODEW {
                        dmSize: size_of::<DEVMODEW>() as u16,
                        ..Default::default()
                    };
                    let nominal = if config.unlimited
                        && EnumDisplaySettingsW(
                            PCWSTR(description.DeviceName.as_ptr()),
                            ENUM_CURRENT_SETTINGS,
                            &mut mode,
                        )
                        .as_bool()
                        && (2..=1000).contains(&mode.dmDisplayFrequency)
                    {
                        mode.dmDisplayFrequency
                    } else {
                        config.fps
                    };
                    return Ok(Self {
                        context,
                        staging,
                        duplication,
                        region: config.region,
                        bounds: description.DesktopCoordinates,
                        nominal,
                        has_image: false,
                        cursor: config.cursor,
                        separate_cursor: false,
                        cursor_buffer: if config.cursor {
                            Some(Dib::new(config.region.width, config.region.height)?)
                        } else {
                            None
                        },
                        unlimited: config.unlimited,
                        last_yield: Instant::now(),
                    });
                }
            }
        }
        Err(CaptureError::Unsupported(
            "DXGI采集区域必须完整位于一台活动显示器内；跨屏请使用兼容采集（GDI）".into(),
        ))
    }
    fn acquire(&mut self, timeout_ms: u32) -> Result<Option<bool>, CaptureError> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        let mut changed = false;
        match unsafe {
            self.duplication
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
        } {
            Ok(()) => {
                let frame = FrameGuard(&self.duplication);
                if info.LastMouseUpdateTime != 0 {
                    self.separate_cursor = info.PointerPosition.Visible.as_bool();
                    changed = self.cursor;
                }
                if self.cursor && info.PointerShapeBufferSize > 0 {
                    changed = true;
                }
                if info.LastPresentTime != 0 || !self.has_image {
                    let resource = resource
                        .ok_or_else(|| CaptureError::Fatal("桌面复制没有返回帧资源".into()))?;
                    let texture: ID3D11Texture2D =
                        resource.cast().map_err(|e| error("读取桌面纹理", e))?;
                    let x = (self.region.x - self.bounds.left) as u32;
                    let y = (self.region.y - self.bounds.top) as u32;
                    let area = D3D11_BOX {
                        left: x,
                        top: y,
                        front: 0,
                        right: x + self.region.width,
                        bottom: y + self.region.height,
                        back: 1,
                    };
                    unsafe {
                        self.context.CopySubresourceRegion(
                            &self.staging,
                            0,
                            0,
                            0,
                            0,
                            &texture,
                            0,
                            Some(&area),
                        );
                    }
                    self.has_image = true;
                    changed = true;
                }
                // Return ReleaseFrame errors explicitly, with the guard only for early exits/panics.
                std::mem::forget(frame);
                unsafe { self.duplication.ReleaseFrame() }.map_err(|e| error("释放桌面帧", e))?;
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(e) => return Err(error("获取桌面帧", e)),
        }
        if !self.has_image {
            return Ok(None);
        }
        if self.unlimited && !changed && self.last_yield.elapsed() < Duration::from_secs(1) {
            return Ok(None);
        }
        self.last_yield = Instant::now();
        Ok(Some(changed))
    }
    fn with_bgra<T>(
        &mut self,
        callback: impl FnOnce(&[u8], usize) -> T,
    ) -> Result<T, CaptureError> {
        if !self.has_image {
            return Err(CaptureError::Fatal("尚无可读取的桌面帧".into()));
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|e| error("映射桌面区域", e))?;
        let guard = MapGuard {
            context: &self.context,
            texture: &self.staging,
        };
        let pitch = mapped.RowPitch as usize;
        if mapped.pData.is_null() || pitch < self.region.width as usize * 4 {
            return Err(CaptureError::Fatal("桌面纹理步长或映射地址无效".into()));
        }
        let pixels = unsafe {
            std::slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                pitch * self.region.height as usize,
            )
        };
        if let Some(buffer) = self.cursor_buffer.as_mut() {
            buffer.copy(pixels, pitch);
            drop(guard);
            if self.separate_cursor {
                buffer.draw_cursor(self.region);
            }
            Ok(callback(buffer.bytes(), buffer.stride()))
        } else {
            let output = callback(pixels, pitch);
            drop(guard);
            Ok(output)
        }
    }
}

pub struct Capture {
    inner: CaptureInner,
}
enum CaptureInner {
    Dxgi(DxgiCapture),
    Gdi {
        region: Region,
        cursor: bool,
        nominal: u32,
        buffer: Dib,
        has_image: bool,
    },
}
impl Capture {
    pub fn dxgi(config: &Config) -> Result<Self, CaptureError> {
        Ok(Self {
            inner: CaptureInner::Dxgi(DxgiCapture::new(config)?),
        })
    }
    pub fn gdi(config: &Config) -> Result<Self, CaptureError> {
        config.validate().map_err(CaptureError::Fatal)?;
        let active = monitors().map_err(CaptureError::Recoverable)?;
        let left = active.iter().map(|m| m.x).min().unwrap();
        let top = active.iter().map(|m| m.y).min().unwrap();
        let right = active.iter().map(|m| m.x + m.width as i32).max().unwrap();
        let bottom = active.iter().map(|m| m.y + m.height as i32).max().unwrap();
        if !contains(
            RECT {
                left,
                top,
                right,
                bottom,
            },
            config.region,
        ) {
            return Err(CaptureError::Fatal("采集区域位于桌面之外".into()));
        }
        Ok(Self {
            inner: CaptureInner::Gdi {
                region: config.region,
                cursor: config.cursor,
                nominal: config.fps,
                buffer: Dib::new(config.region.width, config.region.height)?,
                has_image: false,
            },
        })
    }
    pub fn acquire(&mut self, timeout_ms: u32) -> Result<Option<bool>, CaptureError> {
        match &mut self.inner {
            CaptureInner::Dxgi(c) => c.acquire(timeout_ms),
            CaptureInner::Gdi {
                region,
                cursor,
                buffer,
                has_image,
                ..
            } => {
                let _dpi = DpiGuard::physical();
                buffer.capture(*region, *cursor)?;
                *has_image = true;
                Ok(Some(true))
            }
        }
    }
    pub fn with_bgra<T>(
        &mut self,
        callback: impl FnOnce(&[u8], usize) -> T,
    ) -> Result<T, CaptureError> {
        match &mut self.inner {
            CaptureInner::Dxgi(c) => c.with_bgra(callback),
            CaptureInner::Gdi {
                buffer, has_image, ..
            } => {
                if !*has_image {
                    return Err(CaptureError::Fatal("尚无可读取的桌面帧".into()));
                }
                Ok(callback(buffer.bytes(), buffer.stride()))
            }
        }
    }
    pub fn nominal_fps(&self) -> u32 {
        match &self.inner {
            CaptureInner::Dxgi(c) => c.nominal,
            CaptureInner::Gdi { nominal, .. } => *nominal,
        }
    }
    pub fn is_dxgi(&self) -> bool {
        matches!(self.inner, CaptureInner::Dxgi(_))
    }
}

#[path = "capture/picker.rs"]
mod picker;
pub use picker::pick_region;
