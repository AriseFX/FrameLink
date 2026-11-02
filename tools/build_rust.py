#!/usr/bin/env python3
"""Build and package the Rust desktop sender with an installed Rust toolchain."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import struct
import zipfile


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_pe_x64(path, *, dll):
    """Reject mixed architectures and accidental text/installer package inputs."""
    data = path.read_bytes()
    if len(data) < 64 or data[:2] != b"MZ":
        raise ValueError(f"Not a PE executable: {path}")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if pe + 26 > len(data) or data[pe:pe + 4] != b"PE\0\0":
        raise ValueError(f"Invalid PE header: {path}")
    machine = struct.unpack_from("<H", data, pe + 4)[0]
    flags = struct.unpack_from("<H", data, pe + 22)[0]
    magic = struct.unpack_from("<H", data, pe + 24)[0]
    if machine != 0x8664 or magic != 0x20B or bool(flags & 0x2000) != dll:
        raise ValueError(f"Expected Windows x64 {'DLL' if dll else 'EXE'}: {path}")
    return {"machine": "AMD64", "format": "PE32+", "sha256": sha256(path)}


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="x86_64-pc-windows-msvc" if platform.system() == "Windows" else "x86_64-pc-windows-gnu")
    parser.add_argument("--output-dir", type=Path, default=root / "dist", help="Package output directory")
    parser.add_argument("--skip-build", action="store_true", help="Package a previously validated release build")
    parser.add_argument("--ndi-runtime", type=Path, default=root / "Processing.NDI.Lib.x64.dll", help="Official NDI x64 DLL; matching license files must be alongside it")
    parser.add_argument("--ndi-source-manifest", type=Path, help="Optional JSON provenance record preserved in the package")
    args = parser.parse_args()
    if args.target not in ("x86_64-pc-windows-msvc", "x86_64-pc-windows-gnu"):
        parser.error("Only Windows x64 targets are supported")
    runtime = args.ndi_runtime.expanduser().resolve()
    runtime_licenses = [runtime.parent / name for name in ("NDI License Agreement.pdf", "Processing.NDI.Lib.Licenses.txt")]
    for source in [runtime, *runtime_licenses]:
        if not source.is_file():
            parser.error(f"Missing NDI package input: {source}")
    try:
        runtime_info = validate_pe_x64(runtime, dll=True)
    except ValueError as error:
        parser.error(str(error))
    if args.ndi_source_manifest:
        try:
            provenance = json.loads(args.ndi_source_manifest.read_text(encoding="utf-8"))
            expected = provenance.get("files", {}) if isinstance(provenance, dict) else {}
            for source in [runtime, *runtime_licenses]:
                if expected.get(source.name) != sha256(source):
                    raise ValueError(f"Manifest SHA256 mismatch or missing file: {source.name}")
        except (OSError, ValueError) as error:
            parser.error(f"Invalid NDI source manifest: {error}")
    if not args.skip_build:
        run("cargo", "build", "--locked", "--release", "--target", args.target, "--bin", "FrameLink", cwd=root)
    executable = root / "target" / args.target / "release/FrameLink.exe"
    if not executable.is_file():
        parser.error(f"Missing executable: {executable}")
    try:
        executable_info = validate_pe_x64(executable, dll=False)
    except ValueError as error:
        parser.error(str(error))
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=root, text=True, encoding="utf-8"))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "framelink" and not p.get("source"))
    package = args.output_dir.expanduser().resolve() / f"FrameLink-{version}-Windows-x64"
    package_files = ((executable, "FrameLink.exe"),
                     (runtime, "Processing.NDI.Lib.x64.dll"),
                     (runtime_licenses[0], "NDI License Agreement.pdf"),
                     (runtime_licenses[1], "Processing.NDI.Lib.Licenses.txt"),
                     (root / "README.md", "README.md"),
                     (root / "docs/开发说明.md", "docs/开发说明.md"),
                     (root / "docs/传输协议.md", "docs/传输协议.md"),
                     (root / "docs/validation/jpeg-loopback-2026-11-02.json", "docs/validation/jpeg-loopback-2026-11-02.json"),
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
    if args.ndi_source_manifest:
        shutil.copy2(args.ndi_source_manifest, package / "ndi-source.json")
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
    compiler = subprocess.check_output(["rustc", "-vV"], text=True)
    host_target = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    info = {"product": "FrameLink 帧连", "version": version, "target": args.target, "build_host": platform.platform(),
            "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
            "executable_sha256": executable_info["sha256"], "executable_pe": executable_info,
            "ndi_runtime": {**runtime_info, "file": "Processing.NDI.Lib.x64.dll", "licenses": {p.name: sha256(p) for p in runtime_licenses}, "source_manifest_sha256": sha256(args.ndi_source_manifest) if args.ndi_source_manifest else None},
            "source_sha256": {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in source_files if p.is_file()},
            "validation": {"cross_compiled": host_target != args.target, "windows_runtime": "not_run", "single_instance_windows_runtime": "not_run", "dual_machine_latency": "not_measured"}}
    (package / "build-info.json").write_text(json.dumps(info, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    (package / "使用说明.txt").write_text(f"""FrameLink 帧连 {version} 开发测试版

解压整个文件夹，启动 FrameLink.exe，保留同目录 NDI DLL。
适用于 Windows 10 / 11 x64。NDI DLL 可能需要 VC++ 2015–2022 x64 运行库。
默认中心512x512、240fps、关闭鼠标与预览；可选择640或在屏幕上框选。
关闭窗口默认收到托盘并继续发送，双击托盘图标恢复，右键打开菜单（停止发送/退出）。
再次双击 FrameLink.exe 或快捷方式会恢复并激活已有窗口，保留原发送任务和设置。
升级到本版前请先退出旧版；单实例窗口恢复适用于原进程所在的交互桌面。
首次启动优先导入上一版 Rust 设置，兼容旧 C++ INI，原文件保留。
新设置保存到 %LOCALAPPDATA%\\FrameLink\\settings.toml。
霜雪手机接收：选择 TCP / UDP，填写手机 IP 和监听端口（默认7878），手机端选择相同协议。
TCP / UDP 发送 JPEG，无需 NDI DLL；质量默认75，UDP单张上限65507字节，超限请降低质量/尺寸或改TCP。
协议按所提供7.18 APK静态分析适配，尚需手机实机验证。详见 docs/传输协议.md。
多网卡电脑可在“发送网卡”指定有线网卡，点击“应用并重启”后开始发送。
“自动选择”沿用原有 NDI 设置；指定网卡不保证降低延迟。

本包构建目标：{args.target}。构建主机与文件摘要见 build-info.json；界面截图见 docs/ui。
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
