//! Minimal NDI C ABI. The runtime is loaded lazily on the sender worker.
use std::ffi::c_char;

pub const BGRX: i32 = 0x5852_4742;
pub const NV12: i32 = 0x3231_564e;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoFrame {
    pub width: i32,
    pub height: i32,
    pub fourcc: i32,
    pub rate_n: i32,
    pub rate_d: i32,
    pub aspect: f32,
    pub format: i32,
    pub timecode: i64,
    pub data: *const u8,
    pub stride: i32,
    pub metadata: *const c_char,
    pub timestamp: i64,
}

#[repr(C)]
#[cfg(any(windows, test))]
struct SendSettings {
    name: *const c_char,
    groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}
#[repr(C)]
#[cfg(any(windows, test))]
struct Source {
    name: *const c_char,
    address: *const c_char,
}

#[cfg(windows)]
mod runtime {
    use super::*;
    use libloading::Library;
    use std::{
        ffi::{CStr, CString, c_void},
        marker::PhantomData,
        ptr::NonNull,
    };

    type Initialize = unsafe extern "C" fn() -> bool;
    type Destroy = unsafe extern "C" fn();
    type Version = unsafe extern "C" fn() -> *const c_char;
    type SendCreate = unsafe extern "C" fn(*const SendSettings) -> *mut c_void;
    type SendDestroy = unsafe extern "C" fn(*mut c_void);
    type SendAsync = unsafe extern "C" fn(*mut c_void, *const VideoFrame);
    type Connections = unsafe extern "C" fn(*mut c_void, u32) -> i32;
    type SourceName = unsafe extern "C" fn(*mut c_void) -> *const Source;

    #[cfg(windows)]
    fn load_library() -> Result<(Library, std::path::PathBuf), String> {
        let exe = std::env::current_exe().map_err(|e| format!("无法获取程序路径：{e}"))?;
        let path = exe
            .parent()
            .ok_or("无法获取程序目录")?
            .join("Processing.NDI.Lib.x64.dll");
        // Keep the Windows runtime beside the executable, outside DLL search paths.
        unsafe { Library::new(&path) }
            .map(|library| (library, path.clone()))
            .map_err(|e| {
                format!(
                    "无法加载 {}。请将NDI运行库放在EXE旁，并安装VC++ x64运行库。{e}",
                    path.display()
                )
            })
    }

    pub struct Ndi {
        // Function pointers remain valid because this library outlives every sender.
        _library: Library,
        runtime_path: std::path::PathBuf,
        destroy: Destroy,
        version: Version,
        create: SendCreate,
        sender_destroy: SendDestroy,
        send: SendAsync,
        connections: Connections,
        source: SourceName,
    }
    impl Ndi {
        pub fn load() -> Result<Self, String> {
            #[cfg(windows)]
            crate::network::ensure_ready()?;
            let (library, runtime_path) = load_library()?;
            unsafe fn symbol<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, String> {
                unsafe { lib.get::<T>(name) }.map(|f| *f).map_err(|e| {
                    format!(
                        "NDI运行库缺少接口 {}：{e}",
                        String::from_utf8_lossy(&name[..name.len() - 1])
                    )
                })
            }
            // Resolve all symbols before initializing, so partial failure has no
            // initialized NDI global state to clean up.
            let (initialize, destroy, version, create, sender_destroy, send, connections, source) = unsafe {
                (
                    symbol::<Initialize>(&library, b"NDIlib_initialize\0")?,
                    symbol::<Destroy>(&library, b"NDIlib_destroy\0")?,
                    symbol::<Version>(&library, b"NDIlib_version\0")?,
                    symbol::<SendCreate>(&library, b"NDIlib_send_create\0")?,
                    symbol::<SendDestroy>(&library, b"NDIlib_send_destroy\0")?,
                    symbol::<SendAsync>(&library, b"NDIlib_send_send_video_async_v2\0")?,
                    symbol::<Connections>(&library, b"NDIlib_send_get_no_connections\0")?,
                    symbol::<SourceName>(&library, b"NDIlib_send_get_source_name\0")?,
                )
            };
            if !unsafe { initialize() } {
                return Err("NDI初始化失败，请检查CPU与NDI运行库兼容性".into());
            }
            Ok(Self {
                _library: library,
                runtime_path,
                destroy,
                version,
                create,
                sender_destroy,
                send,
                connections,
                source,
            })
        }
        pub fn version(&self) -> String {
            string(unsafe { (self.version)() })
        }
        pub fn runtime_path(&self) -> &std::path::Path {
            &self.runtime_path
        }
        pub fn create_sender(&self, name: &str) -> Result<Sender<'_>, String> {
            let name = CString::new(name).map_err(|_| "源名称含有空字符")?;
            let settings = SendSettings {
                name: name.as_ptr(),
                groups: std::ptr::null(),
                clock_video: false,
                clock_audio: false,
            };
            let handle =
                NonNull::new(unsafe { (self.create)(&settings) }).ok_or("无法创建NDI视频源")?;
            Ok(Sender {
                ndi: self,
                handle,
                _name: name,
                _worker_only: PhantomData,
            })
        }
    }
    impl Drop for Ndi {
        fn drop(&mut self) {
            unsafe { (self.destroy)() };
        }
    }

    fn string(value: *const c_char) -> String {
        if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned()
        }
    }

    /// Neither sender handles nor capture pointers cross the GUI/worker boundary.
    pub struct Sender<'a> {
        ndi: &'a Ndi,
        handle: NonNull<c_void>,
        _name: CString,
        _worker_only: PhantomData<*mut ()>,
    }
    impl Sender<'_> {
        pub fn source_name(&self) -> String {
            let source = unsafe { (self.ndi.source)(self.handle.as_ptr()) };
            if source.is_null() {
                String::new()
            } else {
                string(unsafe { (*source).name })
            }
        }
        pub fn connections(&self) -> i32 {
            unsafe { (self.ndi.connections)(self.handle.as_ptr(), 0) }
        }
        /// # Safety
        /// The frame descriptor and its pixels must remain allocated and immutable
        /// until the next call returns, flush returns, or this sender is dropped.
        pub unsafe fn submit(&mut self, frame: *const VideoFrame) {
            unsafe { (self.ndi.send)(self.handle.as_ptr(), frame) };
        }
        pub fn flush(&mut self) {
            unsafe { (self.ndi.send)(self.handle.as_ptr(), std::ptr::null()) };
        }
    }
    impl Drop for Sender<'_> {
        fn drop(&mut self) {
            self.flush();
            unsafe { (self.ndi.sender_destroy)(self.handle.as_ptr()) };
        }
    }
}

#[cfg(windows)]
pub use runtime::{Ndi, Sender};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn ndi_x64_layout_matches_the_runtime_abi() {
        assert_eq!(std::mem::size_of::<VideoFrame>(), 72);
        assert_eq!(std::mem::size_of::<SendSettings>(), 24);
        assert_eq!(std::mem::size_of::<Source>(), 16);
        assert_eq!(std::mem::offset_of!(VideoFrame, timecode), 32);
        assert_eq!(std::mem::offset_of!(VideoFrame, data), 40);
        assert_eq!(std::mem::offset_of!(VideoFrame, metadata), 56);
        assert_eq!(std::mem::offset_of!(VideoFrame, timestamp), 64);
    }
}
