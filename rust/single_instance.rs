//! Windows process ownership and activation, independent of the executable path.
//! The mutex lives on the main thread until eframe and the sender have exited.

#[cfg(windows)]
mod implementation {
    use eframe::egui::{self, Context};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        marker::PhantomData,
        rc::Rc,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicIsize, Ordering},
        },
        thread::JoinHandle,
    };
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, HANDLE, HWND, LPARAM, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
            },
            System::Threading::{
                CreateEventW, CreateMutexW, INFINITE, ReleaseMutex, SetEvent,
                WaitForMultipleObjects, WaitForSingleObject,
            },
            UI::WindowsAndMessaging::{
                AllowSetForegroundWindow, EnumWindows, GetPropW, GetWindowThreadProcessId,
                IsIconic, IsWindow, RemovePropW, SW_RESTORE, SW_SHOW, SetForegroundWindow,
                SetPropW, ShowWindowAsync,
            },
        },
        core::{BOOL, PCWSTR, w},
    };

    // Stable across releases and copies of the executable. Global scope keeps
    // one owner on the machine; activation can show only its interactive desktop.
    const MUTEX_NAME: PCWSTR =
        w!("Global\\FrameLink.Instance.7D57CFC8-11A1-4E8C-91B7-8EE8F5F98D45");
    const EVENT_NAME: PCWSTR =
        w!("Global\\FrameLink.Activate.7D57CFC8-11A1-4E8C-91B7-8EE8F5F98D45");
    const WINDOW_PROPERTY: PCWSTR =
        w!("FrameLink.SingleInstance.Window.7D57CFC8-11A1-4E8C-91B7-8EE8F5F98D45");
    const ACTIVATION_KEY: &str = "framelink-single-instance-activation";

    struct OwnedMutex {
        handle: HANDLE,
        owned: bool,
        // Windows mutex ownership belongs to the creating/waiting thread.
        _main_thread: PhantomData<Rc<()>>,
    }
    impl Drop for OwnedMutex {
        fn drop(&mut self) {
            unsafe {
                if self.owned {
                    let _ = ReleaseMutex(self.handle);
                }
                let _ = CloseHandle(self.handle);
            }
        }
    }

    struct Event(HANDLE);
    // SAFETY: this wrapper contains only a kernel event, whose wait/signal APIs
    // support multiple threads. Arc keeps its handle alive until all waits end.
    unsafe impl Send for Event {}
    unsafe impl Sync for Event {}
    impl Event {
        fn new(name: PCWSTR, manual: bool) -> Result<Self, String> {
            unsafe { CreateEventW(None, manual, false, name) }
                .map(Self)
                .map_err(|e| format!("无法创建窗口唤醒事件：{e}"))
        }
        fn signal(&self) -> Result<(), String> {
            unsafe { SetEvent(self.0) }.map_err(|e| format!("无法唤醒已有窗口：{e}"))
        }
    }
    impl Drop for Event {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct Binding {
        request: Arc<Event>,
        stop: Arc<Event>,
        pending: Arc<AtomicBool>,
        window: AtomicIsize,
        listener: Mutex<Option<JoinHandle<()>>>,
    }

    pub struct PrimaryInstance {
        _mutex: OwnedMutex,
        binding: Arc<Binding>,
    }

    #[derive(Clone)]
    pub struct ActivationRegistration(Arc<Binding>);

    impl PrimaryInstance {
        /// None means that the existing instance was notified and this process
        /// must return before reading settings, creating a tray or loading NDI.
        pub fn acquire() -> Result<Option<Self>, String> {
            Self::acquire_named(MUTEX_NAME, EVENT_NAME, true)
        }

        fn acquire_named(
            mutex_name: PCWSTR,
            event_name: PCWSTR,
            activate: bool,
        ) -> Result<Option<Self>, String> {
            // Create the auto-reset event first. A second launch can signal it
            // before the first window/listener exists without losing the request.
            let request = Arc::new(Event::new(event_name, false)?);
            let handle = unsafe { CreateMutexW(None, false, mutex_name) }
                .map_err(|e| format!("无法检查 FrameLink 运行状态：{e}"))?;
            let mut mutex = OwnedMutex {
                handle,
                owned: false,
                _main_thread: PhantomData,
            };
            match unsafe { WaitForSingleObject(mutex.handle, 0) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => mutex.owned = true,
                WAIT_TIMEOUT => {
                    // The newly launched process can grant its foreground right
                    // to the original process, instead of granting it globally.
                    if activate {
                        activate_existing_window();
                    }
                    request.signal()?;
                    return Ok(None);
                }
                _ => {
                    return Err(format!(
                        "无法锁定 FrameLink 运行状态：{}",
                        windows::core::Error::from_thread()
                    ));
                }
            }
            let stop = Arc::new(Event::new(PCWSTR::null(), true)?);
            Ok(Some(Self {
                _mutex: mutex,
                binding: Arc::new(Binding {
                    request,
                    stop,
                    pending: Arc::new(AtomicBool::new(false)),
                    window: AtomicIsize::new(0),
                    listener: Mutex::new(None),
                }),
            }))
        }

        pub fn activation(&self) -> ActivationRegistration {
            ActivationRegistration(self.binding.clone())
        }
    }

    impl ActivationRegistration {
        pub fn register_window(&self, cc: &eframe::CreationContext<'_>) -> Result<(), String> {
            let mut listener_slot = self.0.listener.lock().unwrap_or_else(|e| e.into_inner());
            if listener_slot.is_some() {
                return Err("主窗口已经注册".into());
            }
            let handle = cc
                .window_handle()
                .map_err(|e| format!("无法取得程序窗口：{e}"))?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                return Err("无法取得 Windows 主窗口".into());
            };
            let native = handle.hwnd.get();
            let window = HWND(native as *mut std::ffi::c_void);
            unsafe {
                SetPropW(
                    window,
                    WINDOW_PROPERTY,
                    Some(HANDLE(std::process::id() as usize as *mut _)),
                )
            }
            .map_err(|e| format!("无法注册窗口唤醒标识：{e}"))?;
            self.0.window.store(native, Ordering::Release);
            cc.egui_ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new(ACTIVATION_KEY), self.0.pending.clone());
            });
            let request = self.0.request.clone();
            let stop = self.0.stop.clone();
            let pending = self.0.pending.clone();
            let ctx = cc.egui_ctx.clone();
            let listener = std::thread::Builder::new()
                .name("framelink-window-activation".into())
                .spawn(move || {
                    let handles = [stop.0, request.0];
                    loop {
                        let result = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                        if result == WAIT_OBJECT_0 {
                            break;
                        }
                        if result.0 != WAIT_OBJECT_0.0 + 1 {
                            break;
                        }
                        pending.store(true, Ordering::Release);
                        // Never synchronously wait for the UI thread from here:
                        // main-thread shutdown joins this listener.
                        let window = HWND(native as *mut std::ffi::c_void);
                        unsafe {
                            show_async(window);
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.request_repaint();
                    }
                })
                .map_err(|e| format!("无法启动窗口唤醒监听：{e}"))?;
            *listener_slot = Some(listener);
            Ok(())
        }
    }

    impl Drop for PrimaryInstance {
        fn drop(&mut self) {
            let _ = self.binding.stop.signal();
            if let Some(listener) = self
                .binding
                .listener
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                let _ = listener.join();
            }
            let window = HWND(self.binding.window.load(Ordering::Acquire) as *mut std::ffi::c_void);
            unsafe {
                if IsWindow(Some(window)).as_bool() {
                    let _ = RemovePropW(window, WINDOW_PROPERTY);
                }
            }
            // The owned mutex is released only after the activation listener
            // and eframe/sender have finished; event handles outlive the waiter.
        }
    }

    pub fn take_activation(ctx: &Context) -> bool {
        ctx.data(|data| data.get_temp::<Arc<AtomicBool>>(egui::Id::new(ACTIVATION_KEY)))
            .is_some_and(|pending| pending.swap(false, Ordering::AcqRel))
    }

    unsafe fn show_async(window: HWND) {
        unsafe {
            if IsWindow(Some(window)).as_bool() {
                let mode = if IsIconic(window).as_bool() {
                    SW_RESTORE
                } else {
                    SW_SHOW
                };
                let _ = ShowWindowAsync(window, mode);
            }
        }
    }

    fn activate_existing_window() {
        unsafe extern "system" fn find(window: HWND, parameter: LPARAM) -> BOOL {
            if !unsafe { GetPropW(window, WINDOW_PROPERTY) }.is_invalid() {
                unsafe {
                    *(parameter.0 as *mut isize) = window.0 as isize;
                }
                return false.into();
            }
            true.into()
        }
        let mut found = 0isize;
        unsafe {
            let _ = EnumWindows(Some(find), LPARAM(&mut found as *mut isize as isize));
            if found != 0 {
                let window = HWND(found as *mut std::ffi::c_void);
                let mut process = 0;
                GetWindowThreadProcessId(window, Some(&mut process));
                if process != 0 {
                    let _ = AllowSetForegroundWindow(process);
                }
                show_async(window);
                let _ = SetForegroundWindow(window);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::AtomicU64;
        static NEXT: AtomicU64 = AtomicU64::new(0);

        fn names() -> (Vec<u16>, Vec<u16>) {
            let base = format!(
                "Local\\FrameLink.Test.{}.{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let wide = |suffix| {
                format!("{base}.{suffix}")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect()
            };
            (wide("Mutex"), wide("Event"))
        }

        #[test]
        fn duplicate_launch_is_rejected_and_early_activation_is_retained() {
            let (mutex, event) = names();
            let primary = PrimaryInstance::acquire_named(
                PCWSTR(mutex.as_ptr()),
                PCWSTR(event.as_ptr()),
                false,
            )
            .unwrap()
            .unwrap();
            let other_mutex = mutex.clone();
            let other_event = event.clone();
            assert!(
                std::thread::spawn(move || {
                    PrimaryInstance::acquire_named(
                        PCWSTR(other_mutex.as_ptr()),
                        PCWSTR(other_event.as_ptr()),
                        false,
                    )
                    .unwrap()
                    .is_none()
                })
                .join()
                .unwrap()
            );
            // No window/listener has been registered yet, as during first launch.
            assert_eq!(
                unsafe { WaitForSingleObject(primary.binding.request.0, 1000) },
                WAIT_OBJECT_0
            );
            assert_eq!(
                unsafe { WaitForSingleObject(primary.binding.request.0, 0) },
                WAIT_TIMEOUT
            );
            drop(primary);
            assert!(
                PrimaryInstance::acquire_named(
                    PCWSTR(mutex.as_ptr()),
                    PCWSTR(event.as_ptr()),
                    false
                )
                .unwrap()
                .is_some()
            );
        }

        #[test]
        fn abandoned_mutex_can_be_claimed_after_the_owner_exits() {
            let (mutex, event) = names();
            let old_name = mutex.clone();
            // Keep an open handle, but let its owning thread terminate, matching
            // the abandoned-owner case after a crash while another handle exists.
            let old_handle = std::thread::spawn(move || unsafe {
                let handle = CreateMutexW(None, false, PCWSTR(old_name.as_ptr())).unwrap();
                assert_eq!(WaitForSingleObject(handle, 0), WAIT_OBJECT_0);
                handle.0 as isize
            })
            .join()
            .unwrap();
            let primary = PrimaryInstance::acquire_named(
                PCWSTR(mutex.as_ptr()),
                PCWSTR(event.as_ptr()),
                false,
            )
            .unwrap();
            assert!(primary.is_some());
            drop(primary);
            unsafe {
                let _ = CloseHandle(HANDLE(old_handle as *mut _));
            }
        }
    }
}

#[cfg(windows)]
pub use implementation::{ActivationRegistration, PrimaryInstance, take_activation};
