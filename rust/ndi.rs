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
#[cfg(any(windows, target_os = "macos", test))]
struct SendSettings {
    name: *const c_char,
    groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}
#[repr(C)]
#[cfg(any(windows, target_os = "macos", test))]
struct Source {
    name: *const c_char,
    address: *const c_char,
}

#[cfg(any(windows, target_os = "macos"))]
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

    #[cfg(target_os = "macos")]
    fn load_library() -> Result<(Library, std::path::PathBuf), String> {
        let exe = std::env::current_exe().map_err(|e| format!("无法获取程序路径：{e}"))?;
        let override_path = std::env::var_os("FRAMELINK_NDI_RUNTIME");
        let candidates = super::macos_runtime_paths(&exe, override_path.as_deref())?;
        let mut failures = Vec::with_capacity(candidates.len());
        for path in candidates {
            // All candidates are absolute. Never search the working directory or
            // depend on DYLD_LIBRARY_PATH when launched from Finder.
            match unsafe { Library::new(&path) } {
                Ok(library) => return Ok((library, path)),
                Err(error) => failures.push(format!("{}：{error}", path.display())),
            }
        }
        Err(format!(
            "无法加载 macOS NDI 运行库。请重新解压包含运行库的完整 FrameLink 包；开发环境也可用 FRAMELINK_NDI_RUNTIME 指定绝对路径。\n{}",
            failures.join("\n")
        ))
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

#[cfg(any(windows, target_os = "macos"))]
pub use runtime::{Ndi, Sender};

#[cfg(target_os = "macos")]
fn macos_runtime_paths(
    exe: &std::path::Path,
    override_path: Option<&std::ffi::OsStr>,
) -> Result<Vec<std::path::PathBuf>, String> {
    use std::path::PathBuf;

    if let Some(override_path) = override_path {
        let path = PathBuf::from(override_path);
        if !path.is_absolute() {
            return Err("FRAMELINK_NDI_RUNTIME 必须是 libndi.dylib 的绝对路径".into());
        }
        // An explicit override should report its own failure instead of silently
        // loading a different runtime, which would invalidate version comparisons.
        return Ok(vec![path]);
    }
    let directory = exe.parent().ok_or("无法获取程序目录")?;
    let mut paths = Vec::with_capacity(4);
    if directory.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents) = directory.parent()
        && contents.file_name().is_some_and(|name| name == "Contents")
    {
        paths.push(contents.join("Frameworks/libndi.dylib"));
    }
    paths.push(directory.join("libndi.dylib"));
    paths.push(PathBuf::from("/usr/local/lib/libndi.dylib"));
    paths.push(PathBuf::from(
        "/Library/NDI SDK for Apple/lib/macOS/libndi.dylib",
    ));
    Ok(paths)
}

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

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_bundle_runtime_precedes_installed_libraries() {
        let paths = macos_runtime_paths(
            std::path::Path::new("/Applications/FrameLink.app/Contents/MacOS/FrameLink"),
            None,
        )
        .unwrap();
        assert_eq!(
            paths,
            [
                "/Applications/FrameLink.app/Contents/Frameworks/libndi.dylib",
                "/Applications/FrameLink.app/Contents/MacOS/libndi.dylib",
                "/usr/local/lib/libndi.dylib",
                "/Library/NDI SDK for Apple/lib/macOS/libndi.dylib",
            ]
            .map(std::path::PathBuf::from)
        );
        assert!(paths.iter().all(|path| path.is_absolute()));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_explicit_runtime_never_falls_back() {
        let exe = std::path::Path::new("/tmp/FrameLink");
        let override_path = std::ffi::OsStr::new("/opt/ndi/libndi.dylib");
        assert_eq!(
            macos_runtime_paths(exe, Some(override_path)).unwrap(),
            [std::path::PathBuf::from(override_path)]
        );
        assert!(macos_runtime_paths(exe, Some(std::ffi::OsStr::new("libndi.dylib"))).is_err());
        assert!(macos_runtime_paths(exe, Some(std::ffi::OsStr::new(""))).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_cli_runtime_stays_beside_the_executable() {
        let paths =
            macos_runtime_paths(std::path::Path::new("/work/target/release/FrameLink"), None)
                .unwrap();
        assert_eq!(
            paths.first().unwrap(),
            &std::path::PathBuf::from("/work/target/release/libndi.dylib")
        );
        assert!(
            !paths
                .iter()
                .any(|path| path.starts_with("/work/target/Frameworks"))
        );
    }
}
