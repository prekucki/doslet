#!/usr/bin/env python3
"""Build a private Civilization web site from the Rust Trunk output."""
import hashlib
import json
from pathlib import Path
import shutil
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "web/dist"
OUTPUT = ROOT / "target/civ-web"
CONFIG = ROOT / "tools/civ_web"
GAME = ROOT / "games/civ"
ROMS = Path.home() / "games/retro/mt-32"
TIERS = Path.home() / ".local/share/doslet/speed-tiers"
EXE_SHA256 = "31e35b01f4d00df81685dfcb81e6de196a3876bd2afff6e39cbbfd964f5852ad"
ROM_SHA1 = {
    "CM32L_CONTROL.ROM": "a439fbb390da38cada95a7cbb1d6ca199cd66ef8",
    "CM32L_PCM.ROM": "289cc298ad532b702461bfc738009d9ebe8025ea",
}


def main():
    if not (SOURCE / "index.html").is_file():
        raise SystemExit("Build the browser first: mise run web-build")
    profile = tomllib.loads((CONFIG / "CIV.toml").read_text())
    launch = json.loads((CONFIG / "launch.json").read_text())
    executable = GAME / profile["game"]["executable"]
    if not executable.is_file():
        raise SystemExit(f"Missing local game: {executable}")
    actual = hashlib.sha256(executable.read_bytes()).hexdigest()
    if actual != EXE_SHA256 or launch["startupPatch"]["executableSha256"] != actual:
        raise SystemExit("CIV.EXE changed; revalidate startup selections and native CPU labels")
    if launch["startupPatch"]["selections"] != ["1", "5", "1"]:
        raise SystemExit("CIV startup patch must select VGA, MT-32, mouse and keyboard")
    if launch["executable"] != executable.name:
        raise SystemExit("CIV launch manifest and machine profile disagree")
    if profile["input"].get("mouse_mode") != "auto" or profile["input"].get("hide_cursor") is not True:
        raise SystemExit("CIV browser input must use automatic mouse mode and hide the host cursor")
    if launch.get("displayFilter") != "hq2x":
        raise SystemExit("CIV browser display must select HQ2x")
    if launch["roms"] != list(ROM_SHA1):
        raise SystemExit("CIV launch manifest must use the supported CM-32L ROM pair")
    tiers = json.loads((TIERS / f"{actual}.json").read_text())
    if (tiers.get("identity") != f"exe-sha256:{actual}"
            or tiers.get("unknown") != "grey"
            or tiers.get("irq_policy") != "static-grey"
            or tiers.get("rates") != [99, 8, 16]
            or not tiers.get("nodes")):
        raise SystemExit("Native CIV speed-tier profile is missing or incompatible")
    for name, expected in ROM_SHA1.items():
        if hashlib.sha1((ROMS / name).read_bytes()).hexdigest() != expected:
            raise SystemExit(f"Roland CM-32L ROM changed: {name}")
    files = sorted(path for path in GAME.iterdir() if path.is_file() and not path.is_symlink())
    if (executable not in files or len(files) > 4096
            or sum(path.stat().st_size for path in files) > 256 * 1024 * 1024):
        raise SystemExit("CIV directory exceeds browser archive limits")
    if OUTPUT.exists():
        shutil.rmtree(OUTPUT)
    shutil.copytree(SOURCE, OUTPUT)
    bundle = OUTPUT / "bundle"
    bundle.mkdir()
    with zipfile.ZipFile(bundle / "CIV.zip", "w", zipfile.ZIP_STORED) as archive:
        for path in files:
            archive.write(path, path.name)
    if (bundle / "CIV.zip").stat().st_size > 64 * 1024 * 1024:
        raise SystemExit("CIV ZIP exceeds browser archive limit")
    with zipfile.ZipFile(bundle / "CIV.zip") as archive:
        if set(archive.namelist()) != {path.name for path in files}:
            raise SystemExit("CIV ZIP file list differs from the source directory")
        for path in files:
            if archive.read(path.name) != path.read_bytes():
                raise SystemExit(f"CIV ZIP content mismatch: {path.name}")
    shutil.copyfile(CONFIG / "CIV.toml", bundle / "CIV.toml")
    shutil.copyfile(CONFIG / "launch.json", bundle / "launch.json")
    shutil.copyfile(TIERS / f"{actual}.json", bundle / "CIV-speed-tiers.json")
    for name in ROM_SHA1:
        shutil.copyfile(ROMS / name, bundle / name)
    html = (OUTPUT / "index.html").read_text()
    overlay = """<style>#doslet-bundle-cover{position:fixed;inset:0;z-index:1000;display:grid;place-items:center;background:#111215;color:#ebedf0;font:16px system-ui}#doslet-bundle-card{padding:2rem;max-width:34rem;text-align:center;border:1px solid #454b57;border-radius:14px;background:#20232a}#doslet-bundle-launch{font:inherit;padding:.7rem 1.4rem;cursor:pointer}#doslet-bundle-launch:disabled{opacity:.5;cursor:wait}</style><div id="doslet-bundle-cover"><div id="doslet-bundle-card"><h1>Civilization</h1><p>Doslet wybierze VGA, Roland MT-32 oraz mysz i klawiaturę. Syntezator używa zgodnych ROM-ów CM-32L i natywnego profilu HOT/COLD/GREY.</p><button id="doslet-bundle-launch" disabled>Uruchom grę</button><p id="doslet-bundle-status">Ładowanie emulatora…</p></div></div><script type="module" src="browser/bundle-launch.js"></script>"""
    if "</body>" not in html:
        raise SystemExit("Trunk index.html has no body closing tag")
    (OUTPUT / "index.html").write_text(html.replace("</body>", overlay + "</body>"))
    (OUTPUT / "README.txt").write_text(
        "Prywatna paczka Civilization. Uruchom: python3 -m http.server 8081 --bind 127.0.0.1 "
        "--directory target/civ-web, potem otworz http://127.0.0.1:8081. "
        "Dla innego urzadzenia wymagany jest HTTPS. "
        "W paczce sa komercyjna gra, ROM-y CM-32L i bajty natywnego profilu CPU; nie publikuj jej.\n"
    )
    print(f"{OUTPUT} ({len(files)} files, {len(tiers['nodes'])} native CPU labels)")


if __name__ == "__main__":
    main()
