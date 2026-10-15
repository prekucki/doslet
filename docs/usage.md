# Using Doslet

This guide covers the toolchain, the desktop and browser frontends, audio and
MIDI routing, machine settings and saves. For tests, benchmarks, diagnostics and
execution-engine details, see [development](development.md).

## Install the toolchain

Install [mise](https://mise.jdx.dev/). The repository pins Rust 1.99.0,
rustfmt, Clippy and the `wasm32-unknown-unknown` target in
[`mise.toml`](../mise.toml):

```sh
mise install rust
```

Activate mise in your shell, or prefix the Cargo commands below with
`mise exec --`. Without mise, install [Rust through rustup](https://rustup.rs/)
and select the pinned toolchain explicitly:

```sh
rustup toolchain install 1.99.0 --component rustfmt --component clippy --target wasm32-unknown-unknown
export RUSTUP_TOOLCHAIN=1.99.0
```

## Desktop

Run the guest demo without a window:

```sh
cargo run -p doslet-headless --locked -- demo
```

Open the desktop workspace:

```sh
cargo run -p doslet --locked
```

### Launch a game

Launch your local game directly in the desktop window:

```sh
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE
```

The game starts automatically. Its containing directory is preloaded as C:;
guest writes use an in-memory overlay and do not modify your installation.
Keep the executable alongside its original DOS4GW and data files. DOS text
screens now render during startup; the recorded PRINCE HDI logo appears around
7.3 seconds of emulated time. Host speed may affect how long startup takes.
BIOS/DOS keyboard input is available. DOS mouse input includes position, buttons, ranges, a graphics cursor and guest callbacks.
Click the guest display to capture the mouse and use relative movement. **Esc**
releases capture; a subsequent Esc reaches the game. Focus loss, pause, reset
and game termination also release capture. The system cursor stays visible
until capture succeeds. Browser capture uses Pointer Lock.

MIDI from the guest is rendered by a basic procedural synthesizer and sent to the
default audio device. This is not a sample-based GM or Roland MT-32 synthesizer;
Sound Blaster PCM mixing remains pending. If no audio device
can be opened, a message appears and graphics continue. To disable audio:

```sh
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE --no-audio
cargo run -p doslet --release --locked -- --help
```

### Workspace controls and display

Press **Run** to execute the built-in demo, **Pause** to suspend execution, **Step** to
advance one execution step, or **Reset** to create a fresh session. Linux builds
require a C linker, `pkg-config`, ALSA and udev development files
(`libasound2-dev` and `libudev-dev` on Ubuntu/Debian); launching requires a working Wayland or X11
session and graphics driver. Dependency resolution and native compilation are
separate from checking a live window.

On desktop, **1:1** beside **GUEST DISPLAY** resizes the window to show each guest
pixel as one physical screen pixel, including on HiDPI displays. It uses the raw
image dimensions (for example, 720×400 for text) and allows scrolling if the
window is too small. **Fit** restores automatic scaling with 4:3 proportions.

### Machine configuration and saves

Open **Settings** in either frontend to edit a draft, validate its
resources and **Apply and restart**. The editor configures RAM, x87, SB16 and
MPU-401 hardware, AT/BIOS keyboard input, a two-axis/two-button gameport, host
output, installation media and save policy. Native Apply prepares sources on a
separate thread; failed preparation preserves the active session and reset
settings. Native **Fit** and **1:1** display sizing remain available.

Profiles import version 1 or 2 and export version 2. Persistent overlays use an
explicit native save directory or browser **IndexedDB** namespace. Save
export/import remains available; pending commits and storage errors are shown
in the workspace. Legacy native `rw` continues direct installation writeback.
See [machine configuration](configuration.md) for a version-2 profile,
controller bindings, write policies, local/HTTPS ISO requirements and current
implementation limits.

The local game installations stay ignored. HoMnM's profile mounts its
`homm1.gog` ISO and exposes MSCDEX sector services plus read-only DOS file
access. The recorded run passes its CD check; gameplay remains unverified.

Guest-created files use the reserved RAM overlay by default. They can be reopened
during the session. Export them or explicitly discard temporary changes before
restart; select a persistent save store to retain acknowledged changes.
`--no-audio` retains a silent path; choose **1 — No sounds please** in CIV.

### Launch profiles

[TOML launch profiles](launch-profiles.md) store native game/audio/MIDI
settings, DOS arguments and a C: directory mount. CLI options override profile
settings:

```sh
target/release/doslet --config games/CIV.toml
target/release/doslet --config games/PRINCE.toml --no-audio
target/release/doslet --config games/HoMnM.toml
```

[HEROES startup evidence](compatibility/heroes.md) records the x87 fixes
and remaining CPU requirements.

### External MIDI synthesizers

To use an external or system MIDI synthesizer, click **Find MIDI outputs** in
the UI and select its port under **MIDI OUTPUT**. **Rust synth** restores the
built-in synthesizer. The CLI supports the same choice:

```sh
cargo run -p doslet --locked -- --list-midi-outputs
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE --midi-output 0
```

Replace `0` with the index or exact name of your synthesizer's output port.
`--midi-output` skips CPAL and internal sample generation; it conflicts with
`--no-audio`. A MIDI forwarding port alone does not generate sound. Linux host
MIDI requires an available ALSA sequencer device. A dedicated native MIDI thread
sends complete messages at deadlines derived from guest time. Execution runs up
to 80 ms ahead to buffer short UI stalls; message timestamps retain bounded
execution-chunk resolution (about 1.2 ms). The preallocated queue holds at most
8192 messages and reports exhaustion. Pause/reset/output replacement cancel
queued messages and silence the selected port. **Info** reports queued
messages and dispatches more than 2 ms late, with maximum dispatch lateness.
These counters measure Doslet scheduling, not the external synth's PCM buffers.

### Civilization and MT-32-family audio

Launch your own installation of Civilization 474.03:

```sh
cargo run -p doslet --release --locked -- --game games/CIV.EXE --midi-mode mt32 --mt32-rom-dir /path/to/roms
```

Select **1 — VGA**, **5 — Roland MT-32 MIDI board**, and **1 — Mouse and Keyboard** (or **2 — Keyboard only**)
in the game's startup prompts. Type into the game window; BIOS scan codes and
ASCII characters support letters, digits, arrows, Enter, Escape, modifiers and
F1–F10. Keyboard events captured by application controls stay with those controls.

**GAME MIDI** chooses the game's MIDI dialect; **MIDI OUTPUT** chooses the
renderer or host port. The default game mode is GM. Roland MT-32 SysEx
automatically upgrades it to MT-32 and shows that change under **Info**.
Rust synth and GM host ports use **MT-32 → GM (approximation)** translation in
MT-32 mode. This approximation needs no ROMs and does not reproduce LA synthesis.
**Info** also shows SysEx counts, unmapped notes and the module's LCD text.
For example, ROM-free CIV audio uses:

```sh
cargo run -p doslet --release --locked -- --game games/CIV.EXE --midi-mode mt32 --midi-output rust
```

Mark a real MT-32 or Munt port using **HOST PORT DIALECT → MT-32**, or the CLI
`--midi-mode mt32 --midi-output PORT --midi-output-mode mt32`. These outputs
receive the original SysEx. GM input to an MT-32 host port is unsupported.
With a ROM directory, the default native output is moont; `--midi-output rust`
or a port explicitly selects another output. The ROM path remains available
when switching back to moont. In GM mode, moont uses GmDevice: CM-32L instruments
and eight melodic parts, rather than a complete General MIDI sound bank.

The ROM directory must contain `CM32L_CONTROL.ROM` (v1.02) and
`CM32L_PCM.ROM`. Doslet validates their SHA-1 fingerprints and loads them at
startup; ROMs are never embedded or distributed. The optional `no_std` Rust
[moont engine](https://crates.io/crates/moont/1.0.0) emulates CM-32L, an MT-32
family module. It processes the game's custom instrument SysEx and renders
32-kHz stereo, resampled to the output device. MT-32 Control ROM variants and
the module's LCD are not emulated by this backend. `--mt32-rom-dir` currently
selects native audio; browser ROM selection is pending.

The recorded local run reaches a new Roman game with its first Settler.
Another real run processes 11 startup SysEx messages and 23 note events,
producing nonzero PCM. These are bounded compatibility observations, not a
completed campaign or an audio-device listening test. See
[CIV evidence](compatibility/civ.md) for commands and limits.

### Linux launcher and application icon

Install the launcher and icon for COSMIC and other Linux desktops:

```sh
mise run desktop-install
```

This builds the release binary and uses Python 3's standard library to install
`org.doslet.Doslet.desktop` and the SVG icon under `$XDG_DATA_HOME` (default:
`~/.local/share`). No `sudo` is needed. Without mise, run:

```sh
cargo build -p doslet --release --locked
python3 tools/install_desktop.py
```

The launcher uses the absolute path to the built binary, so it does not depend
on the desktop session's `PATH`. For a custom Cargo target directory or another
installed binary, pass its path to `tools/install_desktop.py`. Run the installer
again after moving the binary or checkout. Rebuilding at the same path needs no
reinstallation. Restart any open Doslet windows after the first build/install;
the Linux window application ID matches the desktop entry and icon name.

## Browser

Build the browser application with [Trunk](https://trunkrs.dev/):

With mise, install the explicitly selected Rust tools and start the web task:

```sh
mise install rust cargo:trunk cargo:wasm-bindgen-cli github:WebAssembly/binaryen
mise run web
```

`mise.toml` pins Rust, Trunk, wasm-bindgen-cli and Binaryen (`wasm-opt`). Keep
wasm-bindgen-cli aligned with the wasm-bindgen version in `Cargo.lock`.
`mise run web-build` generates `web/dist` using LLVM `opt-level = 3` and
`wasm-opt -O3`. The web profile targets execution speed; actual game performance
must be measured. `trunk serve` also compiles this optimized Rust profile, while
the additional wasm-opt pass runs with `--release`.

Trunk is a Rust build tool. It runs Cargo, prepares the browser's WebAssembly
bindings and serves the resulting files locally. **Node.js, npm and npx are not
used for development, builds, tests or CI.** The browser JavaScript bindings are
generated by `wasm-bindgen` and execute in the browser.

```sh
cargo install trunk --version 0.21.13 --locked
cd web
trunk serve
```

Open `http://127.0.0.1:8080`. `trunk build` produces a static bundle in `web/dist`;
the HTML selects the `web` Cargo profile. The web frontend uses an embedded
font. No backend service is required for the demo.
The browser uses iced's tiny-skia software renderer and presents through
Canvas2D. The native frontend uses wgpu. Browser startup diagnostics are logged
to the console.

To mount a local game in the browser, pack its complete installation directory
as a ZIP, including DOS4GW, graphics drivers, music and other data files. The
ZIP may contain the files directly or inside a wrapping folder such as `PRINCE/`.
In Doslet, choose **Open… → Choose ZIP…**, select **PRINCE/PRINCE.EXE** (or the relevant
EXE/COM), then press **Start game**. The selected program's containing directory
becomes C:, with its subdirectories preserved. ZIP data stays in the browser;
there is no upload, and commercial files are not included in the web build.

Stored and Deflate ZIP entries are supported. The import accepts at most 64 MiB
of ZIP data, 4096 entries, 64 MiB per file and 256 MiB of expanded files. DOS
paths must use ASCII names; duplicate case-insensitive names, symbolic links,
encrypted files, absolute paths and parent-directory traversal are rejected.
Unsupported compression and corrupt data produce an error.

Restart recreates the running game's drive from the original ZIP. Guest writes
use a separate 1-MiB overlay. Choose temporary or persistent saves under
**Settings → Drives**; persistent browser saves use IndexedDB. Temporary changes
must be exported or explicitly discarded before restart. See
[configuration](configuration.md) for save policies and limits.
BIOS/DOS keyboard input and DOS mouse-driver input are mapped from the UI. **Start game** creates and resumes Web Audio for the actual
guest MIDI stream. If the browser keeps audio suspended, open **Audio → Enable audio**;
the same button offers **Mute** / **Unmute** once audio is running. **Info**
reports context state, note events, peak, rendered audio frames and underruns.
The machine and ZIP live in a dedicated Web Worker. A separate Rust/WASM module
synthesizes 48-kHz stereo directly in AudioWorklet; samples do not pass through
iced or the window thread. Bounded MIDI packets carry guest timestamps and
watermarks over a direct MessageChannel. Guest timing remains independent of
display refresh. Slow emulation can still underrun; the worklet emits silence
and preserves guest time until data arrives. Sound Blaster PCM mixing and
original GM/MT-32 instrument fidelity remain pending.

Browser **Find MIDI outputs** explicitly requests Web MIDI access. Choose a
port connected to a running system/software or hardware synthesizer; Web Audio
and Web MIDI do not supply a system GM synthesizer. Unsupported API, denied
permission and absent/disconnected ports are reported without removing the
built-in option. GM ports receive translated channel messages without SysEx.
Selecting an MT-32 port explicitly requests Web MIDI SysEx permission; denied
permission is reported. SysEx messages are bounded to 1024 bytes. Running status and messages split between batches are
assembled before sending. Ports with `clear()` use timestamped native scheduling;
other ports use an owned, bounded timer queue so pause/reset can cancel pending
messages. This fallback relies on window timers, unlike the Rust worklet.
Selecting a working output after a host fault allows Run without resetting the
machine. Switching replays channel state, RPN 0 and held/sustained notes. Raw MT-32
outputs additionally receive guest-written configuration through bounded DT1
packets. Voice envelopes and reverb tails are restarted.

For the local acceptance installation, `mise run package-prince` generates
ignored `target/prince-game.zip` and validates startup from that ZIP. This task
requires your own `PRINCE/` directory and never includes game data in a build.

If your shell exports `NO_COLOR=1`, Trunk 0.21.13 rejects that value. Use
`NO_COLOR=true trunk serve` or `NO_COLOR=true trunk build`; the mise web tasks
already set the compatible value.
