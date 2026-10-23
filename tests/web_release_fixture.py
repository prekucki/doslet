#!/usr/bin/env python3
"""Write original COM programs and profiles for browser release checks; no game assets."""
import argparse
import hashlib
import json
from pathlib import Path

from browser_smoke import game_zip


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    root = parser.parse_args().output
    root.mkdir(parents=True, exist_ok=True)
    (root / "game.zip").write_bytes(bytes(game_zip()))
    (root / "game.toml").write_text('''version = 2
name = "Release fixture"
[machine]
cpu = "i386"
[output]
audio = true
midi_output = "rust"
video_filter = "nearest"
[game]
executable = "GAME/TEST.COM"
[[drives]]
letter = "C"
kind = "zip"
source = "game.zip"
policy = "read-only"
''')
    # Like protected_graphics.rs, enter protected mode after selecting VGA.
    # Register AOT requires flat 32-bit segments; keep all segment bases zero.
    program = bytearray(0x400)
    startup = bytes.fromhex(
        "b81300 cd10 66 0f01160002 b81500 0f01f0 66ea 00040100 0800"
    )
    program[:len(startup)] = startup
    program[0x100:0x106] = bytes.fromhex("1700 00030100")  # GDTR -> 10300h
    program[0x208:0x210] = bytes.fromhex("ffff0000009acf00")
    program[0x210:0x218] = bytes.fromhex("ff0f00000092c000")
    code = bytes.fromhex(
        "fa b810000000 8ec0 8ed8 8ed0 bc00002000 "
        "b82a2a2a2a b9803e0000 bf00000a00 fc f3ab 40 43 41 42 ebfa"
    )
    program[0x300:0x300 + len(code)] = code
    (root / "AOT.COM").write_bytes(program)
    (root / "aot.toml").write_text('''version = 2
name = "AOT release fixture"
[output]
audio = false
video_filter = "nearest"
[game]
executable = "AOT.COM"
''')
    offset = 0x300 + len(code) - 6
    nodes = [
        {"pc": 0x10100 + offset + i, "bits": 32,
         "bytes": list(program[offset + i:offset + i + 15])}
        for i in range(4)
    ]
    (root / "aot-profile.json").write_text(json.dumps({
        "version": 1,
        "identity": "exe-sha256:" + hashlib.sha256(program).hexdigest(),
        "nodes": nodes,
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
