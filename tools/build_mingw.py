#!/usr/bin/env python3
"""Build the Windows x64 sender with an installed MinGW-w64 toolchain.

Works on macOS/Linux (cross compilation) and Windows. Outputs are isolated from
the supplied reference executable. No compiler or package downloads are made.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cxx", default="x86_64-w64-mingw32-g++")
    parser.add_argument("--windres", default="x86_64-w64-mingw32-windres")
    parser.add_argument("--out", type=Path, default=root / "build-mingw")
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 1, 8))
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    compiler = shutil.which(args.cxx)
    if not compiler:
        parser.error(f"Compiler not found: {args.cxx}")
    target = subprocess.check_output([compiler, "-dumpmachine"], text=True).strip()
    if not target.startswith("x86_64") or "mingw" not in target:
        parser.error(f"Expected Windows x64 MinGW compiler; got {target}")
    out = args.out.resolve()
    objdir = out / "obj"
    objdir.mkdir(parents=True, exist_ok=True)
    yuv = root / "third_party/libyuv"
    sources = [root / "src/main.cpp", root / "src/selftest.cpp"]
    sources += sorted(p for p in (yuv / "source").glob("*.cc")
                      if not re.search(r"_(neon|neon64|sve|sme|lsx|lasx)", p.name))
    flags = ["-std=c++17", "-O3", "-DNDEBUG", "-DUNICODE", "-D_UNICODE", "-DNOMINMAX",
             "-I", str(yuv / "include"), "-Wall", "-Wextra",
             "-Wno-missing-field-initializers", "-Wno-misleading-indentation"]
    # A complete source/header fingerprint avoids stale inline code and compiler
    # option changes. On an unchanged tree the existing objects can be reused.
    digest = hashlib.sha256()
    digest.update(subprocess.check_output([compiler, "--version"]))
    digest.update(json.dumps(flags).encode())
    inputs = sources + sorted((root / "src").glob("*.hpp")) + sorted((yuv / "include").rglob("*.h"))
    for path in inputs:
        digest.update(str(path.relative_to(root)).encode())
        digest.update(path.read_bytes())
    fingerprint = digest.hexdigest()

    def compile_one(source):
        obj = objdir / (str(source.relative_to(root)).replace("/", "_") + ".o")
        stamp = obj.with_suffix(".stamp")
        if not obj.exists() or not stamp.exists() or stamp.read_text() != fingerprint:
            subprocess.run([compiler, *flags, "-c", str(source), "-o", str(obj)], check=True)
            stamp.write_text(fingerprint)
        return obj

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        objects = list(pool.map(compile_one, sources))
    resource = root / "resources/app.rc"
    if resource.exists():
        windres = shutil.which(args.windres)
        if not windres:
            parser.error(f"Resource compiler not found: {args.windres}")
        obj = objdir / "app.rc.o"
        subprocess.run([windres, "-I", str(resource.parent), str(resource), str(obj)], check=True)
        objects.append(obj)
    exe = out / "ScreenCastSendCpp.exe"
    subprocess.run([compiler, "-static", "-municode", "-mwindows", "-s",
                    "-o", str(exe), *map(str, objects),
                    "-ld3d11", "-ldxgi", "-luuid", "-lgdi32", "-luser32",
                    "-lcomctl32", "-lshell32", "-lole32", "-lwinmm"], check=True)
    for name in ("Processing.NDI.Lib.x64.dll", "NDI License Agreement.pdf", "Processing.NDI.Lib.Licenses.txt"):
        destination = out / name
        if destination != root / name:
            shutil.copy2(root / name, destination)
    print(f"Built {exe} ({exe.stat().st_size:,} bytes)")


if __name__ == "__main__":
    main()
