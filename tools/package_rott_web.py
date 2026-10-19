#!/usr/bin/env python3
"""Make a private static ROTT site from the already built Trunk bundle."""
import hashlib
import json
from pathlib import Path
import shutil
import struct
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "web/dist"
OUTPUT = ROOT / "target/rott-web"
PROFILE = ROOT / "games/ROTT.toml"
SAVES = ROOT / "games/rott-saves"
ROMS = Path.home() / ".config/dosbox/mt32-roms"
TIER_BASE = Path.home() / ".local/share/doslet/speed-tiers/profiles/RiseoftheTriad-speed-tiers.json"
TIER_LEARNED = TIER_BASE.with_name(TIER_BASE.name + ".learned.json")
ROTT_EXE_SHA256 = "2deedf54c45178792483114c017223407e3044abcc0863526aa477654a78569a"
# Preserve native instruction labels, but pace the browser's interpreter at rates
# that kept the bundled ROTT startup synchronized with the audio worklet.
WEB_TIER_RATES = [133, 8, 33]
ROM_HASHES = {
    "CM32L_CONTROL.ROM": "a439fbb390da38cada95a7cbb1d6ca199cd66ef8",
    "CM32L_PCM.ROM": "289cc298ad532b702461bfc738009d9ebe8025ea",
}


def checksum(data):
    value = 0xCBF29CE484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return value


def main():
    if not (SOURCE / "index.html").is_file():
        raise SystemExit("Build the browser first: mise run web-build")
    config = tomllib.loads(PROFILE.read_text())
    archive = Path(config["drives"][0]["source"])
    manifest = (SAVES / "manifest.bin").read_bytes()
    if (manifest[:8] != b"DSN1\x01\0\0\0" or len(manifest) < 108
            or len(manifest) != 108 + 24 * struct.unpack_from("<I", manifest, 96)[0]
            or checksum(manifest[:-8]) != struct.unpack_from("<Q", manifest, len(manifest) - 8)[0]):
        raise SystemExit("ROTT save manifest is invalid")
    revision, digest = struct.unpack_from("<QQ", manifest, 80)
    snapshot = SAVES / f"snapshot-{revision:016x}-{digest:016x}.bin"
    payload = snapshot.read_bytes()
    if (not payload.startswith(b"DSV1") or len(payload) > 16 * 1024 * 1024
            or checksum(payload) != digest):
        raise SystemExit("ROTT save overlay is invalid")
    for name, expected in ROM_HASHES.items():
        actual = hashlib.sha1((ROMS / name).read_bytes()).hexdigest()
        if actual != expected:
            raise SystemExit(f"Unsupported {name}: {actual}")
    if not archive.is_file() or archive.stat().st_size > 64 * 1024 * 1024:
        raise SystemExit("ROTT ZIP is missing or too large")
    with zipfile.ZipFile(archive) as game_zip:
        executable = config["game"]["executable"].replace("\\", "/").upper()
        entries = [name for name in game_zip.namelist()
                   if name.upper() == executable or name.upper().endswith("/" + executable)]
        if len(entries) != 1:
            raise SystemExit(f"Expected one {executable} in the ROTT ZIP")
        exe_hash = hashlib.sha256(game_zip.read(entries[0])).hexdigest()
    if exe_hash != ROTT_EXE_SHA256:
        raise SystemExit(f"ROTT.EXE changed ({exe_hash}); verify the native labels before packaging")
    tiers = json.loads(TIER_LEARNED.read_text())
    expected_identity = "profile-sha256:" + hashlib.sha256(TIER_BASE.read_bytes()).hexdigest()
    if (tiers.get("identity") != expected_identity or tiers.get("unknown") != "grey"
            or tiers.get("irq_policy") != "static-grey" or not tiers.get("nodes")):
        raise SystemExit("The learned ROTT speed-tier profile does not match its base profile")
    exact_profile = TIER_BASE.parent.parent / f"{exe_hash}.json"
    if exact_profile.exists():
        raise SystemExit(f"An exact-executable profile is now available ({exact_profile}); compare it with the selected learned profile before packaging")

    if OUTPUT.exists():
        shutil.rmtree(OUTPUT)
    shutil.copytree(SOURCE, OUTPUT)
    bundle = OUTPUT / "bundle"
    bundle.mkdir()
    shutil.copyfile(archive, bundle / "ROTT.zip")
    (bundle / "saves.dsv").write_bytes(payload)
    tiers["rates"] = WEB_TIER_RATES
    (bundle / "ROTT-speed-tiers.json").write_text(json.dumps(tiers, separators=(",", ":")))
    for name in ROM_HASHES:
        shutil.copyfile(ROMS / name, bundle / name)
    (bundle / "ROTT.toml").write_text("""version = 2
name = "Rise of the Triad"
[machine]
cpu_mhz = 133
[game]
executable = "ROTT.EXE"
working_directory = 'C:\\'
[input]
gameport = true
keyboard = "at"
mouse_mode = "capture"
[output]
audio = true
game_midi = "gm"
host_midi = "gm"
midi_output = "moont"
rom_binding = "bundle"
[[drives]]
letter = "C"
kind = "zip"
source = "selected-zip"
policy = "persistent"
store = "rott-browser-saves"
[[bindings]]
id = "selected-zip"
kind = "zip"
location = "bundled/ROTT.zip"
[[bindings]]
id = "rott-browser-saves"
kind = "indexeddb"
location = "rott-bundled-saves"
""")
    html = (OUTPUT / "index.html").read_text()
    overlay = """<style>#doslet-bundle-cover{position:fixed;inset:0;z-index:1000;display:grid;place-items:center;background:#111215;color:#ebedf0;font:16px system-ui}#doslet-bundle-card{padding:2rem;max-width:34rem;text-align:center;border:1px solid #454b57;border-radius:14px;background:#20232a}#doslet-bundle-launch{font:inherit;padding:.7rem 1.4rem;cursor:pointer}#doslet-bundle-launch:disabled{opacity:.5;cursor:wait}</style><div id="doslet-bundle-cover"><div id="doslet-bundle-card"><h1>Rise of the Triad</h1><p>Doslet uruchomi grę z dołączonym zapisem ustawień dźwięku i syntezatorem CM-32L.</p><button id="doslet-bundle-launch" disabled>Uruchom grę</button><p id="doslet-bundle-status">Ładowanie emulatora…</p></div></div><script type="module" src="browser/bundle-launch.js"></script>"""
    (OUTPUT / "index.html").write_text(html.replace("</body>", overlay + "</body>"))
    (OUTPUT / "README.txt").write_text(
        "Private ROTT site. Serve this directory over localhost or HTTPS; for example: "
        "python3 -m http.server 8080 --bind 127.0.0.1 --directory target/rott-web. "
        "Open http://127.0.0.1:8080 on the same computer and click Uruchom grę. "
        "For a browser on another computer or phone, serve the site over HTTPS; "
        "plain HTTP at a LAN IP is not a secure browser context and cannot run the save/audio APIs. "
        "The bundled CPU labels come from the local native ROTT profile; "
        "browser rates are HOT 133, COLD 8, GREY/IRQ 33 MHz. "
        "The included commercial game, profile instruction bytes and CM-32L ROMs must not be redistributed.\n"
    )
    print(f"{OUTPUT} (native labels: {TIER_LEARNED}, {len(tiers['nodes'])} nodes)")


if __name__ == "__main__":
    main()
