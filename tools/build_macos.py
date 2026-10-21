#!/usr/bin/env python3
"""Build a macOS FrameLink.app and ZIP with the installed Rust and Xcode tools."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import tempfile
import zipfile


TARGETS = {
    "aarch64-apple-darwin": ("arm64", "arm64"),
    "x86_64-apple-darwin": ("x86_64", "x64"),
}


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def output(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def check_architecture(path, architecture):
    available = output("lipo", "-archs", str(path)).split()
    if architecture not in available:
        raise ValueError(
            f"{path.name} does not contain {architecture}; found {', '.join(available)}"
        )
    return available


def ndi_dependencies(path, architecture):
    """Require the redistributable runtime to be self-contained apart from macOS."""
    install_names = output("otool", "-arch", architecture, "-D", str(path)).splitlines()[1:]
    if len(install_names) != 1:
        raise ValueError(f"{path.name} is not a dynamic library with a valid install name")
    install_name = install_names[0].strip()
    lines = output("otool", "-arch", architecture, "-L", str(path)).splitlines()[1:]
    dependencies = []
    for line in lines:
        dependency = line.strip().split(" (compatibility version", 1)[0]
        if dependency and dependency != install_name:
            dependencies.append(dependency)
    for dependency in dependencies:
        # Libraries in the shared dyld cache need not exist as separate files.
        if dependency.startswith(("/usr/lib/", "/System/Library/")):
            continue
        raise ValueError(
            f"NDI runtime has a non-system dependency: {dependency}. "
            "Use a self-contained redistributable libndi.dylib; additional dylibs "
            "are not copied implicitly."
        )
    return dependencies


def make_icon(source, destination, work):
    base = work / "icon-source.png"
    run("sips", "-s", "format", "png", str(source), "--out", str(base), stdout=subprocess.DEVNULL)
    iconset = work / "app.iconset"
    iconset.mkdir()
    for size in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            suffix = "@2x" if scale == 2 else ""
            target = iconset / f"icon_{size}x{size}{suffix}.png"
            pixels = str(size * scale)
            run("sips", "-z", pixels, pixels, str(base), "--out", str(target), stdout=subprocess.DEVNULL)
    run("iconutil", "-c", "icns", str(iconset), "-o", str(destination))


def copy_notices(root, package, metadata):
    for name in ("README.md", "NDI License Agreement.pdf", "Processing.NDI.Lib.Licenses.txt"):
        shutil.copy2(root / name, package / name)
    (package / "docs").mkdir()
    shutil.copy2(root / "docs/开发说明.md", package / "docs/开发说明.md")
    shutil.copytree(root / "docs/ui", package / "docs/ui")
    licenses = package / "third-party-licenses"
    (licenses / "libyuv").mkdir(parents=True)
    for name in ("LICENSE", "PATENTS", "AUTHORS"):
        shutil.copy2(root / "third_party/libyuv" / name, licenses / "libyuv" / name)
    inventory = []
    for dependency in metadata["packages"]:
        if not dependency.get("source"):
            continue
        inventory.append({
            "name": dependency["name"],
            "version": dependency["version"],
            "license": dependency.get("license"),
            "repository": dependency.get("repository"),
        })
        directory = Path(dependency["manifest_path"]).parent
        for notice in directory.iterdir():
            if notice.is_file() and notice.name.lower().startswith(
                ("license", "licence", "copying", "notice", "copyright", "unlicense")
            ):
                destination = licenses / f"{dependency['name']}-{dependency['version']}"
                destination.mkdir(exist_ok=True)
                shutil.copy2(notice, destination / notice.name)
    (licenses / "rust-dependencies.json").write_text(
        json.dumps(inventory, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )


def main():
    root = Path(__file__).resolve().parents[1]
    default_target = "aarch64-apple-darwin" if platform.machine() == "arm64" else "x86_64-apple-darwin"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, default=default_target)
    parser.add_argument("--skip-build", action="store_true", help="Package an existing release build for the selected target")
    parser.add_argument("--ndi-runtime", type=Path, help="Absolute path to a redistributable libndi.dylib (optional)")
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("macOS packaging requires macOS and the Xcode command-line tools")
    for command in ("cargo", "rustc", "sips", "iconutil", "lipo", "otool", "codesign"):
        if shutil.which(command) is None:
            parser.error(f"Required tool is unavailable: {command}")
    architecture, label = TARGETS[args.target]
    runtime = args.ndi_runtime
    if runtime is not None:
        if not runtime.is_absolute() or not runtime.is_file():
            parser.error("--ndi-runtime must name an existing dylib by its absolute path")
        runtime = runtime.resolve()
        check_architecture(runtime, architecture)
        dependencies = ndi_dependencies(runtime, architecture)
    else:
        dependencies = []
    if not args.skip_build:
        run("cargo", "build", "--locked", "--release", "--target", args.target, "--bin", "FrameLink", cwd=root)
    executable = root / "target" / args.target / "release/FrameLink"
    if not executable.is_file():
        parser.error(f"Missing release build: {executable}")
    check_architecture(executable, architecture)
    source_files = sorted({
        *[path for path in (root / "rust").rglob("*") if path.is_file()],
        root / "Cargo.toml", root / "Cargo.lock", root / ".cargo/config.toml",
        *[path for path in (root / "resources").rglob("*") if path.is_file()],
        Path(__file__).resolve(),
    })
    if args.skip_build:
        newer = [path.relative_to(root).as_posix() for path in source_files
                 if path.is_file() and path.suffix in (".rs", ".swift", ".toml", ".lock")
                 and path.stat().st_mtime_ns > executable.stat().st_mtime_ns]
        if newer:
            parser.error("Release build is older than source inputs; rebuild without --skip-build")
    metadata = json.loads(output("cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", args.target, cwd=root))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "framelink" and not p.get("source"))
    bundle_version = version.split("-", 1)[0].split("+", 1)[0]
    if re.fullmatch(r"\d+\.\d+\.\d+", bundle_version) is None:
        parser.error(f"Cannot convert Cargo version to a macOS bundle version: {version}")
    dist = root / "dist"
    dist.mkdir(exist_ok=True)
    package_name = f"FrameLink-{version}-macOS-{label}"
    package = dist / package_name
    archive = dist / f"{package_name}.zip"
    with tempfile.TemporaryDirectory(prefix=".framelink-macos-", dir=dist) as temporary:
        work = Path(temporary)
        staging = work / package_name
        app = staging / "FrameLink.app"
        contents = app / "Contents"
        macos = contents / "MacOS"
        resources = contents / "Resources"
        macos.mkdir(parents=True)
        resources.mkdir()
        bundled_executable = macos / "FrameLink"
        shutil.copy2(executable, bundled_executable)
        bundled_executable.chmod(0o755)
        make_icon(root / "resources/app.ico", resources / "app.icns", work)
        info = {
            "CFBundleIdentifier": "com.framelink.app",
            "CFBundleName": "FrameLink",
            "CFBundleDisplayName": "FrameLink 帧连",
            "CFBundleExecutable": "FrameLink",
            "CFBundlePackageType": "APPL",
            "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleShortVersionString": bundle_version,
            "CFBundleVersion": bundle_version,
            "CFBundleIconFile": "app.icns",
            "CFBundleDevelopmentRegion": "zh_CN",
            "LSMinimumSystemVersion": "13.0",
            "LSApplicationCategoryType": "public.app-category.video",
            "LSUIElement": False,
            "NSHighResolutionCapable": True,
        }
        with (contents / "Info.plist").open("wb") as destination:
            plistlib.dump(info, destination)
        if runtime is not None:
            frameworks = contents / "Frameworks"
            frameworks.mkdir()
            bundled_runtime = frameworks / "libndi.dylib"
            shutil.copy2(runtime, bundled_runtime)
            run("codesign", "--force", "--sign", "-", "--timestamp=none", str(bundled_runtime))
        run("codesign", "--force", "--sign", "-", "--timestamp=none", str(app))
        run("codesign", "--verify", "--deep", "--strict", str(app))
        copy_notices(root, staging, metadata)
        build_info = {
            "product": "FrameLink 帧连", "version": version, "target": args.target,
            "build_host": platform.platform(),
            "packaged_at_utc": datetime.now(timezone.utc).isoformat(),
            "rustc": output("rustc", "--version"),
            "release_build": "existing" if args.skip_build else "cargo_build",
            "executable_sha256": sha256(bundled_executable),
            "source_sha256": {path.relative_to(root).as_posix(): sha256(path)
                              for path in source_files if path.is_file()},
            "ndi_runtime": {
                "bundled": runtime is not None,
                "source_sha256": sha256(runtime) if runtime is not None else None,
                "dependencies": dependencies,
            },
            "validation": {
                "executable_architecture": architecture,
                "bundle_signature": "ad_hoc_verified",
                "notarized": False,
                "gui_runtime": "not_run",
                "screen_capture": "not_run",
                "ndi_send": "not_run",
                "dual_machine_latency": "not_measured",
            },
        }
        (staging / "build-info.json").write_text(
            json.dumps(build_info, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        runtime_note = (
            "包内已包含指定的 NDI 运行库。"
            if runtime is not None else
            "包内未包含 NDI 运行库。发送视频前，需安装对应架构的 NDI Runtime，\n"
            "或构建时通过 --ndi-runtime 指定可分发的 libndi.dylib。"
        )
        (staging / "使用说明.txt").write_text(
            f"FrameLink 帧连 {version}\n\n"
            "适用于 macOS 13 或更高版本。将 FrameLink.app 放到应用程序文件夹后打开。\n"
            "屏幕采集需在系统设置的隐私与安全性中，允许 FrameLink 录制屏幕，随后重新打开应用。\n"
            "区域尺寸和坐标使用屏幕物理像素；可选居中预设或手动输入。\n"
            "采集方式为 ScreenCaptureKit；帧率设置是上限，实际帧率取决于屏幕更新。\n"
            "关闭窗口会停止发送并退出。\n\n"
            "无需 NDI 的转换基准：在解压目录的终端运行\n"
            "./FrameLink.app/Contents/MacOS/FrameLink --benchmark-pixels 2\n"
            "每项运行2秒，输出JSON；结果不包含采集、网络或端到端延迟。\n\n"
            f"{runtime_note}\n\n"
            "此包使用本地临时签名，未经过 Apple 公证。\n"
            "打包检查不代表已验证屏幕采集、NDI 接收或双机延迟；状态见 build-info.json。\n"
            "详细构建和测试说明见 README.md 与 docs。\n",
            encoding="utf-8",
        )
        temporary_archive = work / f"{package_name}.zip"
        # The generated bundle has ordinary files only. zipfile retains Unix
        # executable modes and marks Chinese filenames as UTF-8 explicitly.
        with zipfile.ZipFile(temporary_archive, "w", zipfile.ZIP_DEFLATED, compresslevel=6, strict_timestamps=False) as result:
            for path in sorted(staging.rglob("*")):
                if path.is_file():
                    result.write(path, path.relative_to(work).as_posix())
        with zipfile.ZipFile(temporary_archive) as result:
            if result.testzip() is not None:
                raise RuntimeError("ZIP verification failed")
            executable_entry = result.getinfo(f"{package_name}/FrameLink.app/Contents/MacOS/FrameLink")
            if not (executable_entry.external_attr >> 16) & 0o111:
                raise RuntimeError("ZIP did not preserve executable permissions")
        if package.exists():
            shutil.rmtree(package)
        staging.replace(package)
        temporary_archive.replace(archive)
    print(f"Application: {package / 'FrameLink.app'}")
    print(f"Package: {archive} ({archive.stat().st_size:,} bytes)")
    print(f"SHA256: {sha256(archive)}")
    if runtime is None:
        print("NDI runtime not bundled. Sending requires a compatible NDI Runtime.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"macOS packaging failed: {error}") from error
