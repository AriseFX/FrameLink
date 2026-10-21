#!/usr/bin/env python3
"""Build and package the Rust desktop sender with an installed Rust toolchain."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import zipfile


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="x86_64-pc-windows-msvc" if platform.system() == "Windows" else "x86_64-pc-windows-gnu")
    parser.add_argument("--skip-build", action="store_true", help="Package a previously validated release build")
    args = parser.parse_args()
    if args.target not in ("x86_64-pc-windows-msvc", "x86_64-pc-windows-gnu"):
        parser.error("Only Windows x64 targets are supported")
    if not args.skip_build:
        run("cargo", "build", "--locked", "--release", "--target", args.target, "--bin", "FrameLink", cwd=root)
    executable = root / "target" / args.target / "release/FrameLink.exe"
    if not executable.is_file():
        parser.error(f"Missing executable: {executable}")
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=root, text=True))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "framelink" and not p.get("source"))
    package = root / f"dist/FrameLink-{version}-Windows-x64"
    package_files = ((executable, "FrameLink.exe"),
                     (root / "Processing.NDI.Lib.x64.dll", "Processing.NDI.Lib.x64.dll"),
                     (root / "NDI License Agreement.pdf", "NDI License Agreement.pdf"),
                     (root / "Processing.NDI.Lib.Licenses.txt", "Processing.NDI.Lib.Licenses.txt"),
                     (root / "README.md", "README.md"),
                     (root / "docs/开发说明.md", "docs/开发说明.md"),
                     (root / "tests/SingleInstanceVerification.ps1", "tests/SingleInstanceVerification.ps1"))
    for source, _ in package_files:
        if not source.is_file():
            parser.error(f"Missing package input: {source}")
    if package.exists():
        shutil.rmtree(package)
    package.mkdir(parents=True)
    for source, name in package_files:
        (package / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, package / name)
    if (root / "docs/ui").exists():
        shutil.copytree(root / "docs/ui", package / "docs/ui", dirs_exist_ok=True)
    licenses = package / "third-party-licenses"
    (licenses / "libyuv").mkdir(parents=True, exist_ok=True)
    for name in ("LICENSE", "PATENTS", "AUTHORS"):
        shutil.copy2(root / "third_party/libyuv" / name, licenses / "libyuv" / name)
    inventory = []
    for dependency in metadata["packages"]:
        if not dependency.get("source"):
            continue
        directory = Path(dependency["manifest_path"]).parent
        inventory.append({"name": dependency["name"], "version": dependency["version"], "license": dependency.get("license"), "repository": dependency.get("repository")})
        # Preserve the original notices, including build dependencies. The
        # inventory is deliberately broader than the final linked runtime.
        notices = [p for p in directory.iterdir() if p.is_file() and p.name.lower().startswith(("license", "licence", "copying", "notice", "copyright", "unlicense"))]
        for notice in notices:
            dest = licenses / f"{dependency['name']}-{dependency['version']}"
            dest.mkdir(parents=True, exist_ok=True)
            shutil.copy2(notice, dest / notice.name)
    (licenses / "rust-dependencies.json").write_text(json.dumps(inventory, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    source_files = sorted([*root.glob("rust/**/*.rs"), root / "Cargo.toml", root / "Cargo.lock", root / ".cargo/config.toml", *root.glob("resources/*"), root / "tools/build_rust.py", root / "tests/SingleInstanceVerification.ps1"])
    info = {"product": "FrameLink 帧连", "version": version, "target": args.target, "build_host": platform.platform(),
            "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
            "executable_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
            "source_sha256": {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in source_files if p.is_file()},
            "validation": {"cross_compiled": True, "windows_runtime": "not_run", "single_instance_windows_runtime": "not_run", "dual_machine_latency": "not_measured"}}
    (package / "build-info.json").write_text(json.dumps(info, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    (package / "使用说明.txt").write_text(f"""FrameLink 帧连 {version} 开发测试版

解压整个文件夹，启动 FrameLink.exe，保留同目录 NDI DLL。
适用于 Windows 10 / 11 x64。NDI DLL 可能需要 VC++ 2015–2022 x64 运行库。
默认中心512x512、240fps、关闭鼠标与预览；可选择640或在屏幕上框选。
关闭窗口默认收到托盘并继续发送，右键托盘可停止或退出。
再次双击 FrameLink.exe 或快捷方式会恢复并激活已有窗口，保留原发送任务和设置。
升级到本版前请先退出旧版；单实例窗口恢复适用于原进程所在的交互桌面。
首次启动优先导入上一版 Rust 设置，兼容旧 C++ INI，原文件保留。
新设置保存到 %LOCALAPPDATA%\\FrameLink\\settings.toml。

已通过 Windows GNU 交叉编译和本机测试；界面截图见 docs/ui。
Windows 实机运行、240fps稳态和双机延迟尚未验收。
单实例验收脚本：tests/SingleInstanceVerification.ps1（尚未实机运行）。
开发与验收说明：docs/开发说明.md。
""", encoding="utf-8")
    archive = package.parent / f"{package.name}.zip"
    temporary_archive=archive.with_suffix(".zip.tmp")
    with zipfile.ZipFile(temporary_archive, "w", zipfile.ZIP_DEFLATED, compresslevel=6, strict_timestamps=False) as output:
        for path in sorted(package.rglob("*")):
            if path.is_file():
                output.write(path, path.relative_to(package.parent).as_posix())
    with zipfile.ZipFile(temporary_archive) as output:
        if output.testzip() is not None:
            raise RuntimeError("Package verification failed")
    temporary_archive.replace(archive)
    print(f"Package: {archive} ({archive.stat().st_size:,} bytes)")
    print(f"SHA256: {hashlib.sha256(archive.read_bytes()).hexdigest()}")


if __name__ == "__main__":
    main()
