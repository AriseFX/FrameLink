#[derive(Clone, Copy, Debug)]
pub enum TrayAction {
    Show,
    Stop,
    Exit,
}

pub fn register_window(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = cc.window_handle()
            && let RawWindowHandle::Win32(handle) = handle.as_raw()
        {
            cc.egui_ctx.data_mut(|data| {
                data.insert_temp(
                    eframe::egui::Id::new("native-root-window"),
                    handle.hwnd.get(),
                )
            });
        }
    }
    #[cfg(not(windows))]
    let _ = cc;
}

#[cfg(windows)]
mod implementation {
    use super::TrayAction;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    };
    use tray_icon::{
        Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            IsIconic, IsWindow, SW_HIDE, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
        },
    };

    pub struct Tray {
        _icon: TrayIcon,
        actions: Receiver<TrayAction>,
    }
    pub fn set_window_visible(ctx: &eframe::egui::Context, visible: bool) {
        // Wake the real window even while egui's viewport is hidden. The
        // viewport commands then keep eframe's visibility state in sync.
        unsafe {
            if let Some(handle) =
                ctx.data(|data| data.get_temp::<isize>(eframe::egui::Id::new("native-root-window")))
            {
                let window = HWND(handle as *mut std::ffi::c_void);
                if !IsWindow(Some(window)).as_bool() {
                    return;
                }
                let mode = if !visible {
                    SW_HIDE
                } else if IsIconic(window).as_bool() {
                    SW_RESTORE
                } else {
                    SW_SHOW
                };
                let _ = ShowWindow(window, mode);
                if visible {
                    let _ = SetForegroundWindow(window);
                }
            }
        }
        ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Visible(visible));
        if visible {
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Focus);
        }
        ctx.request_repaint();
    }
    impl Tray {
        pub fn new(ctx: &eframe::egui::Context, stop: Arc<AtomicBool>) -> Result<Self, String> {
            let menu = Menu::new();
            let show = MenuItem::new(format!("打开 {}", crate::brand::APP_NAME), true, None);
            let pause = MenuItem::new("停止发送", true, None);
            let exit = MenuItem::new("退出", true, None);
            menu.append_items(&[&show, &pause, &PredefinedMenuItem::separator(), &exit])
                .map_err(|e| e.to_string())?;
            let image = image::load_from_memory(include_bytes!("../resources/app.ico"))
                .map_err(|e| e.to_string())?
                .to_rgba8();
            let (width, height) = image.dimensions();
            let icon =
                Icon::from_rgba(image.into_raw(), width, height).map_err(|e| e.to_string())?;
            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("FrameLink 帧连 · 屏幕推流")
                .with_icon(icon)
                .build()
                .map_err(|e| e.to_string())?;
            let (sender, actions) = mpsc::channel();
            let show_id = show.id().clone();
            let pause_id = pause.id().clone();
            let exit_id = exit.id().clone();
            let context = ctx.clone();
            let events = sender.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = if event.id == show_id {
                    TrayAction::Show
                } else if event.id == pause_id {
                    stop.store(true, Ordering::Release);
                    TrayAction::Stop
                } else if event.id == exit_id {
                    TrayAction::Exit
                } else {
                    return;
                };
                let _ = events.send(action);
                // Stop is performed above and can remain hidden. Exit restores
                // the app so it can finish stopping and close on its UI thread.
                if !matches!(action, TrayAction::Stop) {
                    set_window_visible(&context, true);
                }
                context.request_repaint();
            }));
            let context = ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |event| {
                if matches!(
                    event,
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                ) {
                    let _ = sender.send(TrayAction::Show);
                    set_window_visible(&context, true);
                }
            }));
            Ok(Self {
                _icon: icon,
                actions,
            })
        }
        pub fn poll(&self) -> Option<TrayAction> {
            self.actions.try_recv().ok()
        }
    }
    impl Drop for Tray {
        fn drop(&mut self) {
            MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
            TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
        }
    }
}
#[cfg(windows)]
pub use implementation::{Tray, set_window_visible};

#[cfg(not(windows))]
pub struct Tray;
#[cfg(not(windows))]
pub fn set_window_visible(ctx: &eframe::egui::Context, visible: bool) {
    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Visible(visible));
}
#[cfg(not(windows))]
impl Tray {
    pub fn new(
        _: &eframe::egui::Context,
        _: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self, String> {
        Err("托盘发送仅支持Windows".into())
    }
    pub fn poll(&self) -> Option<TrayAction> {
        None
    }
}
