#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[cfg(all(not(windows), not(test)))]
compile_error!(
    "FrameLink is a Windows x64 application. Build with --target x86_64-pc-windows-msvc or x86_64-pc-windows-gnu; host-only checks use cargo test --lib."
);
use framelink::{brand::APP_NAME, snapshot, ui::DesktopApp};

fn main() {
    if let Err(error) = run() {
        eprintln!("{APP_NAME}: {error}");
        #[cfg(windows)]
        unsafe {
            use windows::{
                Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
                core::PCWSTR,
            };
            let text: Vec<_> = error.encode_utf16().chain(Some(0)).collect();
            let title: Vec<_> = format!("{APP_NAME} 启动失败")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|value| value == "--measure-send") {
        if args.len() < 3 {
            return Err("用法：FrameLink --measure-send <秒数1–60> <报告.json> [--fps 240 | --follow-refresh] [--pixel nv12|bgrx]".into());
        }
        #[cfg(windows)]
        {
            let options = framelink::diagnostics::SendOptions::parse(&args[3..])?;
            let _instance = framelink::single_instance::PrimaryInstance::acquire()?
                .ok_or("请先退出已运行的 FrameLink，再开始发送诊断")?;
            framelink::network::initialize(&framelink::settings::load().0.network_adapter);
            let seconds = args[1].parse::<f64>().map_err(|_| "诊断运行时间需为数字")?;
            return framelink::diagnostics::measure_send_with_options(
                seconds,
                std::path::Path::new(&args[2]),
                &options,
            );
        }
        #[cfg(not(windows))]
        return Err("发送诊断需要在 Windows 发送端运行".into());
    }
    if args.first().is_some_and(|value| value == "--check-ndi") {
        if args.len() != 1 {
            return Err("用法：FrameLink --check-ndi".into());
        }
        #[cfg(windows)]
        {
            #[cfg(windows)]
            let _instance = framelink::single_instance::PrimaryInstance::acquire()?
                .ok_or("请先退出已运行的 FrameLink，再检查 NDI 运行库")?;
            #[cfg(windows)]
            framelink::network::initialize(&framelink::settings::load().0.network_adapter);
            let ndi = framelink::ndi::Ndi::load()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "runtime_path": ndi.runtime_path(),
                    "runtime_version": ndi.version(),
                    "initialized": true,
                    "scope": "runtime_initialization_only",
                    "screen_capture": false,
                    "video_sent": false,
                }))
                .map_err(|error| error.to_string())?
            );
            return Ok(());
        }
        #[cfg(not(windows))]
        return Err("当前平台不支持NDI运行库检查".into());
    }
    if args
        .first()
        .is_some_and(|value| value == "--benchmark-pixels")
    {
        if args.len() > 2 {
            return Err("用法：FrameLink --benchmark-pixels [每项秒数，默认2]".into());
        }
        let seconds = args
            .get(1)
            .map_or(Ok(2.0), |value| value.parse::<f64>())
            .map_err(|_| "基准运行时间需为数字")?;
        return framelink::benchmark::pixels(seconds);
    }
    if args.first().is_some_and(|v| v == "--ui-snapshot") {
        let folder = std::path::PathBuf::from(args.get(1).map(String::as_str).unwrap_or("docs/ui"));
        snapshot::write_gallery(&folder)?;
        println!("UI snapshots: {}", folder.display());
        return Ok(());
    }
    #[cfg(windows)]
    let instance = match framelink::single_instance::PrimaryInstance::acquire()? {
        Some(instance) => instance,
        None => return Ok(()),
    };
    #[cfg(windows)]
    framelink::network::initialize(&framelink::settings::load().0.network_adapter);
    let icon = eframe::icon_data::from_png_bytes(&icon_png()?).map_err(|e| e.to_string())?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1200.0, 820.0])
            .with_min_inner_size([960.0, 700.0])
            .with_icon(icon),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        ..Default::default()
    };
    #[cfg(windows)]
    let activation = instance.activation();
    let result = eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            framelink::tray::register_window(cc);
            #[cfg(windows)]
            activation
                .register_window(cc)
                .map_err(std::io::Error::other)?;
            Ok(Box::new(DesktopApp::new(cc)))
        }),
    )
    .map_err(|e| e.to_string());
    #[cfg(windows)]
    {
        drop(instance);
        if result.is_ok() && framelink::network::restart_requested() {
            framelink::network::restart()?;
        }
    }
    result
}
fn icon_png() -> Result<Vec<u8>, String> {
    use std::io::Cursor;
    let icon = image::load_from_memory(include_bytes!("../resources/app.ico"))
        .map_err(|e| e.to_string())?;
    let mut bytes = Cursor::new(Vec::new());
    icon.write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(bytes.into_inner())
}
