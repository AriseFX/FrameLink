#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
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
    if args.first().is_some_and(|v| v == "--ui-snapshot") {
        let folder = std::path::PathBuf::from(args.get(1).map(String::as_str).unwrap_or("docs/ui"));
        snapshot::write(&folder.join("desktop-1200.png"), 1200, 820)?;
        snapshot::write(&folder.join("desktop-960.png"), 960, 740)?;
        println!("UI snapshots: {}", folder.display());
        return Ok(());
    }
    #[cfg(windows)]
    let instance = match framelink::single_instance::PrimaryInstance::acquire()? {
        Some(instance) => instance,
        None => return Ok(()),
    };
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
    drop(instance);
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
