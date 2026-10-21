use std::{env, fs, path::PathBuf};
mod resource_build;

fn main() {
    println!("cargo:rerun-if-changed=rust/build.rs");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if os != "windows" && os != "macos" {
        return;
    }
    if env::var("CARGO_CFG_TARGET_POINTER_WIDTH").as_deref() != Ok("64") {
        panic!("The NDI runtime and ABI require a 64-bit target");
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let yuv = root.join("third_party/libyuv");
    println!("cargo:rerun-if-changed={}", yuv.display());
    let mut sources: Vec<_> = fs::read_dir(yuv.join("source"))
        .expect("vendored libyuv sources")
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "cc"))
        .filter(|p| {
            let excluded: &[&str] = if os == "windows" {
                &["_neon", "_neon64", "_sve", "_sme", "_lsx", "_lasx"]
            } else {
                &["_sve", "_sme", "_lsx", "_lasx"]
            };
            !excluded
                .iter()
                .any(|name| p.file_name().unwrap().to_string_lossy().contains(name))
        })
        .collect();
    sources.sort();
    if os == "macos" && env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        // Upstream embeds I8MM assembly in the baseline NEON translation unit.
        // Enable those instructions only for its runtime-dispatched I8MM
        // functions; a global -march would raise the CPU floor for every Mac.
        let original = yuv.join("source/row_neon64.cc");
        let mut source = fs::read_to_string(&original).expect("libyuv NEON source");
        for name in [
            "ARGBToUV444MatrixRow_NEON_I8MM",
            "ARGBToUVMatrixRow_NEON_I8MM_Impl",
            "ARGBToUVMatrixRow_NEON_I8MM",
            "ARGBColorMatrixRow_NEON_I8MM",
        ] {
            let signature = format!("void {name}(");
            assert_eq!(
                source.matches(&signature).count(),
                1,
                "libyuv I8MM signature changed"
            );
            source = source.replace(
                &signature,
                &format!("__attribute__((target(\"i8mm\"))) {signature}"),
            );
        }
        let generated = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("row_neon64.cc");
        fs::write(&generated, source).expect("libyuv NEON target annotations");
        let entry = sources.iter_mut().find(|path| **path == original).unwrap();
        *entry = generated;
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(yuv.join("include"))
        .files(sources)
        .define("NDEBUG", None)
        .define("NOMINMAX", None)
        .define("LIBYUV_DISABLE_SVE", None)
        .define("LIBYUV_DISABLE_SME", None)
        .opt_level(3)
        .warnings(false);
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag("/utf-8");
    } else if os == "windows" {
        build.flag("-std=c++17");
        // Only libyuv remains C++; ship no MinGW libstdc++ DLL dependency.
        build.cpp_link_stdlib("stdc++").cpp_link_stdlib_static(true);
        println!("cargo:rustc-link-arg=-static-libgcc");
    } else {
        build.flag("-std=c++17");
    }
    build.compile("framelink_yuv");
    if os == "windows" {
        resource_build::compile(&root);
    } else {
        // Resolve the active Swift toolchain for both full Xcode and CLT-only
        // installs. The ScreenCaptureKit bridge needs its compatibility archives.
        let swift = std::process::Command::new("xcrun")
            .args(["--find", "swift"])
            .output()
            .expect("Xcode command-line tools with Swift are required");
        assert!(
            swift.status.success(),
            "Unable to locate the Swift compiler"
        );
        let swift = PathBuf::from(String::from_utf8(swift.stdout).unwrap().trim());
        let libraries = swift
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("lib/swift/macosx");
        if libraries.is_dir() {
            println!("cargo:rustc-link-search=native={}", libraries.display());
        }
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
