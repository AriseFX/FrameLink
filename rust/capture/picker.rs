//! A frozen-desktop region picker, in physical pixels, on the selected monitor.
use super::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};

struct GdiObject(HGDIOBJ);
impl Drop for GdiObject {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.0);
            }
        }
    }
}
struct Picker {
    monitor: Monitor,
    original: Dib,
    dimmed: Dib,
    canvas: Dib,
    start: Option<POINT>,
    mouse: POINT,
    result: Option<Region>,
    done: bool,
    font: GdiObject,
    scale: i32,
}
impl Picker {
    fn new(monitor: &Monitor) -> Result<Self, String> {
        let mut original = Dib::new(monitor.width, monitor.height).map_err(|e| e.to_string())?;
        original
            .capture(
                Region {
                    x: monitor.x,
                    y: monitor.y,
                    width: monitor.width,
                    height: monitor.height,
                },
                false,
            )
            .map_err(|e| e.to_string())?;
        let mut dimmed = Dib::new(monitor.width, monitor.height).map_err(|e| e.to_string())?;
        dimmed.copy(original.bytes(), original.stride());
        unsafe {
            for pixel in
                std::slice::from_raw_parts_mut(dimmed.pixels, dimmed.stride() * dimmed.height)
                    .as_chunks_mut::<4>()
                    .0
            {
                pixel[0] = (u16::from(pixel[0]) * 36 / 100) as u8;
                pixel[1] = (u16::from(pixel[1]) * 36 / 100) as u8;
                pixel[2] = (u16::from(pixel[2]) * 36 / 100) as u8;
            }
        }
        let canvas = Dib::new(monitor.width, monitor.height).map_err(|e| e.to_string())?;
        Ok(Self {
            monitor: monitor.clone(),
            original,
            dimmed,
            canvas,
            start: None,
            mouse: POINT::default(),
            result: None,
            done: false,
            font: GdiObject(HGDIOBJ::default()),
            scale: 96,
        })
    }
    fn selection(&self) -> Option<RECT> {
        let start = self.start?;
        let left = start.x.min(self.mouse.x);
        let top = start.y.min(self.mouse.y);
        let width = (start.x - self.mouse.x).unsigned_abs() & !1;
        let height = (start.y - self.mouse.y).unsigned_abs() & !1;
        (width >= 2 && height >= 2).then_some(RECT {
            left,
            top,
            right: left + width as i32,
            bottom: top + height as i32,
        })
    }
    fn pixels(&self, size: i32) -> i32 {
        (size * self.scale / 96).max(1)
    }
    fn mouse_position(&self, position: LPARAM) -> POINT {
        POINT {
            x: (position.0 as u16 as i16 as i32).clamp(0, self.monitor.width as i32),
            y: ((position.0 >> 16) as u16 as i16 as i32).clamp(0, self.monitor.height as i32),
        }
    }
    fn init_font(&mut self, hwnd: HWND) {
        self.scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
        self.font = GdiObject(
            unsafe {
                CreateFontW(
                    -self.pixels(17),
                    0,
                    0,
                    0,
                    500,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    windows::core::w!("Microsoft YaHei UI"),
                )
            }
            .into(),
        );
    }
    fn paint(&self, hwnd: HWND) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let target = BeginPaint(hwnd, &mut ps);
            if target.is_invalid() {
                return;
            }
            let dc = self.canvas.dc;
            let width = self.monitor.width as i32;
            let height = self.monitor.height as i32;
            let _ = BitBlt(dc, 0, 0, width, height, Some(self.dimmed.dc), 0, 0, SRCCOPY);
            let blue = GdiObject(CreateSolidBrush(COLORREF(0x00ffb94b)).into());
            if let Some(rect) = self.selection() {
                let _ = BitBlt(
                    dc,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    Some(self.original.dc),
                    rect.left,
                    rect.top,
                    SRCCOPY,
                );
                let thickness = self.pixels(2);
                let _ = FillRect(
                    dc,
                    &RECT {
                        bottom: rect.top + thickness,
                        ..rect
                    },
                    HBRUSH(blue.0.0),
                );
                let _ = FillRect(
                    dc,
                    &RECT {
                        top: rect.bottom - thickness,
                        ..rect
                    },
                    HBRUSH(blue.0.0),
                );
                let _ = FillRect(
                    dc,
                    &RECT {
                        right: rect.left + thickness,
                        ..rect
                    },
                    HBRUSH(blue.0.0),
                );
                let _ = FillRect(
                    dc,
                    &RECT {
                        left: rect.right - thickness,
                        ..rect
                    },
                    HBRUSH(blue.0.0),
                );
                // Small corner handles distinguish the selected crop on busy images.
                let half = self.pixels(4);
                for (x, y) in [
                    (rect.left, rect.top),
                    (rect.right, rect.top),
                    (rect.left, rect.bottom),
                    (rect.right, rect.bottom),
                ] {
                    let _ = FillRect(
                        dc,
                        &RECT {
                            left: x - half,
                            top: y - half,
                            right: x + half,
                            bottom: y + half,
                        },
                        HBRUSH(blue.0.0),
                    );
                }
            }
            let old_font = SelectObject(dc, self.font.0);
            SetBkMode(dc, TRANSPARENT);
            let _ = SetTextColor(dc, COLORREF(0x00fff6ed));
            let card_width = self.pixels(460).min(width - self.pixels(32));
            let card_height = self.pixels(74);
            let card_left = (width - card_width) / 2;
            let card_top = self.pixels(28).min((height - card_height).max(0));
            let background = GdiObject(CreateSolidBrush(COLORREF(0x0031251b)).into());
            let old_brush = SelectObject(dc, background.0);
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            let _ = RoundRect(
                dc,
                card_left,
                card_top,
                card_left + card_width,
                card_top + card_height,
                self.pixels(18),
                self.pixels(18),
            );
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            let mut title = "拖动选择采集区域".encode_utf16().collect::<Vec<_>>();
            let mut title_rect = RECT {
                left: card_left,
                top: card_top + self.pixels(9),
                right: card_left + card_width,
                bottom: card_top + self.pixels(35),
            };
            DrawTextW(
                dc,
                &mut title,
                &mut title_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let _ = SetTextColor(dc, COLORREF(0x00cdb9a8));
            let mut hint = "松开鼠标确认  ·  Esc / 右键取消"
                .encode_utf16()
                .collect::<Vec<_>>();
            let mut hint_rect = RECT {
                left: card_left,
                top: card_top + self.pixels(38),
                right: card_left + card_width,
                bottom: card_top + self.pixels(64),
            };
            DrawTextW(
                dc,
                &mut hint,
                &mut hint_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if let Some(rect) = self.selection() {
                let label = format!(
                    "{} × {}  ·  X {}  Y {}",
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    self.monitor.x + rect.left,
                    self.monitor.y + rect.top
                );
                let box_width = self.pixels(320).min(width);
                let box_height = self.pixels(38);
                let x = rect.left.clamp(0, (width - box_width).max(0));
                let y = if rect.bottom + self.pixels(10) + box_height < height {
                    rect.bottom + self.pixels(10)
                } else {
                    (rect.top - box_height - self.pixels(10)).max(0)
                };
                let old_brush = SelectObject(dc, background.0);
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                let _ = RoundRect(
                    dc,
                    x,
                    y,
                    x + box_width,
                    y + box_height,
                    self.pixels(12),
                    self.pixels(12),
                );
                SelectObject(dc, old_brush);
                SelectObject(dc, old_pen);
                let _ = SetTextColor(dc, COLORREF(0x00fff6ed));
                let mut label = label.encode_utf16().collect::<Vec<_>>();
                let mut r = RECT {
                    left: x,
                    top: y,
                    right: x + box_width,
                    bottom: y + box_height,
                };
                DrawTextW(
                    dc,
                    &mut label,
                    &mut r,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
            }
            SelectObject(dc, old_font);
            let _ = BitBlt(target, 0, 0, width, height, Some(dc), 0, 0, SRCCOPY);
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        return LRESULT(1);
    }
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Picker;
    if state.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    match message {
        WM_PAINT => {
            unsafe { &*state }.paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let picker = unsafe { &mut *state };
            picker.mouse = picker.mouse_position(lparam);
            picker.start = Some(picker.mouse);
            unsafe {
                SetCapture(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let picker = unsafe { &mut *state };
            if picker.start.is_some() {
                picker.mouse = picker.mouse_position(lparam);
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let picker = unsafe { &mut *state };
            picker.mouse = picker.mouse_position(lparam);
            if let Some(rect) = picker.selection() {
                picker.result = Some(Region {
                    x: picker.monitor.x + rect.left,
                    y: picker.monitor.y + rect.top,
                    width: (rect.right - rect.left) as u32,
                    height: (rect.bottom - rect.top) as u32,
                });
                picker.done = true;
                unsafe {
                    let _ = ReleaseCapture();
                    let _ = DestroyWindow(hwnd);
                }
            } else {
                picker.start = None;
                unsafe {
                    let _ = ReleaseCapture();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == 27 => {
            unsafe {
                (*state).done = true;
                let _ = ReleaseCapture();
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_CLOSE => {
            unsafe {
                (*state).done = true;
                let _ = ReleaseCapture();
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            // DestroyWindow itself can lose focus; guard against re-entering destruction.
            unsafe {
                if !(*state).done {
                    (*state).done = true;
                    let _ = ReleaseCapture();
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Run on a dedicated UI thread. Esc and right-click cancel without changing the region.
pub fn pick_region(monitor: &Monitor) -> Result<Option<Region>, String> {
    if monitor.width < 2 || monitor.height < 2 {
        return Err("显示器尺寸太小，无法框选".into());
    }
    let _dpi = DpiGuard::physical();
    let mut state = Box::new(Picker::new(monitor)?);
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| format!("无法获取程序实例：{e}"))?;
        // A unique class avoids cross-thread lifetime races when one picker is closing.
        let class_name = wide(&format!("FrameLink.RegionPicker.{:p}", &*state));
        let class = WNDCLASSW {
            hInstance: HINSTANCE(instance.0),
            lpfnWndProc: Some(procedure),
            hCursor: LoadCursorW(None, IDC_CROSS).map_err(|e| e.to_string())?,
            ..Default::default()
        };
        let class = WNDCLASSW {
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..class
        };
        if RegisterClassW(&class) == 0 {
            return Err(format!(
                "无法注册框选窗口：{}",
                windows::core::Error::from_thread()
            ));
        }
        let hwnd = match CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("选择采集区域"),
            WS_POPUP,
            monitor.x,
            monitor.y,
            monitor.width as i32,
            monitor.height as i32,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            Some((&mut *state as *mut Picker).cast::<c_void>()),
        ) {
            Ok(value) => value,
            Err(e) => {
                let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(HINSTANCE(instance.0)));
                return Err(format!("无法创建框选窗口：{e}"));
            }
        };
        state.init_font(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let _ = UpdateWindow(hwnd);
        let mut message = MSG::default();
        let mut failure = None;
        while IsWindow(Some(hwnd)).as_bool() {
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status <= 0 {
                if status < 0 {
                    failure = Some(format!(
                        "框选窗口消息失败：{}",
                        windows::core::Error::from_thread()
                    ));
                } else {
                    PostQuitMessage(message.wParam.0 as i32);
                }
                let _ = DestroyWindow(hwnd);
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(HINSTANCE(instance.0)));
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(state.result)
}
