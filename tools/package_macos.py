#!/usr/bin/env python3
"""Build a local macOS application bundle using Cargo and Python's standard library."""

import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys


REPO = Path(__file__).resolve().parent.parent


def main() -> None:
    if sys.platform != "darwin":
        raise SystemExit("macOS packaging must run on macOS")

    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=REPO,
    ))
    package = next(p for p in metadata["packages"] if p["name"] == "doslet")
    binary = None
    with subprocess.Popen(
        ["cargo", "build", "-p", "doslet", "--release", "--locked",
         "--message-format=json-render-diagnostics"],
        cwd=REPO, stdout=subprocess.PIPE, text=True,
    ) as build:
        for line in build.stdout:
            message = json.loads(line)
            if (message.get("reason") == "compiler-artifact"
                    and message.get("package_id") == package["id"]
                    and message.get("executable")
                    and "bin" in message["target"]["kind"]):
                binary = Path(message["executable"])
        if build.wait():
            raise SystemExit("Cargo build failed; application bundle was not updated")
    if binary is None:
        raise SystemExit("Cargo did not report a Doslet executable")

    bundle = Path(metadata["target_directory"]) / "macos" / "Doslet.app"
    contents = bundle / "Contents"
    executable = contents / "MacOS" / "doslet"
    executable.parent.mkdir(parents=True, exist_ok=True)
    (contents / "Resources").mkdir(exist_ok=True)
    iconset = bundle.parent / "Doslet.iconset"
    subprocess.run(
        ["swift", str(REPO / "tools/render_macos_icon.swift"),
         str(REPO / "assets/org.doslet.Doslet.svg"), str(iconset)],
        cwd=REPO, check=True,
    )
    subprocess.run(
        ["iconutil", "-c", "icns", str(iconset), "-o",
         str(contents / "Resources/Doslet.icns")], check=True,
    )
    shutil.copy2(binary, executable)
    with (contents / "Info.plist").open("wb") as output:
        plistlib.dump({
            "CFBundleDevelopmentRegion": "en",
            "CFBundleDisplayName": "Doslet",
            "CFBundleExecutable": "doslet",
            "CFBundleIconFile": "Doslet.icns",
            "CFBundleIdentifier": "org.doslet.Doslet",
            "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleName": "Doslet",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": package["version"],
            "CFBundleVersion": package["version"],
            "NSHighResolutionCapable": True,
        }, output)
    print(f"Created {bundle}")
    print("Open Doslet.app in Finder, or copy it to /Applications.")


if __name__ == "__main__":
    main()
