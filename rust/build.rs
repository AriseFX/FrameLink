use std::{env, fs, path::PathBuf};
mod resource_build;

fn main() {
    println!("cargo:rerun-if-changed=rust/build.rs");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if os != "windows" {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        panic!("FrameLink requires Windows x86_64");
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let yuv = root.join("third_party/libyuv");
    println!("cargo:rerun-if-changed={}", yuv.display());
    let mut sources: Vec<_> = fs::read_dir(yuv.join("source"))
        .expect("vendored libyuv sources")
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "cc"))
        .filter(|p| {
            let excluded = &["_neon", "_neon64", "_sve", "_sme", "_lsx", "_lasx"];
            !excluded
                .iter()
                .any(|name| p.file_name().unwrap().to_string_lossy().contains(name))
        })
        .collect();
    sources.sort();
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
    } else {
        build.flag("-std=c++17");
        // Only libyuv remains C++; ship no MinGW libstdc++ DLL dependency.
        build.cpp_link_stdlib("stdc++").cpp_link_stdlib_static(true);
        println!("cargo:rustc-link-arg=-static-libgcc");
    }
    build.compile("framelink_yuv");
    resource_build::compile(&root);
}
