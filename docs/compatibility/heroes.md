# HEROES.EXE startup evidence

Local input: `games/HoMnM/HEROES.EXE`. Its files remain unchanged.

The initial run stopped on `D9 C9` at `0180:0023772E` (FXCH ST(1)).
After register exchange was added, the next instruction was FDIV ST(0), ST(1)
at `0180:00237730`. This is a Pentium FDIV defect probe: the program computes
4195835 / 3145727, multiplies the quotient back, subtracts it from the original
numerator and compares the residual. The probe requires rounded arithmetic;
the previous exact-quotient-only implementation stopped explicitly.

The x87 unit now retains a 64-bit finite significand, applies guest precision
and rounding control, and implements the probe's FXCH, FDIV, FMULP, FSUBP and
memory FCOMP forms. A native x87 comparison agreed on significands, signs,
exponents, precision exceptions and C1 for 360 cases. The reference vectors
are retained as an unsafe-free regression fixture. A separate regression
checks the complete probe and its zero residual at extended precision.

The recorded run reached INT 2Fh/AX=1500h before CD-ROM support existed, so that
session reported zero installed drives. It exited normally with status 1 after
printing `Unable to access CD Drive.` at 25045764 steps. Runtime ISO mounting
and basic MSCDEX sector services have since been added, but this game has not
been rerun with the new support. Directory lookup and DOS file access through
the CD drive remain necessary; the recorded result does not establish menu or
gameplay compatibility.

Reproduction:

```sh
CARGO_TARGET_DIR=target/civ-probe cargo run -p doslet-headless --release --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 100000000
```

## CD-ROM startup with a mounted ISO

The test configuration mounts an ISO 9660 game-disc image. Enabling
MSCDEX discovery alone changed the failure to a missing-disc message: the game
then tried to open `D:\HEROES\SOUND\HEROES01.62S` through DOS AH=3Dh. The ISO
must therefore expose ordinary DOS file access, in addition to raw sectors.

The profile now mounts the installation as C: and that image as D:. The native
adapter excludes the ISO from the C: preload. `doslet-iso9660` retains one media
buffer and validates directory/file extents. CD handles read borrowed media;
DOS open/read/seek/close and read-only attributes/handle queries are implemented.
Synthetic guest and allocation tests verify reads, seek, EOF, close, write
denial, MSCDEX drive discovery and out-of-range sector errors. No game files or
image contents were modified or committed.

With the actual image attached, the game passes the CD check, performs the BIOS
display-combination query and reaches VBE mode 101h (640×480). At 36622817 steps
it stops on CPU instruction `0F AC` (SHRD) at `0180:00225DEE`. This establishes
CD startup progress, not menu or playable-session compatibility.

An ignored native application test loads the actual TOML profile without opening
a window/audio device and reaches Running/Vbe101 at 30000000 steps. Neither CD
error appears in the guest console. Run it with:

```sh
RUSTFLAGS='-C target-cpu=native' cargo test -p doslet --release --locked \
  local_heroes_profile_passes_the_cd_check -- --ignored --nocapture
```

```sh
DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog CARGO_TARGET_DIR=target/civ-probe \
  cargo run -p doslet-headless --release --locked --example inspect_civ -- \
  games/HoMnM/HEROES.EXE '' 100000000
```

Native profile launch:

```sh
target/release/doslet --config games/HoMnM.toml
```

## Publisher intro after double-shift support

SHRD and SHLD now support word/dword operands with immediate or CL counts.
CPU tests cover a bit-serial reference, defined flags, count masking/zero-count
behavior, register aliases, memory addressing/segment overrides and default
32-bit protected code with a 16-bit override.

Repeating the actual ISO-backed run reaches Running/Vbe101 at 100000000 steps,
with no fault. The captured 640×480 frame displays the New World Computing
publisher intro. This is startup/intro evidence, not gameplay acceptance or an
independent audio-device listening test.

## SETUP.EXE timer calibration

The sound setup program, rather than HEROES.EXE, reproduced the reported port
43h fault. At 67470115 steps it writes B0h at 0180:00181EF2, selecting binary
mode 0 on PIT channel 2. It loads FFFFh, enables the gate through port 61h and
executes a sequence of DIV instructions to measure elapsed counter ticks.

PIT mode 0 now counts guest clock edges, supports gate pause/resume, a single
terminal output transition, count wrapping and reloading. The PC clocks all
three channels and connects channel 2 gate/output to port 61h. Chip and machine
tests verify load timing, latched counts, terminal output and IRQ isolation.
The actual SETUP run passes the rejected timer command and calibration, then
stops at 67470961 steps on unsupported FILD m64int (`DF /5`) at
0180:0017E3E5. This does not establish successful completion of sound setup.

```sh
DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog CARGO_TARGET_DIR=target/civ-probe \
  cargo run -p doslet-headless --release --locked --example inspect_civ -- \
  games/HoMnM/SETUP.EXE '' 100000000
```

## Integer load and floating-point calibration

FILD m16/m32/m64int now retains every signed integer bit in the independent x87
significand, including values beyond binary64's exact-integer range. Tests cover
both integer limits, PC/RC combinations, stack overflow and subtracting adjacent
large integers. The CPU decodes the setup program's `DF 2C 24` stack-memory form.

The same calibration sequence then requires FDIVR/FMUL with memory sources,
rounded FSTP m32real and FADD m64real. These selected finite memory forms and
RC-controlled normal memory stores are implemented too. Tests check operand
order, tie rounding in every RC mode, destination precision independent of PC,
precision exceptions and retaining the source value until a store/pop.

The actual ISO-backed SETUP run passes that sequence and reaches 67470977 steps.
At that stage it stopped on FRNDINT (`D9 FC`) at 0180:0019695F. Subsequent work
below completes the observed setup startup. The guest files and configuration
remain unchanged.

## Completed SETUP startup and clean exit

FRNDINT and FIST/FISTP now support guest rounding, integer signed bounds,
precision/invalid exceptions and masked integer indefinite. Other observed
requirements were dword ENTER, BSF/BSR, absent Video7/Chips/Cirrus BIOS probes,
the absent Tseng bank-register probe at 3CDh and BIOS attribute-palette/overscan
writes. Focused tests cover these CPU, chip and BIOS behaviors.

The actual SETUP run reaches its text main menu by 90M steps. It identifies
`182 Mhz 386`, `Sound Blaster 16/AWE32/Compatible` and `VESA Compatible`.
The CPU MHz value comes from the functional guest clock model, not a host
hardware measurement. Pressing 1 opens `Select a New Sound Card`; Esc returns
to the main menu, and another Esc exits without saving with DOS status 0 at
130531090 steps. Host playback, every sound-card choice and a saved
configuration's behavior in HEROES remain separate acceptance work.

The native application regression uses the game launch profile plus the
SETUP executable override, disables host audio and checks both menus and clean
exit. The headless screenshots are local artifacts at
`/tmp/doslet-heroes-setup-menu.png` and `/tmp/doslet-heroes-setup-sound.png`.

```sh
RUSTFLAGS='-C target-cpu=native' cargo test -p doslet --release --locked \
  local_heroes_setup_profile_reaches_menus_and_exits_without_saving \
  -- --ignored --nocapture
```

```sh
target/release/doslet --config games/HoMnM.toml --game games/HoMnM/SETUP.EXE
```

## Register arithmetic after native audio startup

A gameplay report identified unsupported `D8 C1` at `0180:001A667F`, the register form
`FADD ST(0), ST(1)`. FADD, FMUL, FSUB and FSUBR with an ST(0) destination
now use the retained finite significands and the guest precision/rounding
control. CPU regression tests check result, operand order, unchanged source
and unchanged stack TOP. CPU/x87 tests, Clippy and WASM checks pass.

A new headless run with the actual CD image and an Esc key at 100M steps
remained Running in VBE 101h through 600M steps without a fault. It produced
1,490,436 SB PCM frames, of which 1,377,774 were nonzero. This scenario
does not establish all gameplay paths or complete x87 instruction support.

## Mouse motion and possible game patch

Disassembly of the loaded local executable identifies the polling procedure at
linear address 00213FABh. It requests INT 33h/000Bh through the game's interrupt
wrapper, sign-divides returned CX/DX motion by a sensitivity field at mouse
object offset 021Eh, adds it to its own coordinates at offsets 0330h/0332h and
clamps them to 0..639 and 0..479. The initialization code also hides the DOS
cursor and registers an INT 33h/000Ch callback at 00212FF0h for button events.
These addresses describe this locally loaded version, not every distribution.

A recorded real run sampled 4,333 INT 33h/000Bh calls and, after an injected
movement at 350M steps, observed the game's mouse object at 002832F0h holding
(500, 370), with sensitivity divisor 1. Guest files were unchanged. This is
motion/coordinate evidence; it does not establish every button/callback path.

A later GDB inspection of the live city captured 80 consecutive INT 33h calls,
all AX=000Bh. Stepping through the wrapper confirmed that the game reads the
returned CX/DX and takes its no-motion branch when both low words are zero.
The nonzero branch uses the accumulation described above. Static inspection of
initialization identifies AX=0000h (reset), AX=0002h (hide), and AX=000Ch (register
callback). The callback at 00212FF0h creates button events using coordinates from
the game's own position fields at 00283620h/00283622h; it does not use incoming
CX/DX as the event position. No AX=0003h absolute read appeared in this city sample.
This does not exclude queries on unobserved screens or during other transitions.
Local traces: `target/city-mouse-queries.log`, `target/city-mouse-use.log`.
Absolute host moves also update the emulator's relative counters, which explains
why uncaptured absolute tracking could previously appear to work in this game.

Changing the game's motion query to an absolute-position query would also
require replacing its accumulation/sensitivity logic and preserving its button
event machinery. No binary patch was applied. Fixing host capture and keeping
raw driver counters moving at logical bounds addresses the emulator problem
without a version-specific game patch. A separate reported `DE C1` instruction
is FADDP ST(1), ST(0); it now preserves retained precision and pops only after
successful arithmetic, covered by exact-integer and unmasked-exception tests.

## Main-menu CPU profile and retrace waits

The [wait and polling inventory](heroes-waits.md) catalogs VGA, DSP, memory-timer
and DOS-clock waits, separates audio/event work, and provides tools to record
additional paths. Its runtime evidence covers 50 million startup/menu steps.

An instrumented native run profiled steps 350M..430M in the main menu, after
Esc at 100M. This covers about 9.697 seconds of guest time. Session execution
took 3.071 seconds; RGBA conversion took 0.004 seconds for 58 changed frames.
There were no HLT steps. Randomized PC samples show event polling, extender
work and the retrace loop at 0022427Ah, not a single dominant halted routine.

The graphics driver uses `IN AL,DX; TEST AL,AH; JZ/JNZ -5` with AH=08h and
DX=03DAh. The previous recognizer accepted only the AND AL,08h form, so no
VGA waits were batched in this run. The TEST form now batches confirmed complete
iterations strictly before device deadlines. Its final iteration executes
normally to preserve raw AL display-enable bits. Differential machine tests
cover 16/32-bit code, both branches, DMA, IRQs, partial budgets and AH changes.

Repeating the same workload took 3.001 seconds of Session execution, about
2.3% less in this single instrumented pair. The profiled interval batched
2,352,084 logical wait steps. SHA-256 matched for the 15-MiB extended-memory
snapshot, final RGB frame and MIDI output; final CPU registers, execution
budget and PCM frame/nonzero counts also matched. This is a modest retrace-wait
improvement, not general detection of arbitrary game-menu busy loops or a
measurement of native UI/audio-thread CPU usage.

## Register store/pop after timer-wait activation

A gameplay report after timer-wait activation identified unsupported `DD D8` at
0180:001A3FB9. Disassembly confirms FSTP ST(0), immediately followed by FILD of
the next integer. Register FST/FSTP forms (DD D0..DF) now copy retained values
and tags without format rounding, and optionally pop. ST(0) as a destination
therefore discards one stack entry. Empty destinations are valid; empty sources
remain explicit stack errors under the current incomplete exception model.

CPU tests exercise all eight destinations with and without pop, including exact
integers beyond binary64 precision under single-precision PC, and verify no bus
writes or integer-flag changes. Unit tests cover TOP wrapping, zero/special tags,
destination overwrite and an empty source. Workspace tests, Clippy and the native
release build pass. The full gameplay path has not been replayed;
this establishes the reported instruction support rather than gameplay acceptance.

## Animated title residue: RAM versus SVGA

A visual report identified repeated strips of the purple title flag above the current
logo, followed by a partially erased/star-shaped logo. An ISO-backed headless
run without the Esc shortcut reproduces the upper strips at 250M steps. In this
recorded run the main menu is clean at about 300M steps; this does not establish
that every reported persistent variant has cleared.

At 250M, the game's 640x480 indexed image starts at physical RAM 0001A690h.
Its image descriptor at 0010F15Ch contains width/height at offsets 10h/12h and
that buffer pointer at 14h. All 307,200 bytes match the visible portion of
physical VRAM, including the title residue. This rules out UI scaling and
scanout conversion as the source in that sample. Equality alone does not rule
out an earlier video readback influencing RAM.

The game's RAM-to-video rectangle routine starts at 00212638h, using bank
selection through 0022B488h/0022B49Ch and the selected VBE adapter at 0022B8CCh.
The RAM-to-RAM rectangle copier starts at 00212778h. The next investigation
boundary is construction/restoration of the animated image and its rectangle
coordinates, rather than changing the advertised VBE pitch or bank granularity
without evidence. No specific faulty CPU instruction, missing VBE operation or
root cause has yet been established; no corrective graphics patch was applied.

The `inspect_civ` example's `DOSLET_PROBE_RAM=1` now writes a borrowed physical
VRAM dump to `/tmp/doslet-probe-vram.bin` in addition to low/extended RAM dumps.
Reproduce this sample with the mounted ISO, `DOSLET_PROBE_FRAME=1`, a 250M budget
and no scheduled key. Dumps remain local; no game assets are added to the repo.

## Intro frame loss: independent decoding and CPU timing experiment

The 2026-10-16 diagnostic reproduces the purple strips and eroded title with
the C: installation and D: ISO from `games/HoMnM.toml`, without input or host
audio playback. VRAM and the game's RAM image are identical at 250M steps.
Disabling the native core, decode cache, VGA wait acceleration and idle-loop
recognition produces identical RAM/VRAM dumps and checkpoint images through
300M steps. This rules out those optimizations for this recorded path.

The intro is the CD's `HEROES/DANIM/INTRO02U.SMK`. A separate Rust diagnostic
using crates.io `smk = 0.1.0` decodes it independently. Completed guest frames
77 through 109 match the complete reference sequence exactly. The game's own
Smacker frame counter then advances from 109 to 111, 113, 115 and 117 without
decoding the intervening frames. Replaying precisely that omitted-frame
sequence through the independent decoder reproduces all 66 captured guest
frames with zero differing pixels, including the corrupted title. The frame comparison records both comparisons.
This establishes lost delta-frame history rather than a bank/LFB or double
buffer presentation error in this sample.

Single-stepping 200M–270M steps found no x87 opcode sites in the flat protected
game code (CS=0180h). Pixel changes lead to the integer Smacker block decoder;
no RAM-to-RAM rectangle copy at 00212778h was observed in that window. These
observations concern this intro interval, not all earlier game calculations.

A controlled copy of the workspace under `target/heroes-clock-ab` changes only
the nominal CPU step cost from four 33-MHz clocks to one, including the matching
deadline conversion. Production timing and game files remain unchanged. In
that experiment the Smacker skipped-frame counter remains zero through movie
frame 153. RAM images sampled at guest seconds 25–29 match independent frames
117, 126, 134, 143 and 152 exactly. A sample taken during decompression at
second 24 is partial and is not counted as a completed-frame comparison.
Thus the current functional CPU/device time ratio can make the soundtrack-led
player drop required video deltas. The tested faster ratio avoids the observed
corruption; it is not a claim of cycle accuracy or a global speed change.

Reproduce the baseline and instruction-level observations:

```sh
cargo run -p doslet-headless --example audit_heroes_animation --release --locked -- \
  games/HoMnM/HEROES.EXE games/HoMnM/homm1.gog target/heroes-animation/default
cargo run -p doslet-headless --example audit_heroes_animation --release --locked -- \
  games/HoMnM/HEROES.EXE games/HoMnM/homm1.gog target/heroes-animation/reference reference
cargo run -p doslet-headless --example audit_heroes_animation --release --locked -- \
  games/HoMnM/HEROES.EXE games/HoMnM/homm1.gog target/heroes-animation/trace-frames trace
```

The observer uses addresses from this specific local build. It writes frames,
packets and RAM dumps only to the chosen diagnostic directory. Local independent
decoder sources, its lockfile and experiment artifacts remain in ignored
`target/smacker-reference` and `target/heroes-animation`. `smk` is not an
application dependency. Review of its production source found no build script,
unsafe code, network/process operations or writes; its only dependency is `log`.
This static review is not a security certification. The registry checksum for
the reviewed `smk 0.1.0` package is
`206125ea6519083d366404d5b1b7150d476e7c54db9171aa3aaf3f25ecec1349`.

### Configurable clock and native worker checks

The production `machine.cpu_mhz` setting reproduces the faster-clock experiment:
at 132 MHz, movie frame 153 reports zero skipped frames and its indexed image
matches independent frame 152 (FNV-1a `e1c26c1e2ca04dc3`) at step 956434018.
This check services positional ISO requests, prepares the default native core
and drains audio without a window or host audio device. A separate 66-MHz run
still reported one skipped frame; that setting is not verified as a clean intro.

```sh
cargo test -p doslet --release --locked \
  local_heroes_132mhz_intro_matches_independent_frame -- --ignored --nocapture
```

The native worker also has a 35-second pacing observer. It retains Rust sample
synthesis, uses the current local profile's CPU setting, and requests occasional
fresh snapshots without opening a window or audio device. The specific-build
Smacker counters are test-only observations. At 40 MHz an initial run observed
three skipped frames and sampled host lag of 0.2–10.5 ms. A subsequent 50-MHz run
with concurrent compilation and another game test reached 8.5 seconds of host
lag and two skipped movie frames. These are different host-load conditions,
not a controlled CPU-speed comparison or an audible-playback acceptance test.
Separating the worker protects UI responsiveness; it does not establish that
the host can sustain every configured MHz value.

```sh
cargo test -p doslet --release --locked \
  local_heroes_worker_pacing -- --ignored --nocapture
```

### Background JIT validation

The 40-guest-second check at 50 MHz with Rust GM synthesis passed with background
compilation and with JIT disabled. Final general registers, CS:EIP, EFLAGS,
ordinary-RAM hashes, visible indexed pixels and palette matched. The check used
the local game and ISO without a window or audio output device. It does not
establish audible playback or presentation smoothness.

Timing comparisons from this run are inconclusive: the host was concurrently
in use. Repeat `local_heroes_background_jit_pacing` on an otherwise idle host
before drawing performance conclusions. JIT remains enabled by default.
