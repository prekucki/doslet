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
screens render during startup. Native launches use the experimental DPMI host
in `direct` mode, including background speed-tier profiling. Select
`--experimental-dpmi reflect` for reflected services or `--experimental-dpmi off`
for the guest extender path. Portable emulator save files require the host to
be off; in-process profiling copies support both host modes.
The recorded PRINCE HDI logo appears around
7.3 seconds of emulated time. Host speed may affect how long startup takes.
BIOS/DOS keyboard input is available. DOS mouse input includes position, buttons, ranges, a graphics cursor and guest callbacks.
On native builds, a guest read of relative motion counters (INT 33h, AX=000Bh)
selects relative input for one emulated second after the latest read. Absolute
reads during that interval do not cancel it. Hover movement is blocked until you
click the guest display to capture the mouse. After one second without relative
reads, capture is released automatically and absolute movement resumes. Absolute
input does not require capture. **Ctrl+Esc** releases capture; **Esc** reaches
the game even while the mouse is captured. Focus loss, pause, reset and game
termination also release capture.
The host cursor stays visible while relative input is waiting for capture.
**Hide cursor over the game** controls visibility during uncaptured absolute input.
Leave **Capture mouse on click** enabled for games using relative counters.
Browser capture still uses explicit click-to-lock Pointer Lock; disabling capture
there selects absolute input.

By default, MIDI from the guest is rendered by a basic procedural synthesizer
and mixed with Sound Blaster PCM and AdLib/OPL3 FM on the default audio device.
FM is included when **SB16** is enabled in Settings. Select AdLib or Sound Blaster
music in the game's own setup to use FM; the MIDI output selector does not route
FM through an external MIDI device. No OPL instrument ROM is required.
The bottom bar shows a 16-band spectrum of the local audio output, from bass
on the left to treble on the right. It fades during silence and clears on pause
or mute. Audio produced by an external MIDI synthesizer is not measured.
Hover over the spectrum to see the guest's Sound Blaster PCM rate, bit depth
and mono/stereo format. The last configured format remains visible between
effects and clears on DSP reset; FM and MIDI have no guest PCM format.
Synthetic tests cover FM output and mixing; real-game FM playback is unverified.
The default MIDI synth
is not a sample-based GM or Roland MT-32 synthesizer. If no audio device
can be opened, a message appears and graphics continue. To disable audio:

```sh
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE --no-audio
cargo run -p doslet --release --locked -- --help
```

### Linux audio output

Native audio defaults to CPAL. On Linux, `--audio-backend pulse` selects an
experimental Rust implementation of the PulseAudio protocol, usable with
PulseAudio or PipeWire's `pipewire-pulse` service. COSMIC is not required.

```sh
cargo run -p doslet --release --locked -- --audio-backend pulse
```

The system mixer receives a source-specific name (**Doslet — Sound Blaster PCM**, **Doslet — AdLib / OPL FM**, **Doslet — MIDI**, or **Doslet — Mixed audio**), icon identifier
`org.doslet.Doslet`, and media role `game`. The new adapter uses the `pulseaudio`
Rust crate without adding libpulse/libpipewire bindings or development packages.
The existing CPAL and MIDI dependencies still use native system libraries; this
does not make the entire application free of FFI. MIDI rendering and forwarding
retain their existing controls.

With application mixing, `--audio-backend auto` tries the Rust adapter first and falls back to CPAL if
opening it fails. A running PulseAudio output has three reconnection attempts
over its lifetime. After exhaustion, `auto` pauses the game and attempts CPAL;
resume after the notice confirms the switch. Explicit `pulse` reports failure
without switching backends. `cpal` selects the previous path directly.

Choose **Info → Audio… → Mixer** on Linux, or pass `--audio-mixer application|system`.
**Application** is the default and combines sources inside Doslet. **System
(experimental)** sends three independent stereo float streams to the
PulseAudio-compatible server: **Sound Blaster PCM** at the guest DSP rate,
**AdLib / OPL FM** at 48 kHz, and **MIDI synthesizer** at 48 kHz.
PCM is decoded and converted to float without application-side resampling.
The existing FM and MIDI engines retain their internal rate conversion.
Changing the PCM rate drains and recreates its server stream; a brief gap is
possible. External MIDI ports retain their separate
routing, with silence in the local MIDI stream.

```sh
cargo run -p doslet --release --locked -- --audio-backend pulse --audio-mixer system
```

System mixing requires the Rust PulseAudio adapter regardless of the backend
selected for application mixing. Initial setup failure is reported without
silently selecting a different mixer. With `--audio-backend auto`, exhaustion
of any stream's three-attempt recovery budget pauses and attempts CPAL with
application mixing. Switching in the UI pauses the game; resume after the
success notice. A failed switch retains the previous output, paused. The choice
survives game resets and machine changes in this process, but is not saved in
TOML launch profiles or save states.

Exact sample alignment is not promised. A prior ten-minute, multi-rate probe
lost marker correspondence; long-run timing and audible quality still need
evaluation. The spectrum shows the maximum band level of the three submitted
sources, not the server's final mix or its volume adjustments.
Server buffering adds latency beyond Doslet's local queue. Validation on
PipeWire used a virtual monitor, not a listening test or a real PulseAudio daemon.
See [the audio probe procedure](development.md#linux-pulseaudio-protocol-probes).

### Workspace controls and display

The small **MPU** lamp in the bottom bar lights green for new MIDI notes and
amber for other MIDI traffic. Hover over it for byte/note counts and the time
since the last note. It tracks game output, independently of the selected
synthesizer; a dark lamp does not mean that a sustained note has stopped sounding.
The lamp stays dim while paused and adds no separate refresh timer.

Press the play icon to start or resume, the pause icon to suspend execution,
or the restart icon to create a fresh session. Hover over each icon for its
label. **Info → Step instruction** advances one execution step while paused.
Linux builds
require a C linker, `pkg-config`, ALSA and udev development files
(`libasound2-dev` and `libudev-dev` on Ubuntu/Debian); launching requires a working Wayland or X11
session and graphics driver. Dependency resolution and native compilation are
separate from checking a live window.

Desktop emulation and synthesis run on a dedicated thread. The Details panel
shows the nominal CPU MHz and **emulation lag** against the host clock. A growing
lag means execution is falling behind; increasing the configured MHz also
increases the required host work. A stalled window does not stop emulation:
presentation resumes with the latest image instead of replaying queued frames.

**Pixel perfect** selects the largest whole-number zoom that fits the available
display area, measured in physical pixels, including on HiDPI displays. It does
not resize the window. A 320×200 frame can become 640×400 or 960×600; a 640×480
frame can become 1280×960. The raw raster proportions are preserved, including
720×400 text. If even 1× does not fit, the display scrolls. **Fit** fills the
available area at 4:3 proportions and permits fractional scaling.

The small filter selector beside these buttons works on desktop and in the
browser, including while paused:

- **Nearest** preserves the original colors and pixel edges. Use it with
  **Pixel perfect** for an exact integer enlargement.
- **Sharp Bilinear** first repeats pixels by whole factors, then blends the
  remaining fractional scale. This keeps pixel interiors sharp while softening
  boundaries. The intermediate enlargement is capped at 4× per axis.
- **HQ2x** uses the HQx edge-aware algorithm to enlarge the frame 2× before
  linear sampling to the selected display size. It smooths diagonals and can
  change the appearance of small fonts.

Scaling and filtering are independent. Filters operate on presentation snapshots
in the UI; guest execution and mouse coordinates still use the original frame.
Filtered images are cached until the source frame, filter or intermediate zoom
changes. These filters use CPU preprocessing on both platforms; their live
frame rate has not been benchmarked.

Presentation keeps the previous image visible until the renderer has uploaded
its replacement. At most one upload is in progress; subsequent frames replace
the waiting frame, so a slower renderer does not accumulate a playback queue.
An upload failure keeps the last ready image and shows an error. Selecting a
filter or scaling mode retries the upload.

### Machine configuration and saves

To resume a portable machine snapshot in the native window, use
`target/release/doslet --load-state target/heroes-states/city.pb.gz`.
See [portable states](save-states.md#resume-in-the-native-window) for restore
semantics and the optional static hot/cold/grey experiment.

Open **Info → Settings…** in either frontend to edit a draft, validate its
resources and **Apply and restart**. The editor configures RAM, x87, SB16 and
MPU-401 hardware, AT/BIOS keyboard input, a two-axis/two-button gameport, host
output, installation media and save policy. Native Apply prepares sources on a
separate thread; failed preparation preserves the active session and reset
settings. **Fit**, **Pixel perfect** and display filters remain available.

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

### Apple General MIDI on macOS

Select **Apple DLS synth** under **MIDI OUTPUT**, or use:

```sh
cargo run -p doslet --release --locked -- --game path/to/GAME.EXE --midi-output apple
```

This uses macOS's DLSMusicDevice and system sound bank; no ROMs, separate MIDI
application or downloaded SoundFont are required. Its PCM is mixed with Sound
Blaster effects through the existing audio output. Pause preserves synth state;
Reset creates a fresh synth. Output changes restore channel settings and active
notes, restarting their envelopes and effects.

Version-2 profiles use `[output] midi_output = "apple"`; version-1 profiles use
`[midi] output = "apple"`. Apple output always receives General MIDI, including
the existing MT-32 → GM approximation when needed. It is available only in the
native macOS app; other platforms and the browser report an unsupported output.

### External MIDI synthesizers

A running TiMidity ALSA sequencer port appears among the native MIDI outputs.
`--midi-output timidity` selects the first port whose name contains TiMidity
(case-insensitive); select an exact name or index when several ports exist.
Doslet neither installs nor launches TiMidity, and an installed executable alone
does not expose a MIDI output. This uses the existing MIDI adapter without a new
dependency. TiMidity owns its sound bank, effects and audio output.

Native startup discovers available system MIDI ports. To refresh them, click
**Find MIDI outputs** in the UI and select a port under **MIDI OUTPUT**. **Rust synth** restores the
built-in synthesizer. The CLI supports the same choice:

```sh
cargo run -p doslet --locked -- --list-midi-outputs
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE --midi-output 0
```

Replace `0` with the index or exact name of your synthesizer's output port.
Selecting a port skips internal MIDI sample generation; Sound Blaster PCM still
uses the selected audio backend. `--midi-output` conflicts with `--no-audio`. A MIDI forwarding port
alone does not generate sound. Linux host
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
Rust synth, Apple DLS synth and GM host ports use **MT-32 → GM (approximation)**
translation in MT-32 mode. This approximation needs no ROMs and does not reproduce LA synthesis.
**Info** also shows SysEx counts, unmapped notes and the module's LCD text.
For example, ROM-free CIV audio uses:

```sh
cargo run -p doslet --release --locked -- --game games/CIV.EXE --midi-mode mt32 --midi-output rust
```

Mark a real MT-32 or Munt port using **HOST PORT DIALECT → MT-32**, or the CLI
`--midi-mode mt32 --midi-output PORT --midi-output-mode mt32`. These outputs
receive the original SysEx. GM input to an MT-32 host port is unsupported.
With a ROM directory, the default native output is moont; `--midi-output rust`,
`--midi-output apple` or a port explicitly selects another output. The ROM path
remains available when switching back to moont. In GM mode, moont uses GmDevice: CM-32L instruments
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

### Local macOS application

On macOS, build a release application bundle with Cargo and Python 3:

```sh
python3 tools/package_macos.py
```

The same command is available as `mise run macos-app`. Open
`target/macos/Doslet.app` in Finder, or copy it to `/Applications`.
The bundle contains its own executable and can be moved independently of the
checkout. It uses the build machine's architecture. The icon is rendered with
Swift and AppKit and packaged with macOS `iconutil`; this requires the Xcode
Command Line Tools. Games and synthesizer ROMs are not included.

This bundle is for local use; the script does not apply a Developer ID signature
or notarize it. Run the command again after changing the code, and replace any
copy you previously placed in `/Applications`.

### Linux launcher and application icon

Install the launcher and icon for COSMIC and other Linux desktops:

```sh
mise run desktop-install
```

The launcher includes System Mixer and Silent actions. Window titles include
the game name; audio streams advertise the `org.doslet.Doslet` application ID.
Run the installer again after updating to refresh desktop entries.

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
guest MIDI stream. If the browser keeps audio suspended, open **Info → Audio… → Enable audio**;
the same button offers **Mute** / **Unmute** once audio is running. **Info**
reports context state, note events, peak, rendered audio frames and underruns.
The machine and ZIP live in a dedicated Web Worker. A separate Rust/WASM module
synthesizes 48-kHz stereo directly in AudioWorklet; samples do not pass through
iced or the window thread. Bounded MIDI packets carry guest timestamps and
watermarks over a direct MessageChannel. Guest timing remains independent of
display refresh. Slow emulation can still underrun; the worklet emits silence
and preserves guest time until data arrives. Sound Blaster PCM and OPL3 FM use
the existing timestamped PCM transport to the worklet. Original GM/MT-32
instrument fidelity remains pending.

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

For a local acceptance installation in `PRINCE/`, the opt-in test below
generates ignored `target/prince-game.zip` and validates startup from that ZIP.
It requires your own `PRINCE/` directory and never includes game data in a build:

```sh
cargo test -p doslet --release --locked local_prince_zip_round_trip -- --ignored --nocapture
```

If your shell exports `NO_COLOR=1`, Trunk 0.21.13 rejects that value. Use
`NO_COLOR=true trunk serve` or `NO_COLOR=true trunk build`; the mise web tasks
already set the compatible value.
