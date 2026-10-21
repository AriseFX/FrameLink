use std::{env, path::Path, process::Command};

pub fn compile(root: &Path) {
    let resources = root.join("resources");
    println!("cargo:rerun-if-changed={}", resources.display());
    let target = env::var("TARGET").unwrap();
    let out = std::path::PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = if target.ends_with("msvc") {
        let file = out.join("framelink.res");
        let mut command = cc::windows_registry::find_tool(&target, "rc.exe")
            .map(|tool| tool.to_command())
            .unwrap_or_else(|| Command::new("rc.exe"));
        let result = command
            .arg("/nologo")
            .arg("/I")
            .arg(&resources)
            .arg("/fo")
            .arg(&file)
            .arg(resources.join("rust.rc"))
            .status()
            .expect("Windows SDK resource compiler unavailable");
        assert!(result.success(), "Resource compilation failed");
        file
    } else {
        let file = out.join("framelink-resource.o");
        let compiler = env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
        let result = Command::new(compiler)
            .arg("-I")
            .arg(&resources)
            .arg(resources.join("rust.rc"))
            .arg(&file)
            .status()
            .expect("MinGW-w64 windres unavailable");
        assert!(result.success(), "Resource compilation failed");
        file
    };
    println!("cargo:rustc-link-arg-bin=FrameLink={}", object.display());
}
