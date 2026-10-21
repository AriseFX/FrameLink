#!/usr/bin/env python3
"""Fetch the pinned official macOS NDI redistributable without installing it."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET


RUNTIME_URL = "https://ndi.link/NDIRedistV6Apple"
PACKAGE_URL = "https://downloads.ndi.tv/SDK/NDI_SDK_Mac/libNDI_for_Mac.pkg"
PACKAGE_VERSION = "6.3.2.0.260413"
PACKAGE_SHA256 = "939d73d8456dcb1b64c609cdf36d8bb365a55d4329fab040241202492b993dce"
DYLIB_SHA256 = "af53789fcb8fc1737e6827a54d7ccea09736dc759e00ef02d53f4ed64d9abbd5"
LICENSE_SHA256 = "9f18e17e95324b1907c4de45f676ce2fedbc0a5c4ff6ed451b31c3fcb1229b8d"
INSTALLER_IDENTITY = "Developer ID Installer: NewTek, Inc. (W8U66ET244)"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def require_digest(path: Path, expected: str) -> None:
    actual = sha256(path)
    if actual != expected:
        raise ValueError(
            f"SHA-256 mismatch for {path.name}: expected {expected}, found {actual}. "
            "The pinned NDI runtime must be reviewed before accepting a new download."
        )


def write_metadata(destination: Path, work: Path, license_source: Path) -> None:
    """Replace complete files only; never expose a partially downloaded runtime."""
    license_copy = work / "libndi_licenses.txt"
    shutil.copy2(license_source, license_copy)
    source = {
        "url": RUNTIME_URL,
        "package_url": PACKAGE_URL,
        "package_sha256": PACKAGE_SHA256,
        "dylib_sha256": DYLIB_SHA256,
        "version": PACKAGE_VERSION,
        "installer_identity": INSTALLER_IDENTITY,
        "licenses_source": "libNDIComponent.pkg/Payload/libndi_licenses.txt",
        "licenses_sha256": LICENSE_SHA256,
        "repository_license": "Processing.NDI.Lib.Licenses.txt",
    }
    manifest = work / "source.json"
    manifest.write_text(json.dumps(source, indent=2) + "\n", encoding="utf-8")
    os.replace(license_copy, destination / license_copy.name)
    os.replace(manifest, destination / manifest.name)


def ensure_runtime(root: Path, package: Path | None = None) -> Path:
    """Return a verified app-local libndi.dylib, fetching it when absent or invalid.

    A supplied package is verified and extracted just like the downloaded package.
    The installer is never executed and no system-wide directories are changed.
    """
    root = root.resolve()
    destination = root / "runtime/macos"
    runtime = destination / "libndi.dylib"
    repository_license = root / "Processing.NDI.Lib.Licenses.txt"
    require_digest(repository_license, LICENSE_SHA256)
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".ndi-fetch-", dir=destination) as temporary:
        work = Path(temporary)
        if package is None and runtime.is_file() and sha256(runtime) == DYLIB_SHA256:
            write_metadata(destination, work, repository_license)
            return runtime
        for command in ("pkgutil", "curl") if package is None else ("pkgutil",):
            if shutil.which(command) is None:
                raise RuntimeError(f"Required macOS runtime preparation tool is unavailable: {command}")
        if package is None:
            package = work / "libNDI_for_Mac.pkg"
            subprocess.run([
                "curl", "--fail", "--location", "--proto", "=https",
                "--proto-redir", "=https", "--connect-timeout", "20",
                "--max-time", "180", "--retry", "3", "--retry-delay", "2",
                "--output", str(package), RUNTIME_URL,
            ], check=True)
        else:
            package = package.expanduser().resolve()
            if not package.is_file():
                raise FileNotFoundError(f"NDI package does not exist: {package}")
        require_digest(package, PACKAGE_SHA256)
        signature = subprocess.run(
            ["pkgutil", "--check-signature", str(package)],
            check=True, capture_output=True, text=True,
        ).stdout
        if INSTALLER_IDENTITY not in signature:
            raise ValueError("The NDI package was not signed by the expected NewTek installer identity")
        expanded = work / "expanded"
        subprocess.run(["pkgutil", "--expand-full", str(package), str(expanded)], check=True)
        component = expanded / "libNDIComponent.pkg"
        version = ET.parse(component / "PackageInfo").getroot().get("version")
        if version != PACKAGE_VERSION:
            raise ValueError(f"Unexpected NDI package version: {version}")
        payload = component / "Payload"
        dylib = payload / "libndi.dylib"
        license_source = payload / "libndi_licenses.txt"
        require_digest(dylib, DYLIB_SHA256)
        require_digest(license_source, LICENSE_SHA256)
        if license_source.read_bytes() != repository_license.read_bytes():
            raise ValueError("The NDI redistributable licenses differ from the repository notice")
        # Copy the vendor-signed bytes unchanged; app packaging must not re-sign it.
        staged_runtime = work / "libndi.dylib"
        shutil.copy2(dylib, staged_runtime)
        write_metadata(destination, work, license_source)
        os.replace(staged_runtime, runtime)
    return runtime


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, help="Use an already downloaded official .pkg")
    args = parser.parse_args()
    try:
        runtime = ensure_runtime(Path(__file__).resolve().parents[1], args.package)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"NDI runtime preparation failed: {error}\n")
    print(runtime)


if __name__ == "__main__":
    main()
