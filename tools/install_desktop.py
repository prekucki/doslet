#!/usr/bin/env python3
"""Install the Linux launcher and icon for an existing Doslet binary."""

import argparse
import os
from pathlib import Path
import shutil
import sys


APP_ID = "org.doslet.Doslet"
REPO = Path(__file__).resolve().parent.parent


def desktop_exec(binary: Path) -> str:
    # Exec quoting is applied first, then the desktop entry's string escaping.
    # Percent signs must survive the launcher's field-code expansion as literals.
    argument = str(binary).replace("%", "%%")
    for character in ("\\", '"', "`", "$"):
        argument = argument.replace(character, "\\" + character)
    argument = argument.replace("\\", "\\\\")
    return '"' + argument + '"'


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "binary", nargs="?", type=Path, default=REPO / "target/release/doslet",
        help="built Doslet executable (default: target/release/doslet)",
    )
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("desktop installation is supported on Linux")
    binary = args.binary.expanduser().resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"executable not found: {binary}; run cargo build -p doslet --release --locked")
    if any(character in str(binary) for character in ("\n", "\r", "\t")):
        parser.error("the executable path cannot contain tabs or line breaks")

    data_home = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
    if not data_home.is_absolute():
        parser.error("XDG_DATA_HOME must be an absolute path")
    desktop = data_home / "applications" / f"{APP_ID}.desktop"
    icon = data_home / "icons/hicolor/scalable/apps" / f"{APP_ID}.svg"
    template = (REPO / "packaging/linux" / f"{APP_ID}.desktop").read_text(encoding="utf-8")
    # Preserve action arguments while resolving every launcher to this binary.
    contents = "\n".join(
        "Exec=" + desktop_exec(binary) + line[len("Exec=doslet"):]
        if line == "Exec=doslet" or line.startswith("Exec=doslet ") else line
        for line in template.splitlines()
    ) + "\n"

    # Install only launcher metadata; the binary stays at the selected path.
    icon.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(REPO / "assets" / f"{APP_ID}.svg", icon)
    desktop.parent.mkdir(parents=True, exist_ok=True)
    desktop.write_text(contents, encoding="utf-8")
    print(f"Installed launcher: {desktop}")
    print(f"Installed icon: {icon}")
    print(f"Executable: {binary}")
    print("Restart Doslet to use its application icon.")


if __name__ == "__main__":
    main()
