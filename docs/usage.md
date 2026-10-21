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

Open the desktop workspace with its built-in demo:

```sh
cargo run -p doslet --locked -- --demo
```

### Game library

Without a game, `--config`, `--load-state` or a debugger option, Doslet opens a
library of the launch profiles in the
[profile directory](configuration.md#host-directories), on Linux
`~/.config/doslet/profiles`. The working directory is not scanned, since a
`games` directory there may hold unrelated TOML files. A symbolic link from the
profile directory to your own profile folder keeps relative cover and drive
paths working. Each version-2 profile becomes a card with its `[info]` cover,
release year and genre:
- **Sidebar.** Filters all games, one genre, or opens **Settings** (`settings.toml`
  for global host audio mixer, MIDI output, gamepad deadzone, display scaling,
  interface language, and default/custom hardware presets).
- **Search.** Matches titles, developers, publishers, genres and program names.
- **Game page.** Clicking a card opens it, with the cover over a blurred
  backdrop, primary launch actions (**Play** / **Resume**, **Restart**, and
  configured `[[game.actions]]` entrypoints such as **Setup**), and three tabs:
  - **Overview** — description and game metadata.
  - **Drive C: & State** — paused RAM snapshot status (`autosave.state.gz`), a
    VFS Drive `C:` browser with **All files (VFS)** vs **Changes only** modes,
    color-coded overlay status (`+ Created`, `~ Modified`, `→ Renamed`,
    `− Deleted`, `Base`), inline hex/ASCII file preview, and **Reset C: changes**.
  - **Configuration** — per-game hardware preset, CPU clock/mode, RAM, SB16,
    MPU-401, Gameport, Drive `C:` / CD-ROM `D:` paths, and entrypoints.
- **Placeholders.** Profiles without a cover get a colored card with the game's
  initials.

**Play** prepares the game in the background, then shows it in the same window,
exactly as `doslet --config PROFILE` would. Options given to the library, such
as `--dpmi off` or `--no-audio`, apply to every game it starts.
**‹ Library** in the game's toolbar, or closing the window while a library game
is running, commits pending disk writes and saves a paused RAM snapshot
(`autosave.state.gz`) so **Resume** continues where you left off. Leaving the
game by quitting to DOS returns to the library and removes `autosave.state.gz`
while keeping persistent disk saves in `overlay.redb`.
**Rescan profiles** reloads the directories. Profiles that fail to parse are
skipped with a warning in the terminal. When no profiles are installed yet, the
library offers a button to launch the built-in test demo or open global settings.
Describe a game with the `[info]` fields in
[launch profiles](configuration.md#global-settings-hardware-presets-and-game-profiles).

### Launch a game

Launch a ZIP installation without extracting it:

```sh
cargo run -p doslet --release --locked -- --zip /home/reqc/games/retro/dos/ROTT.zip --program ROTT.EXE
# Equivalent local profile:
cargo run -p doslet --release --locked -- --config games/ROTT.toml
```

Use `--program` to choose an EXE or COM path inside the archive, ignoring case.
With a ZIP profile, `--config` plus `--program` runs another program from the
same ZIP and save store, for example ROTT's sound setup:
`--config games/ROTT.toml --program SNDSETUP.EXE`.
If the archive contains one program, Doslet selects it automatically; otherwise
it reports the available programs. The selected program's directory becomes C:,
including its subdirectories. ZIP data remains unchanged. Writes use a temporary
RAM overlay by default; `--drive-mode ro` denies writes, and `rw` is rejected.
Version-2 profiles support `kind = "zip"` with a profile-relative source or a
`kind = "zip"` host binding. A separate persistent save store can retain saves.
Reset reloads the archive with the active machine settings. ZIP limits are
64 MiB compressed, 4096 entries, 64 MiB per file and 256 MiB expanded. Memory
patch manifests are currently unsupported for native ZIP launches. Successful
archive loading alone does not establish game compatibility.

Launch your local game directly in the desktop window:

```sh
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE
```

The game starts automatically. Its containing directory is preloaded as C:;
guest writes use an in-memory overlay and do not modify your installation.
Keep the executable alongside its original DOS4GW and data files. DOS text
screens render during startup. Native launches use Doslet's DPMI host
in `direct` mode, including speed-tier profiling. Select
`--dpmi reflect` for reflected services or `--dpmi off` for the guest
extender path, or set `machine.dpmi` in the profile. Learned speed tiers,
relocatable patches and AOT procedures follow the program to wherever the mode
loads it. Portable emulator save files and in-process profiling copies support
every host mode.
The recorded PRINCE HDI logo appears around
7.3 seconds of emulated time. Host speed may affect how long startup takes.
BIOS/DOS keyboard input is available. DOS mouse input includes position, buttons, ranges, a graphics cursor and guest callbacks.
Open **Mouse & keyboard** using the icon after Drives on the bottom bar. Choose
**Auto**, **Capture**, or **No capture**, cursor visibility, and BIOS/AT keyboard
delivery. Changes apply immediately without restarting. Keyboard changes release
held keys; the selected settings survive Reset. The mouse and keyboard controls
are also available in **Settings → Input**.
**Auto** follows relative-motion reads by the game: click the display to capture,
and the lock releases after one guest second without relative reads. **Capture**
keeps relative input active after a display click. **No capture** always uses
absolute motion. **Ctrl+Esc** releases native capture; plain **Esc** reaches the
game. In a browser, Pointer Lock requires a display click and Escape releases it.
Focus loss, pause, settings panels, reset and game termination also release
capture. **Hide cursor over the game** controls the host cursor during absolute
input; the cursor stays visible while relative input is waiting for capture.

### Execution core and AOT

The desktop app runs the interpreter by default. For a 32-bit LE program
(DOS4GW and similar extenders), it finds where the loader placed each LE object
shortly after that code starts, and installs the procedure module of that
layout from the user cache. The CPU panel then shows **AOT · Wasmtime** as the
execution core. Whole game procedures run as native code compiled by Wasmtime,
and the interpreter runs everything else. Reset finds the layout again and
reinstalls the module. `--no-aot` keeps the plain interpreter.

Speed tiers and the module run together. A procedure call is charged to the
tier its labels prove: all of its instructions in one tier, or exact per-tier
counts for a copy kernel. Mixed or changed code runs on the labelled
interpreter instead. On ROTT under DPMI off with its learned labels, 40
guest seconds from boot took 8.3 s with tiers alone and 7.3 s with the module
(1.13×). Tier learning (`--learn-speed-tiers`) keeps the plain
interpreter. A cached module that cannot be used is reported and ignored; an
explicit `--wasm-procedures FILE` is installed at launch and stops it instead.

A module binds live addresses. DOS4GW loads ROTT's code at 0x18e000 without a
DPMI host and at 0x1e4000 with one; another memory configuration can move it
as well. Each layout keeps its own module,
`aot/<exe-sha256>/<layout>/procedures.wasm` under the
[cache directory](configuration.md#host-directories). `<layout>` names the
object bases and a digest of their selectors, for example
`le-1e4000-265000-266000-268000-2697eb6b`.

When a layout has no module yet:
1. Its live bindings compile on a background thread while the game continues
   on the interpreter. ROTT's 202 procedures take about 0.1 s.
2. The module is installed at the next batch boundary at which RAM is not
   being recorded for an idle-loop proof.
3. Every 10,000 calls across the module, each entry is evaluated separately.
   An entry receives at least 128 calls before it can be disabled for averaging
   fewer than 32 translated instructions. Entries still awaiting that sample
   remain enabled. Disabled entries retain their compiled functions and are
   retried when a replacement module is installed. If no entry remains enabled, the
   module is suspended and removed at a suitable batch boundary, and an
   `unavailable.txt` reason is left beside it; delete that file to try again.

Earlier measurements found about 700 instructions per call for ROTT and 7 for
HEROES.EXE. The latter module ran 20% slower than the interpreter; those results
motivated the 32-instruction threshold. They predate selection by entry.
The terminal names the live layout, the compilation, the module in use, or why
there is none. For example, `CIV.EXE is not a 32-bit LE program` means a
real-mode MZ game without DOS4GW, which the AOT generator cannot translate.

The same steps are available offline. `capture_procedure_layout` writes the
live bindings (pass `--dpmi off` for a launch with `--dpmi off`), and
`prepare_memory_procedures` compiles them into the cache entry of that layout
or a chosen directory:

```sh
cargo run -p doslet-headless --release --locked --features wasm-static-test --example capture_procedure_layout -- games/ROTT.toml target/rott-layout-direct
cargo run -p doslet-headless --release --locked --features wasm-static-test --example prepare_memory_procedures -- games/ROTT.toml target/rott-layout-direct/bindings.json --computation-kernels --kernel-interrupts before-irq
```

A module records its DPMI mode and layout. Doslet installs a cached module only
for its own layout, and an explicit one only under its DPMI mode.

**Measurements.** Both comparisons below cover emulated execution only, not
desktop frame rate:
- **AOT vs the interpreter.** ROTT starting from boot under DPMI Direct, steps
  60–100 million: the interpreter took 353 ms and the module 287 ms (1.23×).
  AOT executed 15% of the instructions, about 660 per call.
- **JIT vs the interpreter.** On a ROTT gameplay state, the Cranelift JIT took
  360 ms plus 600 ms of compilation for 20 million steps that the interpreter
  ran in 205 ms.

The JIT is therefore no longer part of the default build. To compare it, build
with `--features dynamic-core` and launch with `--dynamic-core`.
`--no-dynamic-core` is still accepted and has no effect.

By default, MIDI from the guest is rendered by a basic procedural synthesizer
and mixed with Sound Blaster PCM and AdLib/OPL3 FM on the default audio device.
FM is included when **SB16** is enabled in Settings. Select AdLib or Sound Blaster
music in the game's own setup to use FM; the MIDI output selector does not route
FM through an external MIDI device. No OPL instrument ROM is required.
The bottom bar shows a 16-band spectrum of the local audio output, from bass
on the left to treble on the right. It fades during silence and clears on pause
or mute. Audio produced by an external MIDI synthesizer is not measured.
Hover over the audio indicator to see the guest PCM format, for example
`22.05 kHz · 8-bit · Mono`, or whether PCM has not been configured yet.
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

Choose **Audio → Playback → Mixer** on Linux, or pass `--audio-mixer application|system`.
**Internal** is the default and combines sources inside Doslet. **System
(experimental)** sends three independent stereo float streams to the
PulseAudio-compatible server: **Sound Blaster PCM** at the guest DSP rate,
**AdLib / OPL FM** at 48 kHz, and **MIDI synthesizer** at 48 kHz.
PCM is decoded and converted to float without application-side resampling.
The existing FM and MIDI engines retain their internal rate conversion.
Changing the PCM rate drains and recreates its server stream; a brief gap is
possible. Each stream and its sample buffers are created on its first source
samples. External MIDI ports retain their separate routing and do not open the
local MIDI stream. Sources that have never produced samples remain dormant.

```sh
cargo run -p doslet --release --locked -- --audio-backend pulse --audio-mixer system
```

System mixing requires the Rust PulseAudio adapter regardless of the backend
selected for application mixing. First-use stream setup failure is reported without
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

The CPU indicator shows the configured MHz in normal mode, the current target
in adaptive CPU mode, or the Grey target in speed-tier mode. Automatic modes
add **Auto**. Hot is the turbo rate for selected code; it appears in the tooltip
alongside the Cold and Grey targets. Hover for these targets or the adaptive
CPU ceiling; click to configure CPU execution.

The CPU panel offers fixed speed, Hot/Grey/Cold rates, and the experimental
adaptive mode. Logarithmic sliders cover 1–1000 MHz; Hot can also run uncapped.
Grey is the reference speed. Native builds offer the interpreter, or show
**AOT · Wasmtime** while a cached procedure module is installed (see
[Execution core and AOT](#execution-core-and-aot)). Builds with the optional
`dynamic-core` feature can also select the slower Cranelift JIT. Browser builds
offer fixed speed and the interpreter. **Apply** changes execution without restarting the program;
**Revert** discards the draft. Controls are unavailable during machine
preparation or an attached GDB session.

On native builds, choose **Create profile** in the CPU panel, enter a JSON file
path, then **Create & use**. This saves a speed-tier profile and activates it
without restarting. Learning remains off until explicitly enabled.
**Save profile as…** saves a copy of the
current labels and tier speeds. Existing files are preserved; choose another
name or use **Open profile…**. Opening a profile restores its labels and saved
speeds without enabling learning. Profiles without saved rates use the draft
tier speeds. The JSON is also accepted by native `--speed-tier-profile`;
CLI rate options retain their existing defaults and overrides. Learned changes
are persisted separately in the game cache or the profile's `.learned.json`;
use **Save profile as…** to export the current combined labels.

Tier learning is **off by default**, including when cached labels are restored.
To record or extend a game's profile without preparing a JSON file, run:

```sh
cargo run -p doslet --release --locked -- \
  --config games/Quake.toml --learn-speed-tiers \
  --hot-mhz 500 --grey-mhz 266 --cold-mhz 8
```

The application restores the executable's cached labels, or starts an untrained
profile, and saves updates in the per-game cache. `--speed-tier-cache DIR`
selects a different cache directory. Existing `--speed-tier-profile FILE.json`
commands still work. Without rate overrides, recording retains cached rates;
a new profile uses Hot/Grey/Cold at 500/133/8 MHz.

When a learning window qualifies, guest execution and playback stop while the
paired diagnostic replays run synchronously. New labels are installed before
execution resumes. No snapshots queue up, and the pause does not become guest
catch-up time. A replay can take up to 120 host seconds; failures retain the
existing labels. Turning learning off prevents subsequent replays. Rates and
core selection remain active across an in-session restart.

In speed-tier mode, CPU activity is measured against the Grey throughput budget
for a recent 250-ms guest-time window. Each executed scheduling unit costs four
clocks at the Grey rate. This weights Hot and Cold by their speed relative to
Grey, including uncapped Hot through actual execution counts. Accelerated idle
passes are excluded. With Hot/Grey/Cold at 500/100/8 MHz, all-Cold execution shows
**8% activity / 92% Idle**, while all-Grey shows **100% activity**. Hot can consume
more than the reference budget; the displayed value is clamped to **0–100%**.
The bar partitions activity into orange Hot, blue Cold and grey unclassified
code (including IRQ-labelled code); dark space is unused Grey capacity, labelled
Idle. On overload the bar is full and scales active colors proportionally.

Normal mode retains **100% minus detected idle**, including proven loop skips
and enabled cooperative waits. These values do not measure host CPU utilization.
A dash means no current sample, including while paused, waiting for storage or
collecting the first window. Hover for the breakdown; the execution table is
under **Info**.

The RAM indicator follows CPU and shows installed guest RAM in a fixed-width
label. Click to inspect DOS allocated/free memory, the largest free conventional
block and block count, plus CPU mode and DPMI status.
For the built-in DPMI host it also shows pool capacity, requested and committed
bytes, available bytes, peak allocation and block count. An available host is
distinct from a client that has entered DPMI. Guest-managed extended-memory
allocation is unknown; protected mode alone does not prove DPMI use. These
counters exclude allocator headers and system reservations and do not measure
host RAM usage.

Audio follows RAM. Its fixed-width **SB16 / OPL3 / MIDI** labels show enabled
interfaces; disabled interfaces are hidden. MIDI turns green for new notes and
amber for other traffic. The spectrum measures local output, excluding external
MIDI sound. Click to open **Audio**:

- **Playback** selects internal or Linux system mixing, local output
  volume, synthesizer/MIDI port, game format and external receiver format.
  MIDI routing controls appear only when MPU-401 is enabled in the active
  machine. Internal volume applies to local PCM, FM and synthesized MIDI; external MIDI
  devices retain their own volume. CM-32L synthesis follows the selected game
  format; a GM synth translates MT-32 input to GM. Changing the Linux mixer
  pauses the game. Use the toolbar to resume after closing the panel.
- **Hardware** keeps Revert and Apply in a fixed footer and edits a draft: SB16 with OPL3, MPU-401 MIDI, IRQ and DMA.
  SB16 and OPL3 share their enable switch. Ports are currently fixed (SB16 220h,
  OPL3 220h/388h, MIDI 330h). **Apply & restart** validates and recreates the
  machine; **Revert** restores active settings. Unsaved changes require export
  or explicit discard. Live MIDI choices are retained when applying hardware.

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

Click the monitor icon to open **Display**, then choose scaling and filtering.
The adjacent fixed-width label reports the guest mode: **80×25 VGA text**,
**320×200×8 VGA** or **640×480×8 SVGA**. Graphics depth is in bits per pixel;
the label does not depend on window scaling or display filters. MDA, CGA and
EGA modes are not currently implemented.
**Pixel perfect** selects the largest whole-number zoom that fits the available
display area, measured in physical pixels, including on HiDPI displays. It does
not resize the window. A 320×200 frame can become 640×400 or 960×600; a 640×480
frame can become 1280×960. The raw raster proportions are preserved, including
720×400 text. If even 1× does not fit, the display scrolls. **Fit** fills the
available area at 4:3 proportions and permits fractional scaling.

The filter selector in the same panel works on desktop and in the
browser, including while paused:

- **Nearest** preserves the original colors and pixel edges. Use it with
  **Pixel perfect** for an exact integer enlargement.
- **Sharp Bilinear** first repeats pixels by whole factors, then blends the
  remaining fractional scale. This keeps pixel interiors sharp while softening
  boundaries. The intermediate enlargement is capped at 4× per axis.
- **HQ2x** uses the HQx edge-aware algorithm to enlarge the frame 2× before
  linear sampling to the selected display size. It smooths diagonals and can
  change the appearance of small fonts.
- **xBRZ (GPU)** is available on desktop and in the browser. Its shader smooths pixel-art edges at
  the selected display size, including fractional scales, without CPU upscaling.

Scaling and filtering are independent. Filters operate on presentation snapshots
in the UI; guest execution and mouse coordinates still use the original frame.
Filtered images are cached until the source frame, filter or intermediate zoom
changes. Sharp Bilinear and HQ2x use CPU preprocessing on both platforms;
xBRZ runs through the GPU renderer. In the browser it requires WebGPU or WebGL2;
if the application falls back to Canvas2D, selecting xBRZ shows a notice and
keeps the current filter and image. Their live frame rate has not been benchmarked.

Presentation keeps the previous image visible until the renderer has uploaded
its replacement. At most one upload is in progress; subsequent frames replace
the waiting frame, so a slower renderer does not accumulate a playback queue.
An upload failure keeps the last ready image and shows an error. Selecting a
filter or scaling mode retries the upload.

### Machine configuration and saves

The drive icon after Display opens **Drives**. Its lamp pulses for 120 ms after
guest data reads from C: or CD-ROM, including cached data and MSCDEX sector
reads. It indicates guest I/O, not physical host disk access, and goes dark
while paused. C: is the startup drive: edit its folder/program (desktop) or
choose a replacement ZIP/program (browser), plus its write policy and save store.
**Add CD-ROM** mounts a local ISO or HTTPS ISO; **Remove** disconnects that image
without deleting files. Doslet currently supports C: and one optional CD-ROM.
Edits remain a draft until **Apply & restart**; **Revert** restores active
mounts. Unsaved game changes retain the usual export/discard protection.

To resume a portable machine snapshot in the native window, use
`target/release/doslet --load-state target/heroes-states/city.pb.gz`.
See [portable states](save-states.md#resume-in-the-native-window) for restore
semantics and the optional static hot/cold/grey experiment.

Open **Info → Settings…** in either frontend to edit a draft, validate its
resources and **Apply and restart**. The editor configures RAM, x87, SB16 and
MPU-401 hardware, AT/BIOS keyboard input, a two-axis/two-button gameport, host
output, installation media and save policy. Native Apply prepares sources on a
separate thread; failed preparation preserves the active session and reset
settings. **Fit**, **Pixel perfect** and display filters remain available in **Display**.

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
Native Doslet automatically discovers compatible CM-32L ROMs in DOSBox folders.
On Linux it searches `$XDG_DATA_HOME/dosbox/mt32-roms`,
`$XDG_DATA_HOME/mt32-rom-data`, `$XDG_CONFIG_HOME/dosbox/mt32-roms`, and
`mt32-rom-data` under `$XDG_DATA_DIRS`. Unset XDG variables use the standard
`~/.local/share`, `~/.config`, `/usr/local/share` and `/usr/share` locations.
It also checks `~/.dosbox/mt32-roms` and `~/.dosbox`. On macOS it checks
`~/Library/Preferences/DOSBox/mt32-roms`, `~/Library/Audio/Sounds/MT32-Roms` and
the system `mt32-rom-data` folders. On Windows it checks
`%LOCALAPPDATA%/DOSBox/mt32-roms` and `%APPDATA%/DOSBox/mt32-roms`.
See the [DOSBox Staging ROM lookup paths](https://www.dosbox-staging.org/preview/dev/0.83/manual/sound/sound-devices/roland-mt-32/#rom-lookup-paths).

`--mt32-rom-dir` or a profile ROM directory takes precedence over discovery.
Automatic discovery skips incomplete or unsupported ROM pairs and retains the
Rust synth when none is found. `--no-audio` skips discovery. With a detected or
explicit ROM directory, the default native output is moont; `--midi-output rust`,
`--midi-output apple` or a port explicitly selects another output. The ROM path
remains available when switching back to moont. In GM mode, moont uses GmDevice: CM-32L instruments
and eight melodic parts, rather than a complete General MIDI sound bank.

The ROM directory must contain `CM32L_CONTROL.ROM` (v1.02) and
`CM32L_PCM.ROM` (names are case-insensitive; symlinks are supported).
Doslet validates their SHA-1 fingerprints and loads them at
startup. The ordinary Doslet builds do not embed or distribute ROMs. The optional `no_std` Rust
[moont engine](https://crates.io/crates/moont/1.0.0) emulates CM-32L, an MT-32
family module. It processes the game's custom instrument SysEx and renders
32-kHz stereo, resampled to the output device. MT-32 Control ROM variants and
the module's LCD are not emulated by this backend. `--mt32-rom-dir` selects
native audio. The private ROTT browser package built below includes the user's
local ROM pair and uses it in an AudioWorklet; a general browser ROM picker is pending.

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
The browser prefers iced's wgpu renderer through WebGPU or WebGL2, with
tiny-skia/Canvas2D as a software fallback. The native frontend uses wgpu. Browser startup diagnostics are logged
to the console.

To build a private ROTT page with the local `games/ROTT.toml`, which queues the
BIOS menu characters `1`, then `5`, run:

```sh
cargo run -p doslet-web --release --locked -- build games/ROTT.toml \
  -o target/rott-site
```

Serve `target/rott-site` at the web root over localhost or HTTPS and click
**Uruchom grę**. The packager merges the existing `games/rott-saves` overlay
into its single game ZIP; later browser saves use IndexedDB. The package
contains the commercial game, so keep it private.

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
