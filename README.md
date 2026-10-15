<p align="center">
  <img src="assets/banner.svg" alt="Doslet — A little DOS. Room to explore." width="880">
</p>

<p align="center">
  <strong>A modular DOS emulator in Rust, with an iced desktop and WebAssembly frontend.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-GPL--2.0--or--later-58dfbe?style=flat-square" alt="GPL-2.0-or-later"></a>
  <img src="https://img.shields.io/badge/status-early_development-97a6b9?style=flat-square" alt="Early development">
  <a href="docs/SPEC.md">Specification</a> ·
  <a href="docs/architecture.md">Architecture</a> ·
  <a href="docs/compatibility/prince.md">PRINCE compatibility</a>
</p>

Doslet is being built around reusable emulation components: a CPU that knows a
bus, chips that know their own registers and signals, and a machine that connects
them. The application adds a workspace and a guest display on top.

**This is a working bootstrap, not a complete DOS emulator.** The included,
original COM demo executes a small 16-bit instruction subset, invokes BIOS video
services, fills VGA memory with color bars, and exits through DOS. Machiavelli the
Prince now reaches its logo, introductory letter and menu. Interactive gameplay
and saves remain pending.

## Start exploring

Install [Rust through rustup](https://rustup.rs/). The repository pins Rust 1.93.1,
rustfmt, Clippy and the `wasm32-unknown-unknown` target in
[`rust-toolchain.toml`](rust-toolchain.toml).

Run the guest demo without a window:

```sh
cargo run -p doslet-headless --locked -- demo
```

Open the desktop workspace:

```sh
cargo run -p doslet --locked
```

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
automatically upgrades it to MT-32 and shows that change in the inspector.
Rust synth and GM host ports use **MT-32 → GM (przybliżenie)** translation in
MT-32 mode. This approximation needs no ROMs and does not reproduce LA synthesis.
The inspector also shows SysEx counts, unmapped notes and the module's LCD text.
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
[CIV evidence](docs/compatibility/civ.md) for commands and limits.

[HEROES startup evidence](docs/compatibility/heroes.md) records the x87 fixes
and remaining CPU requirements. [TOML launch profiles](docs/launch-profiles.md)
store native game/audio/MIDI settings, DOS arguments and a C: directory mount.
CLI options override profile settings:

```sh
target/release/doslet --config games/CIV.toml
target/release/doslet --config games/PRINCE.toml --no-audio
target/release/doslet --config games/HoMnM.toml
```

The local game installations stay ignored. HoMnM's profile mounts its
`homm1.gog` ISO and exposes MSCDEX sector services plus read-only DOS file
access. The recorded run passes its CD check; gameplay remains unverified.

Guest-created files use the reserved RAM overlay. They can be reopened during
the session; Reset or exit discards them. Persistent host saves remain pending.
`--no-audio` retains a silent path; choose **1 — No sounds please** in CIV.

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
queued messages and silence the selected port. The inspector reports queued
messages and dispatches more than 2 ms late, with maximum dispatch lateness.
These counters measure Doslet scheduling, not the external synth's PCM buffers.

Press **Run** to execute the built-in demo, **Pause** to suspend execution, **Step** to
advance one execution step, or **Reset** to create a fresh session. Linux builds
require a C linker, `pkg-config` and ALSA development files (`libasound2-dev` on
Ubuntu/Debian); launching requires a working Wayland or X11
session and graphics driver. Dependency resolution and native compilation are
separate from checking a live window.

On desktop, **1:1** beside **GUEST DISPLAY** resizes the window to show each guest
pixel as one physical screen pixel, including on HiDPI displays. It uses the raw
image dimensions (for example, 720×400 for text) and allows scrolling if the
window is too small. **Fit** restores automatic scaling with 4:3 proportions.

Build the browser application with [Trunk](https://trunkrs.dev/):

With mise, install the explicitly selected Rust tools and start the web task:

```sh
mise install cargo:trunk cargo:wasm-bindgen-cli github:WebAssembly/binaryen
mise run web
```

`mise.toml` pins Trunk, wasm-bindgen-cli and Binaryen (`wasm-opt`). Keep
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
In Doslet, choose **Open game ZIP**, select **PRINCE/PRINCE.EXE** (or the relevant
EXE/COM), then press **Start game**. The selected program's containing directory
becomes C:, with its subdirectories preserved. ZIP data stays in the browser;
there is no upload, and commercial files are not included in the web build.

Stored and Deflate ZIP entries are supported. The import accepts at most 64 MiB
of ZIP data, 4096 entries, 64 MiB per file and 256 MiB of expanded files. DOS
paths must use ASCII names; duplicate case-insensitive names, symbolic links,
encrypted files, absolute paths and parent-directory traversal are rejected.
Unsupported compression and corrupt data produce an error.

Reset recreates the running game's drive from the original ZIP. Guest writes
use a separate 1-MiB RAM overlay and are lost on reset or page reload; creating
new files are supported in that overlay. Persistent storage remains pending.
BIOS/DOS keyboard input and DOS mouse-driver input are mapped from the UI. **Start game** creates and resumes Web Audio for the actual
guest MIDI stream. If the browser keeps audio suspended, click **Enable audio**;
the same button offers **Mute** / **Unmute** once audio is running. The inspector
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

## What exists today

| Component | Implemented | Still needed |
| --- | --- | --- |
| x86 CPU | Selected real/protected-mode 386 operations, cached GDT/LDT segments, EIP32, 16/32-bit ModR/M/SIB, same-privilege gates, bounded REP | Full instruction/privilege coverage, paging, precise faults, timing |
| x87 | Independent control/status/stack, exact signed integer loads, selected finite arithmetic and rounded normal memory stores, constants and infinity comparison | Full extended exponent range, subnormal arithmetic/stores and exception delivery |
| i8042 / CMOS | Controller/A20, keyboard command/ACK protocol, CMOS register bank and memory-size fields | Raw keyboard/mouse protocols and IRQs, RTC clock/IRQ behavior and persistence |
| PIC | Master CPU interrupt delivery; independent slave register bank at A0h/A1h; masks, fixed priority and EOI | Cascading and remaining operating modes |
| DMA | Independent 8237 state, PC page registers, bounded byte/word RAM transfers and auto-init | Full arbitration/cascade timing, MMIO and memory-to-memory |
| PIT | Binary modes 0/2/3, latches, gates, channel-2 output at 61h, fractional guest clock input and IRQ0 | Modes 1/4/5, read-back, BCD and precise periodic load timing |
| VGA/VBE | Mode-13h and banked 640×480×8 pixels, palette writes, borrowed scanout, configuration readback, selected VBE 1.2 BIOS calls | Text/planar modes, register rendering effects, raster timing, more SVGA modes |
| DOS/BIOS | COM/MZ/overlay setup, bounded memory arena, immutable input files/directories, optional sparse RAM writes, timer/user-hook shims | Full DOS kernel, extender compatibility, raw mouse hardware, persistent storage |
| MPU-401 / MIDI | UART setup, bounded output at 330h/331h; independent procedural synth, guest-timed WAV, native and Web Audio output | General MIDI instrument fidelity, serial timing, IRQs and intelligent mode |
| Sound Blaster 16 | Independent DSP/mixer, selected DMA PCM playback, auto-init/pause/resume, block IRQs and bounded output queue | More commands, precise timing, mixer gain, OPL and host audio |
| Frontends | Shared iced desktop/web view, native `--game`, browser Worker/AudioWorklet, ZIP mount/program selection, optional system MIDI on both platforms, headless CLI | Guest input, native CPU thread runner |
| Executable inspector | Borrowed MZ metadata/relocations and LE/LX header detection | LE/LX loading and execution |

Run a standalone real-mode MZ program with a finite execution budget:

```sh
cargo run -p doslet-headless --locked -- run-mz program.exe --steps 200000
```

The loader validates the declared image, memory requirements, relocation targets,
entry and stack before guest writes. The CPU still supports a selected 16-bit
subset; unsupported instructions and services stop execution explicitly. Overlays
and NE/PE/LE/LX programs are rejected. The original test program in
[`tests/fixtures/relocated_mz.rs`](tests/fixtures/relocated_mz.rs) uses a relocated
data segment to print `Relocated MZ` and exits through DOS with code 7.

Trace a DOS startup stub with its containing directory preloaded as C: input:

```sh
cargo run -p doslet-headless --locked -- run-stub PRINCE/PRINCE.EXE --steps 20000000
```

`run-stub` initially loads the MZ portion of an LE/LX program, supplies a DOS
environment and supports loading MZ overlays from that input drive. A guest
extender can then load the extended image through DOS services. The runner skips
symlink entries within the directory. Existing guest files may open read/write
using a 1-MiB sparse RAM overlay; original host files and buffers remain immutable.
`--trace N` prints at most N initial step snapshots; `--trace-tail` retains the
last 64 snapshots. `--peek HEXADDRESS:DECIMALLENGTH` prints a final borrowed RAM
view, limited to 128 bytes per range. The current local PRINCE run loads DOS4GW,
prints its banner and reaches the game's flat 32-bit initialization and device
probes. It now renders the HDI logo, introductory letter/map and main menu.
`--midi-log PATH` records the actual UART bytes with emulated nanosecond timestamps,
draining the bounded MIDI queue; it provides diagnostic evidence, not sound.
Without a MIDI consumer the queue eventually backpressures the guest.
`--midi-wave PATH` renders the guest UART stream into a 48-kHz stereo PCM WAV
without independently loading a MIDI file. Unsupported behavior still stops
with a diagnostic. The native frontend uses the same game loader; see the
[recorded startup result](docs/compatibility/prince.md#doslet-startup-attempt).

To measure execution separately from indexed-to-RGBA conversion:

```sh
cargo run -p doslet-headless --release --features profiling --locked -- run-stub PRINCE/PRINCE.EXE --steps 300000000 --profile-cpu
```

The optional feature counts decoded-instruction cache hits, misses, invalidations
and fallbacks, CPU/IRQ/HLT scheduling units, skipped VGA-wait units
and batches, and actual dispatched bus reads,
samples guest PCs at varying intervals, and drains MIDI without synthesizing it.
The timed CPU boundary excludes sampling, UI rendering and host audio. A game
still running at the finite step limit reports budget exhaustion and exits with
code 1 after printing its profile. Ordinary builds contain no profiling counters.
Interrupt-enabled HLT waits can advance together up to the next PIT/DSP deadline,
the remaining step budget, or 1 ms of guest time. Confirmed
`IN AL,DX; AND AL,08h; JZ/JNZ -5` and `IN AL,DX; TEST AL,AH; JZ/JNZ -5`
loops (AH=08h for TEST) on the active VGA status port can batch
whole iterations strictly before the next raster/PIT/DSP event; other polling
loops execute normally. Append `--no-vga-waits` to disable recognition and
batching for reference measurements. [The driver report](docs/compatibility/prince-svga.md)
records the supported scope and differential checks.

For diagnostic inventories of game polling, the optional `inspect_civ` wait
tracer and Python/objdump scanner correlate backward branches with executed I/O.
See the [Heroes wait inventory](docs/compatibility/heroes-waits.md) for commands,
addresses and the distinction between confirmed waits and ordinary repeated work.
The native `--experimental-heroes-timer-waits` flag optionally batches three
version-specific Heroes countdown waits. It is disabled by default; the terminal
reports the first actual batch. These sites were inactive in the sampled main
menu, so the flag does not promise a reduction in menu CPU usage.

Selected instructions in default-32-bit code use a fixed single-instruction
decode cache shared by native and WASM builds. Page write generations, A20 mapping
and CS context validate entries. Other forms retain the interpreter; REP and IRQ
boundaries stay unchanged. Append `--no-decode-cache` for a reference run using
the same bus and generation tracking. This comparison measures the cache's
execution benefit; generation tracking also has a cost relative to older builds.

Run the opt-in local comparison through 300 million PRINCE steps:

```sh
cargo test -p doslet-headless --release --features profiling --locked local_prince_decode_cache_matches_reference_during_execution -- --ignored --nocapture
```

It compares registers, flags, control state and guest time after varied bounded
batches, all emitted MIDI/PCM, periodic RAM/scanout/palette snapshots and full
final ordinary RAM and all physical VRAM. This headless check does not measure UI, host synthesis or
browser throughput. Guest-PC sampling counts guest activity; it does not locate
host CPU time by emulator function.

## A local game target

Keep your copy of the game in `PRINCE/`, or use a symlink to its installed
directory. That path is ignored by Git. The project does not redistribute game
executables, assets, configuration or saves.

```sh
cargo run -p doslet-headless --locked -- inspect PRINCE/PRINCE.EXE
```

The supplied executable has a DOS MZ stub and an appended LE program. Together
with `DOS4GW.EXE`, this establishes that the target goes beyond the current
real-mode demo. The [compatibility report](docs/compatibility/prince.md) records
the inspected file, evidence, unresolved requirements and acceptance stages.

## Components that can travel

```text
crates/
├── foundation/          bus contracts and integer emulated time
├── cpu/                 reusable x86 interpreter and architectural state
├── hardware/
│   ├── controllers/     independent PIC and PIT libraries
│   ├── audio/           independent MPU-401 UART
│   └── video/           VGA memory, palette and scanout
├── machines/            PC composition and memory/port routing
├── system/              executable inspection and DOS process setup
└── runtime/             bounded sessions and BIOS/DOS service adapters
apps/
├── doslet/              iced frontend and pixel presentation
└── doslet-headless/     demo, COM/MZ runners, inspector and benchmark
```

All emulation and MIDI synthesis libraries compile with `no_std`; components that own buffers
use `alloc`. CPU execution uses static bus dispatch. Scanout borrows device
memory, and RGBA conversion writes into caller-owned storage. A regression test
checks **zero heap allocations while running the demo and converting its frame
after setup**, and while executing the relocated MZ fixture with console output.
It also covers mounted-file reads, MIDI queue output, protected execution and
banked VBE pixels with full scanout conversion after setup.

The iced image adapter currently creates an owned RGBA image when the guest frame
changes. That presentation boundary has an explicit copy and allocation cost;
the emulator does not clone RAM or framebuffer storage per execution step.

## Check the work

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check -p doslet -p doslet-time -p doslet-executable --target wasm32-unknown-unknown --locked
cargo check -p doslet-web-worker -p doslet-web-audio --target wasm32-unknown-unknown --locked
cargo run -p doslet-headless --release --locked -- bench --steps 5000000
```

The benchmark reports host execution steps per second, not emulated MHz or a
game performance claim. REP iterations count as separate execution steps so a
batch stays bounded.

After building the web bundle, run the optional real-browser check with Firefox
and Python's standard library (no Node, Selenium or package installation):

```sh
python3 tests/browser_smoke.py
python3 tests/browser_smoke.py --prince
```

The second command needs the local `target/prince-game.zip` created by
`mise run package-prince`. The isolated headless browser mutes its audio sink;
it checks nonzero rendered samples, ZIP execution, transfer channels,
pause/resume/reset, output recovery, and continued audio/CPU progress during an
800-ms window-thread stall. Web MIDI uses a mock device, including a port without
`clear()`; MT-32 checks cover automatic upgrade, translation, RPN replay and raw
SysEx output with explicit permission; this does not establish playback on a real external synthesizer.

Capture the guest framebuffer without opening a window:

```sh
cargo run -p doslet-headless --locked -- demo --frame /tmp/doslet-demo.ppm
```

[`mise.toml`](mise.toml) provides the same workflows as `mise run test`,
`mise run check`, `mise run demo`, `mise run run`, `mise run web` and
`mise run inspect-prince`. `mise run run` uses the optimized release profile;
`mise run run-debug` retains the debug profile for development.

## Development direction

The next milestones are broader real-mode execution and DOS file/memory services,
then 386/protected-mode execution and the extender and graphics
path required by PRINCE. The [specification](docs/SPEC.md) defines the completion
criteria. Optimized decoded-block execution and optional native JIT backends
follow correctness and profiling evidence.

## License and provenance

Doslet is licensed under [GPL-2.0-or-later](LICENSE). Files that include
third-party code keep their original copyright notices, and third-party
libraries keep their own licenses. The
original demo is part of Doslet. The commercial game is an external test input.

Decode-cache profiling distinguishes CS context transitions and discarded entries
from page-write, mapping, bus-owner and access invalidations. These counters count
rejected entries, not writes to guest code. Collisions count lookups at occupied
slots with another key; evictions count successful replacements. Decode attempts
and failed decodes expose repeated unsupported-form probing. `fallback_hits` is
reserved for experiments and remains zero in the current implementation.
