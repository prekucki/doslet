#!/usr/bin/env python3
"""Package existing Linux binaries and a Trunk runtime with checksums and notices."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("--metadata", type=Path, required=True,
                        help="cargo metadata --locked --offline --format-version 1 output")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--web-dist", type=Path, default=ROOT / "web/dist")
    args = parser.parse_args()
    if not re.fullmatch(r"v[0-9][A-Za-z0-9.+-]*", args.version):
        parser.error("version must be a v-prefixed tag without path separators")
    if git("status", "--porcelain"):
        parser.error("commit the source before packaging")
    args.output.mkdir(parents=True, exist_ok=False)
    commit = git("rev-parse", "HEAD")
    source = f"https://github.com/prekucki/doslet/tree/{commit}"
    packages = json.loads(args.metadata.read_text())["packages"]
    artifacts = []
    with tempfile.TemporaryDirectory(prefix="doslet-package-") as temporary:
        stage = Path(temporary)
        notices = stage / "licenses"
        notices.mkdir()
        shutil.copy2(ROOT / "LICENSE", notices / "Doslet-LICENSE")
        shutil.copy2(ROOT / "packaging/licenses/GPL-3.0.txt", notices / "GPL-3.0.txt")
        shutil.copy2(ROOT / "vendor/v86/LICENSE", notices / "v86-LICENSE")
        shutil.copy2(ROOT / "apps/doslet/src/ravu/COPYING.LESSER", notices / "RAVU-LGPL-3")
        # Preserve every upstream notice in the amalgamated source.
        shutil.copy2(ROOT / "vendor/v86/softfloat/softfloat.c", notices / "softfloat.c")
        shutil.copy2(ROOT / "apps/doslet/src/xbrz.wgsl", notices / "xbrz.wgsl")
        for package in packages:
            origin = Path(package["manifest_path"]).parent
            destination = notices / f"{package['name']}-{package['version']}"
            destination.mkdir(exist_ok=True)
            (destination / "license-expression.txt").write_text(
                (package.get("license") or "See source license files") + "\n")
            for pattern in ["LICENSE*", "LICENCE*", "COPYING*", "NOTICE*"]:
                for path in origin.glob(pattern):
                    if path.is_file():
                        shutil.copy2(path, destination / path.name)
            if package.get("license_file"):
                path = origin / package["license_file"]
                if path.is_file():
                    shutil.copy2(path, destination / path.name)
        common = (
            f"Doslet {args.version}\nSource commit: {commit}\nSource: {source}\n\n"
            "Experimental build. v86 gameport timing and browser game performance\n"
            "remain limited. See docs/backlog.md and docs/compatibility/v86-core.md\n"
            "in the source. No commercial games or MT-32 ROMs are included.\n\n"
            "License: see licenses/ and the source README. The included xBRZ\n"
            "filter means binaries are distributed under GPLv3 terms.\n"
        )
        for kind in ["linux-x86_64", "web"]:
            name = f"doslet-{args.version}-{kind}"
            directory = stage / name
            directory.mkdir()
            shutil.copytree(notices, directory / "licenses")
            instructions = common
            if kind == "linux-x86_64":
                for binary in ["doslet", "doslet-headless", "doslet-web"]:
                    shutil.copy2(ROOT / "target/release" / binary, directory / binary)
                instructions += (
                    "\nBuilt on Ubuntu-compatible 24.04, x86_64 GNU/Linux. Requires\n"
                    "glibc, libasound2, libudev1 and a working graphics driver.\n"
                    "Run ./doslet --demo or ./doslet --game /path/to/GAME.EXE.\n"
                    "Select a backend with --mode simple, --mode aot or --mode v86.\n"
                    "The native v86 core is embedded; no separate WASM file is needed.\n\n"
                    "For portable web builds, unpack the web archive too and use:\n"
                    "./doslet-web build /path/to/game.toml --mode v86 "
                    "--web-dist /path/to/web-runtime -o /path/to/new-site\n"
                    "The same --web-dist option works with serve and other modes.\n"
                    "Building a new runtime with Trunk requires the source checkout.\n"
                )
                archive = args.output / f"{name}.tar.gz"
            else:
                if not (args.web_dist / "browser/v86-runtime.json").is_file():
                    parser.error("web distribution must include the v86 runtime")
                shutil.copytree(args.web_dist, directory, dirs_exist_ok=True)
                instructions += (
                    "\nServe this directory over HTTP (not file://), for example:\n"
                    "python3 -m http.server 8080 --bind 127.0.0.1\n"
                    "Then open http://localhost:8080. The generic UI starts with the\n"
                    "interpreter. Use this directory as doslet-web --web-dist to\n"
                    "package a game with --mode simple, aot or v86.\n"
                )
                archive = args.output / f"{name}.zip"
            (directory / "README.txt").write_text(instructions)
            (directory / "BUILD.json").write_text(json.dumps({
                "version": args.version, "commit": commit, "target": kind,
                "source": source,
            }, indent=2) + "\n")
            if kind == "web":
                # Registry sources can carry normalized timestamps older than ZIP.
                with zipfile.ZipFile(
                    archive, "w", zipfile.ZIP_DEFLATED, strict_timestamps=False
                ) as output:
                    for path in sorted(directory.rglob("*")):
                        if path.is_file():
                            output.write(path, path.relative_to(stage))
            else:
                with tarfile.open(archive, "w:gz") as output:
                    output.add(directory, arcname=name)
            artifacts.append(archive)
    checksums = "".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
        for path in artifacts
    )
    (args.output / "SHA256SUMS").write_text(checksums)
    print(checksums, end="")


if __name__ == "__main__":
    main()
