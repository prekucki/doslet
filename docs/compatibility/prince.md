# Machiavelli the Prince: compatibility target

## New-game startup after memory and controller fixes

Local PRINCE.EXE is an external test input. Its files were read without changes.

A headless execution reproduced Start New, the default setup, family name and
crest selection. At 1101890059 steps the game reached its fatal error handler
with "Critical memory shortage!". The failing array had 54 successful buffers
out of 233, each requiring 64 DOS paragraphs. The 64-entry conventional-memory
block table exhausted its metadata capacity; this was not evidence that the
machine's 16 MiB of physical RAM was too small. During fatal cleanup, the guest
also accessed missing keyboard command handling and the slave PIC mask at A1h.

The DOS block table now has 1024 setup-owned descriptors. Keyboard commands at
60h acknowledge LED, typematic, scan-set, identification, scanning/defaults,
echo, resend and reset/self-test operations. Slave PIC register accesses at
A0h/A1h are independent of the master. Slave IRQ sources and cascade delivery
remain pending.

Repeating the same game inputs after the changes reached the Venice map at
1200000000 steps with no fault or critical-memory message. The guest MCB chain
contained 243 blocks and 192864 free conventional-memory bytes. This verifies
new-game startup, not a complete campaign or hardware audio playback.

Reproduction (writes diagnostics to /tmp, never the game directory):

```sh
DOSLET_PROBE_FRAME=1 DOSLET_PROBE_RAM=1 \
DOSLET_CIV_POINTER_EVENTS='450000000:320:152:0:1,451000000:320:152:0:0,650000000:320:456:0:1,651000000:320:456:0:0,850000000:320:321:0:1,851000000:320:321:0:0,1050000000:280:225:0:1,1051000000:280:225:0:0,1100000000:320:336:0:1,1101000000:320:336:0:0' \
CARGO_TARGET_DIR=target/civ-probe cargo run -p doslet-headless --release --locked \
  --example inspect_civ -- PRINCE/PRINCE.EXE '' 1200000000 \
  '310000000:1c:0d,330000000:1c:0d'
```

The probe supports DOSLET_PROBE_TRACE_AFTER to single-step only the final
interaction and stop at the local PRINCE error-handler address. That address is
specific to this game version. Frame and memory diagnostic dumps remain local.

## Earlier startup observations

Status: actual HDI logo, introductory letter/map and main-menu frames have been
captured through guest execution. Music files open through a sparse RAM overlay
and the real MPU stream contains note events. The desktop CLI can start the game;
a 300M-step opt-in test through UI batches observes nonblank pixels, MIDI notes
and successful game Reset. A recorded live screenshot also confirms the
introductory letter/map in iced and nonzero audio consumed by the host callback.
The browser Worker/AudioWorklet path also runs the actual ZIP to 300M steps,
640×480 graphics and 269 note-ons. Listening checks reported Rust-synth sound and graphics in Firefox and Chrome,
and native Rust-synth playback. Native MIDI playback was also reported; this
does not establish instrument fidelity.
Original instrument fidelity and interactive gameplay remain unverified. See
[the music inventory and synth-selection analysis](prince-music.md).
The [original DOS toolchain inventory](prince-toolchain.md) records Watcom,
DOS/4GW and sound/graphics provenance separately from unverified authoring tools.
Inspection date: 2026-10-14.

## Local input

The supplied `PRINCE` entry is a symlink to a local game installation. Doslet
keeps it ignored by Git and uses it as a read-only input for authorized local
compatibility runs.
No commercial files are included in tests, CI, the browser bundle or commits.

| Property | Observed value |
| --- | --- |
| Executable | `PRINCE/PRINCE.EXE` |
| File size | 931229 bytes |
| SHA-256 | `b1e6c5622d7baafc488eb3fb8f50f51c9a8ca23f3dfde572c5620103df4248fc` |
| MZ header | 96 bytes; 6 relocations |
| Declared MZ image | 10830 bytes |
| Initial MZ CS:IP | `0000:030E` |
| Initial MZ SS:SP | `02A5:0800` |
| Extended header offset | `0x2A50` (10832) |
| Extended signature | `LE` |
| LE CPU / OS codes | `2` / `1` |
| LE entry object / offset | `1` / `0x81074` |

The SHA-256 identifies the inspected local file; it is not a required hash for
every edition of the game. The inspector currently prints header metadata, not
a computed hash.

Reproduce the header check from the repository root:

```sh
cargo run -p doslet-headless --locked -- inspect PRINCE/PRINCE.EXE
```

## Evidence and implications

| Evidence in the local installation | Architectural implication | Confidence / limit |
| --- | --- | --- |
| `PRINCE.EXE` has an appended LE header; its stub contains a Watcom runtime string | A 16-bit real-mode CPU is insufficient | Confirmed file structure; protected-mode startup remains incomplete |
| `DOS4GW.EXE` is present; the game executable contains `RATIONAL DOS/4G` | Plan for 386/protected mode and DOS/4GW-compatible startup/services | Strong static evidence; exact DPMI/VCPI path must be observed |
| Graphics drivers include `VS640480.386`, `S3640480.386`, `AT640480.386` and others; executable contains `VESA` and `SVGATILE.BIN` | Plan for 640×480 graphics and investigate VESA/SVGA/banked memory | Static indicators, not a verified trace of selected mode |
| `JVSOUND.CFG` selects `A220 I7 D1 H5 T6`, `mpuio=330`, `midi=mpu401`, `wave=blaster` | Plan for Sound Blaster-compatible audio, DMA/IRQ and MPU-401 MIDI | Confirmed configuration; actual device accesses remain untraced |
| The game readme discusses DOS extender, mouse-driver and memory-manager compatibility | Include mouse services and extender/memory-manager interactions in acceptance | Confirmed documentation; required call coverage remains unknown |

These observations set the implementation direction. They do not establish a
complete list of interrupts, ports or instructions used by the game.

## Doslet startup attempt

### Browser Worker and Rust/WASM AudioWorklet

```sh
python3 tests/browser_smoke.py --prince
```

After `mise run web-build`, using the ignored local `target/prince-game.zip`,
headless Firefox ran the actual game in the WASM Worker. The latest recorded
sample reports Running at 300240000 execution steps / 36392727272 guest ns,
nonblank 640×480 graphics, 269 note-ons, 1744384 rendered audio frames,
peak 0.391588 and zero worklet underruns. The sink was muted in the isolated
profile; this establishes executed synthesis rather than independent listening.
Separate interactive checks reported working graphics/audio in Firefox and
working graphics/Rust-synth audio in Chrome. These reports do not establish the same
frame/step boundary as the scripted run.

The script also verifies continued Worker/audio progress during an 800-ms window
stall, pause/resume/reset, and Web MIDI framing/output replacement using a mock
device. Some Web MIDI output ports lack `clear()`; an adapter correction
now uses a bounded cancelable timer queue for those ports. The mock exercises
that case and recovery after a device fault without resetting guest RAM.
Native MIDI playback has a listening report; browser TiMidity listening
after the correction remains unrecorded. Host MIDI supplies no built-in OS
synthesizer.

### Native desktop launch

```sh
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE
```

The game starts automatically, with the installation mounted as immutable
input plus RAM writes. The default audio device receives procedural synthesis
of the guest MPU stream; `--no-audio` disables host audio. Basic instruments are
not full GM or MT-32 emulation. Keyboard/mouse input and saves remain pending.
The opt-in `local_prince_reaches_intro_through_ui_batches` test completed 300M
steps through UI scheduling/audio processing, observed nonblank 640×480 scanout
and MIDI notes, and verified Reset. It uses the local installation and opens no
window or host audio device. A subsequent live launch displayed the introductory
letter/map in iced. The captured screenshot records Running at 433520000 steps,
44100-Hz MIDI output, 375 note-ons, peak 0.39 and 2314742 consumed host audio
frames. This verifies display and nonzero callback output, not speaker audibility
or original instrument fidelity. The historical headless captures below remain
separate evidence.

Latest bounded result (2026-10-14), with immutable host input and sparse RAM writes:

```sh
cargo run -p doslet-headless --release --locked --offline -- run-stub PRINCE/PRINCE.EXE --steps 300000000 --midi-log target/prince-midi.log --frame target/prince-story-midi.ppm
```

The run remains **Running** at 300M steps / 36.363636363 s of functional emulated
time. It loads one extender overlay and generates **2228 MIDI wire bytes**;
the final input is **PAVANA.MID**, opened with DOS mode 02h and error zero.
The previous read-only policy rejected these music opens with error 5. The
reserved RAM overlay permits them while preserving every original input file.
The MIDI log records note-on/off, program, volume and pan messages from the
actual game. Its timestamp resolution is the bounded host batch (~1.212 ms).
The queue is drained to the log; that is not synthesis or audible music.

Directly inspected images from actual VGA scanout show the HDI logo at 60M
steps, the introductory letter on a map at 300M, and the main menu at 1.2B.
Local PPM/PNG captures live under ignored `target/` and are not distributed.
The earlier menu capture had failed music opens, so it establishes graphics only.
The current MIDI run also renders the letter/map. PCM remains silence: 776942
stereo frames with peak zero, drained without playback.

DMA, selected DSP playback, PIT mode 3, PIC delivery and clocked raster status
now allow the intro to proceed. A BIOS timer hook invokes the current INT 1Ch
handler. The clock is a deterministic functional model with four nominal 33-MHz
clocks per step; it is not precise physical timing. Mouse reset reports an absent
driver; there is no host mouse/input path yet. The UI still runs the synthetic
demo. Fresh checks passed 115 tests, Clippy, formatting and WASM compilation.
The full startup-demo-plus-audible-MIDI goal remains open.

### Earlier blank-frame / DMA boundary

The earlier bounded result (2026-10-14), with the containing directory mounted read-only:

```sh
cargo run -p doslet-headless --locked --offline -- run-stub PRINCE/PRINCE.EXE --steps 20000000
```

- **9300784 completed steps**, one DOS4GW MZ overlay; the budget is not exhausted.
- Actual DOS/4GW 1.97 startup banner appears on the DOS console.
- Protected flat 32-bit initialization executes with code selector 0150h and
  data/stack selector 0158h, base zero and limit FFFFFFFFh. GDT/LDT are at
  00100010h/3FFFh; IDT is at 000101C0h/07FFh. A20 is enabled.
- The game completes the 387 infinity detection sequence, uses DOS directory
  services, queries VBE, selects **101h (640×480×8)** and initializes SB16 DSP/
  mixer/rate state. The default PC reports DSP 4.05 and IRQ7/DMA1/DMA5 configuration.
- It stops on **unmapped I/O port D4h**, the second DMA controller's mask port.
  Fault-register CS:EIP is **0150:00203CA7** after instruction fetch;
  CR0=00000011h and EFLAGS=00000002h.
- Final scanout is 640×480 with **zero nonzero indexed pixels**; the pending
  MIDI queue contains **zero bytes**. Mode selection is not the startup demo.

Two read-only identity checks connect execution to the extended game image:
the LE entry at file A9474h matches loaded bytes at RAM 001F6074h; the x87 probe
at RAM 00216B0Eh matches a unique sequence in PRINCE.EXE at C9F0Eh, absent from
DOS4GW.EXE. This establishes game/runtime initialization beyond extender setup,
without claiming demo compatibility. Original input hashes below still match
after this run; PRINCE remains ignored and no input file was modified.

Protected segmentation/default widths/SIB, selected x87 operations and VBE/SB
control foundations are now implemented. DMA transfers, PCM/audio, timed IRQ
delivery, further game services and actual MIDI synthesis/playback remain open.
The UI still runs the original synthetic demo. Fresh validation passed 102 tests,
Clippy with -D warnings, formatting and frontend dependencies for WASM; these
checks are distinct from real game compatibility. No GUI reference run
or audible game music was recorded.

### Earlier 386/A20 boundary

The earlier verified result after the 386/A20 extension (2026-10-14): the same command
below completes **40170 steps**, loads one DOS4GW overlay, enables A20, loads
descriptor tables and sets CR0.PE. It then stops explicitly before the next
instruction at `1248:032D`. Reported state:

- CR0=`00000015h` (PE/EM/ET set), EFLAGS=`00000002h`, A20 enabled.
- GDTR base=`000109C0h`, limit=`02FFh`.
- IDTR base=`000101C0h`, limit=`07FFh`.

The CPU now executes selected dword instructions in real-mode code with genuine
32-bit register backing. The 386 model has no x87 and does not allow AC/ID flag
toggling. DOS4GW's DPMI, VCPI and XMS installation probes report those external
services absent. Unsupported BIOS INT 15h probes return AH=86h/CF=1. The
following startup uses the fast A20 gate and the keyboard-controller command
path, and reads CMOS memory-size fields matching the machine's 16 MiB RAM.

At this earlier boundary no protected instruction had been executed, and the LE game image, startup-demo
frames and audible MIDI remain unverified. The 6707-step result below is the
earlier baseline, superseded by this state. An intermediate run through a null
BIOS/multiplex vector produced misleading extra steps; explicit shims now stop
that path and return the defined probe/error statuses.

### Earlier overlay and 16-bit startup baseline

Recorded on 2026-10-14 using the local files and a preloaded, read-only drive:

```sh
cargo run -p doslet-headless --locked --offline -- run-stub PRINCE/PRINCE.EXE --steps 500000
```

Observed execution now enters the actual PRINCE MZ startup at `1010:030E`,
initializes the stack, resizes the process block, scans its DOS environment and
queries `dos4gw.exe` attributes/current directory. The startup relocates its own
working state toward the top of conventional memory, then calls DOS
`INT 21h/AX=4B03h` with the path `C:\dos4gw.exe`. Its parameter block supplies
load segment `1010h` and relocation segment `1010h`. This is the overlay path;
it should not be replaced with assumed EXEC/AL=00 child-process startup.

Doslet loads that MZ module through the ordinary memory bus and applies its
relocations. The old DOS4GW header places relocations across offset `3Ch`;
those bytes are not `e_lfanew`. A regression test now prevents the inspector
from treating them as an extended-header pointer.

The fresh run reports **one loaded MZ overlay and 6707 completed steps**, then
stops explicitly on opcode prefix `66h` at `11CD:6604` within DOS4GW. The
instruction there is a 32-bit accumulator store (`66 A3`). CPU detection code
nearby also uses 32-bit flags/registers. No protected-mode transition, LE image
execution, VGA/SVGA game frame or MIDI output from the game has been observed.
The 500000-step limit was not exhausted; the unsupported instruction stopped it.

| Input | SHA-256 after the run |
| --- | --- |
| `PRINCE.EXE` | `b1e6c5622d7baafc488eb3fb8f50f51c9a8ca23f3dfde572c5620103df4248fc` |
| `DOS4GW.EXE` | `dd9f4f342533f99570475b62e53231a468de2e3d83e4fc31e80c27d3a7d6b49c` |
| `JVSOUND.CFG` | `01f1ef4ffa7cab3129a15798413e78293b715ddcb879ac0f23d3d6fb5bcd2949` |

The PRINCE executable hash still matches the original inspection. The runner
only reads host files and does not execute the game outside the emulator.

MIDI remains part of the acceptance target. The local sound configuration uses
MPU-401 at `330h`, and the input includes `GM_LOGO.MID`, `MT_LOGO.MID` and other
MIDI pieces. The actual intro sequence and selected music have not yet been
traced. The independent MPU-401 UART now routes synthetic guest bytes through
ports `330h/331h` to a bounded output queue. That proves the device data path;
it provides neither synthesis nor audible game-music evidence.

## Acceptance stages

1. Record a fixed-speed reference run in Doslet: configuration, executable identity,
   selected video/audio paths, title/menu screenshots and startup diagnostics.
2. Run the actual MZ stub/extender startup in Doslet; report unsupported
   instructions, services and device accesses with guest addresses.
3. Reach the real title/menu screen using the game's file and graphics paths.
4. Start a game, navigate the map and complete one turn with mouse input.
5. Save to a separate writable overlay, restart and load that save. Preserve the
   original installation as a read-only input.
6. Verify pause/resume, repeated sessions, bounded memory and audio behavior.
7. Repeat the playable path in the browser using explicit asset import and
   browser storage. Obtain audio access through user interaction.

Only completed stages with recorded evidence may be marked supported. The COM
color-bar demo, successful WASM compilation and LE header inspection are useful
bootstrap checks, but none is a game run.

The earlier relocated-MZ slice executed an original standalone real-mode
fixture. The explicit DOS-stub path now extends that foundation as recorded
above; the startup-demo and audible MIDI acceptance requirements remain open.
