# Game launch profiles

Native Doslet supports version-1 TOML profiles through `--config`. Paths inside
the profile resolve relative to the TOML file. CLI paths remain relative to the
shell's working directory. Browser profile selection is not implemented.

```sh
target/release/doslet --config games/CIV.toml
target/release/doslet --config games/PRINCE.toml --no-audio
target/release/doslet --config games/HoMnM.toml
```

The three local profiles live in ignored `games/`. Profiles can mount a single
ISO 9660 image as a DOS CD-ROM alongside the C: game directory. HoMnM's profile
mounts `HoMnM/homm1.gog` as D:. The recorded game run passes its CD check and
enters VBE graphics and the publisher intro. Playable-session compatibility
remains unverified.

## Supported fields

```toml
version = 1
name = "Civilization"

[game]
executable = "civ/CIV.EXE"
arguments = []
working_directory = 'C:\'

[midi]
game_mode = "mt32"
output = "moont"
rom_directory = "/path/to/mt32-roms"

[audio]
enabled = true
```

`version` is required and must be the integer 1. The remaining fields are
optional, but the final settings must include an executable from the profile
or `--game`. An omitted name uses the executable filename. Unknown fields,
wrong types and unsupported versions are errors.

Without `[[drives]]`, the executable's containing directory is mounted as C:.
An explicit single C: directory mount can expose a larger game tree:

```toml
[[drives]]
letter = "C"
kind = "directory"
source = "PRINCE"
mode = "overlay"
```

The executable must be inside the mounted directory. `mode` selects `ro`
(deny writes), `overlay` (default, RAM only) or `rw` (native host writeback).
Legacy `writable = true` selects overlay and false selects ro; do not combine
it with `mode`. Explicit `--drive-mode ro|overlay|rw` overrides the profile.
Overlay writes disappear on reset/exit. RW flushes modified files after their
handles close, using a temporary sibling and atomic rename. It preserves the
case of existing names and refuses symlinks and paths outside C:. The RAM byte,
extent and file limits still apply; delete/rename and writable CDs are pending.

To persist Heroes sound setup, choose Save Changes inside SETUP:

```sh
target/release/doslet --config games/HoMnM.toml --game games/HoMnM/SETUP.EXE --drive-mode rw
```

An optional immutable CD-ROM image uses a separate drive letter
from D: to Z:

```toml
[[drives]]
letter = "D"
kind = "cdrom"
source = "HoMnM/homm1.gog"
```

The image is validated as sector-aligned ISO 9660 during session setup. MSCDEX
exposes drive discovery, version, primary-volume metadata and raw sector reads.
Absolute DOS file paths on the CD support open/read/seek/close, read-only
attributes and handle information. Reads borrow image extents without copying
the media. The configured ISO is excluded from the C: directory mount so it is
not loaded twice. Joliet/Rock Ridge, multi-extent files, CD default-drive/CWD,
FindFirst enumeration, MSCDEX driver info/device requests and CD audio remain
pending. `writable = false` or `mode = "ro"` is accepted for CD images; other
write modes are rejected.

`game.arguments` is a string array. Doslet joins it into the PSP command tail
with a leading space and quotes empty arguments or arguments containing spaces.
Arguments must be printable ASCII without embedded double quotes; the complete
tail must fit 126 bytes. Overlong input is rejected. `working_directory` is an
absolute ASCII DOS path on C:, defaults to `C:\`, and must refer to a directory
exposed by the mounted files. Reset restores both arguments and working directory.
The process's host working directory is never changed.

## MIDI and audio

`midi.game_mode` and `midi.host_mode` accept `gm` or `mt32`; both default to GM.
The game mode should match the sound option chosen in game setup. Roland MT-32
SysEx still upgrades the effective game mode automatically. Rust output in
MT-32 mode is labelled “MT-32 → GM (approximation)”.

`midi.output` accepts `rust`, `apple` (native macOS), `moont`, or a host MIDI port name/index. Port names
are preferable to indices across host restarts. `moont` requires
`midi.rom_directory`, containing the user's `CM32L_CONTROL.ROM` and
`CM32L_PCM.ROM`. With a ROM directory and no explicit output, moont is selected;
otherwise the existing Rust synth default applies. ROMs remain user-owned and
are never embedded in profiles or the build.

`apple` uses the macOS system General MIDI bank and needs no ROM directory.
It always receives GM, using MT-32 → GM translation for MT-32 games;
`midi.host_mode` only selects the dialect of an external MIDI port.

`audio.enabled` defaults to true. False prevents opening an audio device and
clears the selected MIDI output and ROM directory for this launch.

## CLI precedence

Built-in defaults are overridden by profile values, then explicit CLI options:

```sh
target/release/doslet --config games/CIV.toml --midi-output rust
target/release/doslet --config games/CIV.toml --midi-mode gm --no-audio
```

`--game`, `--midi-mode`, `--midi-output-mode`, `--midi-output` and
`--mt32-rom-dir` override their profile fields. `--no-audio` disables profile
audio, including profile-supplied ROM/output settings. Explicit `--midi-output`
or `--mt32-rom-dir` enables audio disabled in a profile. Explicit conflicting
CLI options such as `--no-audio --midi-output` remain Clap errors.

## Remaining storage work

Browser profiles need archive-relative paths and user-selected media handles
before the resolved description is sent to the Worker.

No `loadfix` workaround is added by default: PRINCE's observed memory failure
was descriptor exhaustion, fixed in the allocator. Future memory-layout settings
need a reproducible requirement and an implemented loader contract.
