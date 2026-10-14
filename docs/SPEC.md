# Doslet specification

Status: initial implementation, 2026-10-14.

## Tooling constraint

Node.js, npm, npx and tools or CI actions that execute through Node must not be
introduced. The build uses Cargo and the Rust implementation of Trunk, selected
as `cargo:trunk` in mise. `wasm-bindgen` generates browser JavaScript bindings;
these do not require a Node runtime.

## Purpose

Doslet must provide reusable Rust emulation components and an iced frontend that
can run natively and in a browser. Machiavelli the Prince, supplied locally in
`PRINCE/`, is the first game acceptance target. It is not a bootstrap completion
criterion: its LE executable and DOS/4GW environment require substantial CPU,
DOS and graphics implementation beyond the first slice.

## Required boundaries

1. `doslet-bus` defines CPU-visible memory and port operations without choosing a
   machine, device or frontend.
2. `doslet-x86` owns architectural state and executes through a generic bus.
   It must not depend on PC devices, DOS, windows, host files or host clocks.
3. Each chip library owns its registers, internal state and signal behavior.
   It must not access another chip or CPU through global state.
4. `doslet-pc` composes devices, routes address spaces and connects signals.
   A machine instance has one mutable owner.
5. DOS loading and services must access guest state and storage through explicit
   interfaces. Host filesystem policy belongs to platform adapters.
6. `doslet-runtime` owns a session and presents bounded control operations,
   borrowed scanout and diagnostics to every frontend.
7. iced and browser APIs must remain in application/presentation code.

The foundation and chip libraries use `no_std`. Fixed state needs no allocator;
buffer-owning libraries may use `alloc`. The current crate graph is documented
in [architecture.md](architecture.md).

## Memory and performance requirements

- CPU execution must not allocate per instruction or execution step.
- A batch must have a finite step budget. String repetition must yield between
  small bounded operations; a large CX must not monopolize the UI event loop.
- Ordinary RAM, VRAM and storage data must have explicit owners. Their public
  views should borrow slices or refer to address ranges.
- A borrowed RAM view must not bypass mapped device behavior. Ranges overlapping
  MMIO are rejected; crossing accesses use the bus operation for each part.
- Device state, guest registers, addresses and small events may be copied by
  value. Large guest buffers must not be cloned to implement ordinary execution.
- Video and audio conversion APIs should write into supplied output storage.
  UI/GPU copies and allocations must be identified separately from core execution.
- Scheduling should use typed events and device identifiers with capacity
  reserved outside execution. A full event queue must produce a defined result.
- CPU bus dispatch should be static in hot paths. Dynamic dispatch may be used
  at coarse host/plugin boundaries after measuring its cost.
- Runtime allocation tests and representative benchmarks must guard these
  properties. Measurements must state their workload, build profile and boundary.

The current regression tests cover the COM color-bar program and indexed-to-RGBA
conversion with preallocated output, plus relocated MZ execution and DOS console
output after setup, and mounted-file reads with MIDI queue output. They do not establish allocation behavior
for unimplemented instruction families or future devices. iced currently takes
an owned RGBA image per changed frame; shared GPU texture presentation remains
future work.

## CPU execution contract

The interpreter implements all 16-bit ModR/M effective-address forms with
DS/SS defaults and segment overrides. Its selected real-mode instruction scope
includes byte/word MOV and ALU groups, segment MOV, LEA/LES/LDS, TEST/XCHG,
INC/DEC, stack operations, accumulator moffs, shifts/rotates, CBW/CWD,
MUL/IMUL/DIV/IDIV, near/far calls and returns, Jcc/LOOP/JMP, INT/IRET, byte
IN/OUT and basic flag controls. Wide word/dword IN/OUT forms preserve register
aliases and use overridable bus operations that default to adjacent byte ports.
MOVS, CMPS, STOS, LODS and SCAS support byte/word
width and bounded REP/REPE/REPNE, yielding after one element. General registers
have 32-bit backing, with byte/word aliases preserving untouched high bits.
In 16-bit code, operand-size override 66h supports selected dword moves, ALU,
shifts/rotates, stack operations, near calls/returns and bounded string operations.
FS/GS moves/overrides, PUSHA(D)/POPA(D), 16-bit ENTER/LEAVE, selected bit-immediate
and MOVSX/MOVZX instructions, descriptor-table loads/stores, SMSW/LMSW and CR
register moves support the observed DOS4GW startup path.

The CPU models selected i386 real/protected-mode behavior. EIP and EFLAGS have
32-bit storage; POPF(D) cannot enable the 486 AC or Pentium ID flags. PUSHFD
omits RF/VM. CR0.PE transitions retain the previous hidden segment caches until
reload. GDT/LDT code/data descriptor reloads validate type, presence, privilege
and limits, including expand-down stacks, and set accessed bits. Code D selects
default operand/address width; 66h/67h select the other width. Both 16-bit
ModR/M and 32-bit ModR/M/SIB addressing are implemented. Stack addressing uses
SS.B independently of operand width. Near/far transfers and same-privilege
16/32-bit interrupt/trap gates and IRET use those caches and widths. LLDT/LTR,
LAR/LSL, SETcc, LSS/LFS/LGS and the dword F7 integer group support observed
startup code. This remains a selected subset, not a complete 386 implementation.
Paging, virtual-8086 execution, privilege-changing gates/returns, task switching
and precise exception delivery remain pending. Integer divide errors stop
explicitly rather than delivering INT 0.

`Cpu::default()` has no x87 and starts with CR0.EM/ET set. `with_x87()` installs
an independent fixed-stack `doslet-x87` unit and clears EM. The default PC uses
that unit and reports it through CMOS equipment bits. Supported x87 forms are
control/status initialization and transfer, FLD1/FLDZ, register duplication and
sign change, selected m32/m64 loads/stores, FCOMPP and FDIVP/FDIVRP for exact
finite results or masked zero/invalid results. Inexact division and m32 stores,
nonfinite memory loads, unmasked exceptions and other instruction forms stop
explicitly. Full 80-bit arithmetic, rounding, exception delivery and the remaining
instruction families are pending; the current values use binary64 storage.

CPU segment translation produces an unmasked linear address; machine A20 wiring
controls address bit 20. The default PC has 16 MiB of RAM, with A20 initially
disabled. Fast port 92h, i8042 output-port commands and BIOS A20 services update
the same signal. With A20 disabled, odd MiB regions alias the preceding even
MiB rather than every address being truncated to 20 bits. Borrowed RAM views
above 1 MiB require A20 enabled and still reject mapped device ranges.
INT pushes FLAGS, CS and return IP,
loads the guest IVT entry, and clears IF/TF. IRET restores that frame. The PC
delivers eligible PIC interrupts at instruction boundaries; STI and SS reloads
inhibit one following instruction. Interrupt-enabled HLT idles until delivery;
the machine can combine idle scheduling units up to the earliest PIT/DSP event,
remaining batch budget or 1-ms guest-time limit. Pending eligible IRQs and
IF-disabled HLT do not fast-forward. The PC also recognizes the exact unprefixed
`IN AL,DX; AND AL,08h; JZ/JNZ -5` pattern on the active VGA input-status port.
After discovery and a complete confirming iteration, whole taken iterations can
advance together strictly before a VGA retrace/PIT/DSP event, remaining budget
or 1-ms bound. AL, flags and instruction phase remain equivalent to ordinary
stepping. IF-disabled waits retain pending PIC state; pending eligible IRQs,
interrupt shadows, TF/RF/VM, paging, changed code/CS/DX and inactive ports fall
back. Ordinary one-step/debug execution cannot batch. Architectural exception delivery and
complete historical CPU-model behavior are not implemented. Unsupported opcodes,
operands, ports and services stop the session with a diagnostic; a faulted
instruction is not guaranteed to have rolled back its earlier side effects.

Execution steps are scheduling units, not physical clock cycles or retired
instruction counts. Each repeated string iteration counts as one step. Integer emulated
time and rational peripheral clock conversion exist independently. The PC now
advances a functional timeline by four nominal 33-MHz clocks per execution,
IRQ-entry or idle step. Fractional time is retained for PIT 1,193,182-Hz input,
selected VGA pixel clocks and DSP sample rates. This is deterministic across
host batches; it is not cycle-accurate CPU/bus arbitration or complete peripheral
timing. Timing-sensitive compatibility still needs real-run evidence.

`Cpu::step` has a bounded 512-entry single-instruction cache for selected
default-32-bit code. It caches prefix decisions, operations, immediates and
16/32-bit address expressions; effective addresses and segment permissions are
resolved against current registers. Implemented cached forms include byte/dword
MOV and ALU/CMP/TEST, dword INC/DEC, LEA, XCHG, NEG/NOT, selected dword stack
operations, short/near relative jumps and conditional jumps, near CALL/RET and
byte IN/OUT through DX. Word operand overrides, REP strings, other instructions
and buses without the optional code contract use the original interpreter.
Both paths consume one scheduling step and preserve the existing IRQ boundaries.

PC RAM tracks checked write generations for 4-KiB pages, including mutations
through wide write views. A code span checks both page generations, its RAM
owner identity and A20 mapping generation. CS-cache/context changes clear the
instruction cache. Offset-independent CS checks are resolved once per context;
hits still check the instruction's complete offset range. Counter exhaustion
disables caching rather than wrapping.
MMIO, noncontiguous A20 spans, code-address wrap and failed whole-instruction
segment validation retain byte fetching and its partial fault progress. Storage
is fixed/reserved during setup; execution does not allocate cache entries.
`Session::set_decode_cache(false)` and headless `--no-decode-cache` select the
reference path. The profiling feature exposes hits, misses, invalidations and
fallbacks. Differential tests cover dynamic operands, faults and invalidation;
the opt-in local PRINCE test compares bounded execution through 300 million
steps, including full final RAM/VRAM, scanout/palette, MIDI, PCM and guest time.

A decoded-block backend may share architectural state and instruction semantics
with the interpreter. It must invalidate cached code when relevant guest memory
changes. Native JIT backends are optional future components. The initial WASM
path uses the interpreter and does not generate host machine code.

## DOS and BIOS contract

The bootstrap COM loader creates a minimal PSP at segment 1000h, loads code at
offset 0100h, initializes CS/DS/ES/SS and reserves the stack return word at FFFEh.
It supports nonempty images up to FEFEh bytes. The standalone real-mode MZ loader
borrows parsed file data, validates the image, relocation targets, entry point
and interrupt stack space before writing, applies segment relocations and
initializes CS:IP and SS:SP from the header; DS/ES point to the PSP. The runtime
supplies ordinary RAM from segment 1000h up to A000h, excluding VGA and BIOS.

MZ allocation honors minimum/maximum extra paragraphs within that supplied
block, with maximum zero requesting all available space. With both values zero,
the load module goes at the high end. Extra bytes after the module are cleared.
Header planning supplies the initial process block to the DOS arena below. Entry must lie within
the load module; segment-base wrap and stacks without six bytes for INT are
rejected. SP=0 denotes the top of a 64-KiB stack when that stack fits. `load_mz`
rejects overlays and NE/PE/LE/LX execution. The explicit `load_mz_stub` path
permits execution of the MZ portion of LE/LX files; extended-image loading
remains unsupported. DOS EXEC/AL=03 loads an MZ overlay at caller-supplied load
and relocation segments without creating a PSP or changing entry registers.
Bus errors during loading may
leave partial writes; file/layout validation errors precede all guest writes.

Stub sessions supply an ASCII environment and program path at segment 0F00h.
DOS memory allocate/free/resize services use 64 fixed records, with guest-visible
MCBs, splitting/coalescing and explicit exhaustion. The list-of-lists subset
exposes only the first-MCB pointer; a complete DOS kernel data layout is pending.
The runtime still owns one process; EXEC/AL=00, child-process restoration and
handle inheritance remain unsupported.

The headless stub runner preloads its input directory into `ReadOnlyDrive` before
execution. It supports case-insensitive C: paths and 64 file handles, attributes,
open/close/read/seek, current-directory change/query and stdout/stderr console
writes. The default drive remains read-only. An optional sparse RAM overlay
permits read/write opens and writes to existing guest files without changing
input buffers or host files. The headless stub runner reserves 1 MiB and 64
write extents during setup. Merged reads borrow base/patch storage; exhaustion
returns DOS error 8. Zero-length writes truncate/extend, including zero-filled
gaps. New-file creation, persistence and modified-executable loading are pending.
Each file read is bounded
by DOS CX (at most 65535 bytes), and console writes by remaining fixed capacity.
No host filesystem call or per-step allocation occurs in the DOS service path.
ASCII FCB parsing is bounded to 255 input bytes and supports 8.3 wildcards.
Only C: is available; AH=0Eh supports selecting it. Directory names are inferred
from mounted file paths, with a fixed 63-byte current-directory limit. Relative
file/attribute/overlay paths use that state; absolute paths start at the root.
Default-drive, break-check flag, InDOS/critical-error flag pointers, DTA, vector
get/set, DOS-version, US country information with an
ASCII far-call uppercase callback and device-info services implement
selected observed startup calls. Directory enumeration, write overlays, a full
DOS ABI and non-ASCII name/code-page handling remain pending.

BIOS extended-memory queries match installed RAM, and A20 enable/disable/query
services reflect the actual gate. Other unavailable INT 15h functions return
AH=86h/CF=1. DPMI, XMS and VCPI installation probes report absence through their
defined status registers; no external host is installed. Their explicit IVT
shims prevent startup from running uninitialized RAM through a null vector.

BIOS/DOS HLE services are installed at explicit F000h shim addresses. A guest
INT must follow its current IVT vector, including vectors replaced by guest code.
Only arrival at an unchanged installed IRET shim invokes the host service.
An installed IRQ0 shim updates BDA ticks/daily rollover, invokes the current
INT 1Ch user hook and sends PIC EOI through guest instructions while preserving
AX. Modified shims execute as guest code. INT 10h/AH=0Fh
reports the default text or mode-13h configuration. Text rendering is pending.
Implemented BIOS/termination services also include INT 10h/AX=0013h, INT 20h and
INT 21h/AH=4Ch, alongside the DOS services above and bounded AH=09h string
output. DOS carry results update the saved interrupt frame so IRET restores
them correctly. The console reserves 4096 bytes during setup and
reports capacity exhaustion rather than reallocating during execution.

DOS filesystem implementation must separate names/handles from the host mount.
The game directory should be a read-only input mount, with saves and other writes
in a separate overlay. No game files are bundled, modified or uploaded by the
bootstrap inspector.

## Initial device scope

| Device | Implemented foundation | Required follow-up |
| --- | --- | --- |
| i8259 | Single edge-triggered PIC, fixed priority, interrupt mask and EOI | Cascading, full ICW/OCW behavior, auto-EOI and rotating/level modes |
| i8237 | Independent four-channel register/transfer state; two PC controllers, page registers, byte/word RAM transfers, counts, masks and auto-init | Full arbitration/cascade timing, memory-to-memory and MMIO transfers |
| i8254 | Binary modes 2/3, count latch, low/high accesses, batched clocks and IRQ0 through the functional timeline | Modes 0/1/4/5, gates, read-back, BCD and exact load-edge timing |
| VGA | Linear A0000h aperture, mode-13h pixels, six-bit palette writes, miscellaneous/sequencer/graphics/CRTC configuration readback | Text/planar modes, register effects on rendering/memory, raster events and vendor SVGA extensions |
| VBE | 512 KiB VRAM, banked 640×480×8 mode 101h, borrowed scanout; BIOS 4F00/01/02/03/05 and far-call bank switch | More modes, LFB/PM interface, scanline/display-start controls and timing |
| MPU-401 | UART command/status/acknowledgements, 1024-byte output queue and allocation-free drain at 330h/331h | Synthesizer/host audio, serial timing, IRQs and intelligent mode |
| SB16 DSP / CT1745 | Control probes, rates/mixer state, selected 8/16-bit mono/stereo playback through DMA, auto-init/pause/resume, bounded stereo PCM queue and block IRQ acknowledgements | ADC/legacy/ADPCM commands, busy/reset timing, cascaded IRQ routing and applying mixer gain to audio |
| i8042 | Command/status/data registers, bounded controller replies, command byte, interface/self-test, output-port/A20 commands | Guest keystrokes, keyboard protocol, mouse/auxiliary channel, timing and IRQs |
| MC146818 | CMOS register bank and installed-memory fields at 70h/71h; initial BCD date/time | RTC ticking, clock-format conversion, alarms, periodic IRQs and persistence |

Only the first sixteen bootstrap palette entries are EGA colors; the remaining
entries form a test color cube. This is not complete VGA BIOS palette emulation.
VGA configuration banks retain defined register fields, enforce CRTC protection
and color/mono port selection, and report absent SVGA extension registers as FFh.
Programmable register effects on raster/planar memory are not yet implemented.
Selected text/mode-13h/VBE timings drive input-status blanking/retrace; the guest
can poll 3BAh/3DAh without host-refresh-dependent or read-toggle timing.
The VBE adapter advertises version 1.2 and mode 101h only, with one 64-KiB
read/write window at A000h and 64-KiB bank granularity. Optional functions and
unavailable modes return AX=014Fh. A mode switch clears the preallocated VRAM
unless preservation is requested. No LFB is advertised. This is a selected
generic banked card, without vendor SVGA acceleration/register extensions.
The PC has no AWE/EMU8000; reads in its declared probe banks 600h–6FFh,
A00h–AFFh and E00h–EFFh float high and writes have no receiver. Other unsupported
ports still stop explicitly. SB16 reports DSP 4.05 and mixer IRQ7/DMA1/DMA5
configuration. Functional DMA/PCM/IRQ execution is covered by synthetic and real
startup runs; host playback and MIDI synthesis remain pending. A 4096-frame
preallocated PCM queue stops explicitly when the host fails to drain it.
PIC advanced configuration is not yet validated; use its documented subset.
PIT unsupported control modes return an error. The machine adapter's current
port error identifies the rejected port, not yet the precise chip command.

Keyboard/mouse, OPL and storage controllers must be
separate chip/card libraries as they are implemented. Sound Blaster card
composition should reuse OPL and connect DMA/IRQ signals through the machine,
rather than embedding dependencies on CPU or host audio.

## Frontend contract

The desktop and web application share iced view construction. Native state owns
the Session; browser window state contains only snapshots and presentation data.
The native frontend runs at most 400000 execution steps per UI timer callback, in
10000-step chunks. The browser Worker owns its Session and runs at most 400000
steps and about 8 ms of host work per turn, yielding between turns. A host
monotonic clock paces native and silent browser execution; active worklet playback
paces the browser Worker with bounded lookahead. The machine's own
deterministic clock drives guest devices. Audio queue capacity can defer a
batch. Pause/resume resets the host pacing anchor. It does not require browser
shared memory.
The native CLI uses clap derive: `--game EXE` starts a session with the containing
directory preloaded as C: and a 1-MiB RAM write overlay; no arguments retain the
demo. Invalid arguments and game-load errors exit before opening a window.
`--no-audio` disables host audio. The browser imports a local ZIP through a file
picker and lets the user select an EXE/COM. The program's containing directory
is mounted as C:, preserving subdirectories; stored/Deflate entries are expanded
only during session setup. Original compressed data remains owned by the
Worker for Reset; execution owns preloaded files plus a separate RAM overlay.
The picker transfers bytes locally without uploading them or bundling game data.
Import limits are 64 MiB compressed, 4096 entries, 64 MiB per file and 256 MiB
expanded. Invalid/duplicate DOS paths, symbolic links, encrypted files and
unsupported compression are rejected explicitly. Reset targets the running
program, independently of subsequent import/selection changes. Page reload loses
the mount and RAM writes. Guest keyboard/mouse mapping is pending.

Browser presentation uses iced's tiny-skia renderer with Canvas2D. A small
separate Rust/WASM AudioWorklet module pulls samples from a fixed 16384-byte
MIDI event queue on the guest timeline. The Worker sends bounded wire packets
and time watermarks directly through a MessageChannel; window timers do not
produce samples or drive machine execution. Eighty milliseconds of guest
lookahead and an initial 40-ms watermark prefill absorb ordinary jitter.
Unknown future MIDI prevents guest audio time from advancing: underruns output
silence and preserve state. Actual callback lengths are honored with fixed
2048-frame scratch capacity; Rust render/receive/reset allocate nothing after
setup. WASM compilation happens before worklet creation, outside rendering.
Start game resumes Web Audio in response to a user gesture; Enable audio retries
a suspended context, with Mute/Unmute available once running. A suspended context
does not block guest execution. Epochs invalidate stale audio on game/output
changes. Pause preserves queued synth state; Reset clears it. The user confirmed
Rust synth sound and graphics in Firefox and Chrome.

Optional host MIDI bypasses internal synthesis: native midir ports and browser
Web MIDI outputs. Discovery/permission is explicit. The default stays Rust synth.
Running status and partial messages are framed before sending; native SysEx is
bounded to 1024 bytes and browser SysEx permission is not requested. Basic channel
state, held/sustained notes and the partial wire prefix are replayed on output
transitions; exact voice envelopes/effects are not restored. Browser ports with
clear() use timestamped output, while ports lacking it use a bounded cancelable
window timer queue. Native output has a dedicated thread that owns the midir
connection and sends from a preallocated 8192-message queue at guest-derived
host deadlines. Execution runs up to 80 ms ahead; timestamps have bounded
chunk resolution, not sample accuracy. Only MIDI events and scheduling state
are shared with that thread. Short messages allocate nothing after setup;
native SysEx payloads use bounded host allocations. Pause/reset cancel future
messages and request silence before subsequent replay. Dispatch lateness is
visible independently of the host synth's own buffering. Failures pause the
machine and report once; selecting a working output permits Run without
resetting guest RAM. Sound Blaster PCM mixing and internal GM/MT-32 fidelity
remain pending.

The `web` Cargo profile uses LLVM opt-level 3, thin LTO and one codegen unit.
Release Trunk builds additionally run wasm-opt -O3. These are speed-oriented
build settings; no measured browser performance improvement is claimed.

The native runner still executes on the UI thread in bounded batches; a native
CPU thread remains future work. Browser Workers use the same Session boundary.
The emulator must not depend on the UI refresh frequency for guest timing.

## Milestones and acceptance

| Stage | Completion criteria | Current status |
| --- | --- | --- |
| B0: bootstrap | Component tests, allocation guard, headless COM demo, native and WASM compilation, docs and game inspector | Initial implementation |
| B1: real-mode environment | Broader CPU coverage, precise memory/interrupt behavior, DOS handles/files/memory, MZ relocation loader, synthetic integration programs | MZ relocation slice implemented; remaining work pending |
| P0: PRINCE startup | 386 state/instructions, protected-mode transitions and LE/extender startup; trace the actual DOS/4GW path | Reaches game initialization/device probes; startup remains incomplete |
| P1: first game screen | Implement observed SVGA/VESA, files, timers and mouse paths; capture real startup/title/menu frames | HDI logo, story/map and main menu captured; interactive input pending |
| P2: playable session | New game, map navigation, one turn, save/reload and pause/resume with bounded memory | Pending |
| P3: audio and web parity | Observed Sound Blaster/MIDI behavior, browser game import/storage/audio, same acceptance actions on native and web | Pending |

Synthetic fixtures must remain redistributable and independent of commercial
assets. Local game checks are opt-in and must report the exact executable used.
Successful builds, synthetic tests and header inspection do not constitute a
successful PRINCE run. [The game report](compatibility/prince.md) owns that evidence.

The active acceptance objective is the actual MPRINCE startup demo with audible
MIDI. Loading DOS4GW, reaching a later instruction, queued synthetic MIDI bytes
and compilation do not satisfy that objective. The local DOS startup now loads
its actual DOS4GW overlay, prints the extender banner and executes flat 32-bit
initialization from the game image. Actual HDI logo, introductory letter/map and
main-menu frames have been captured. Sparse RAM writes permit the game's MIDI
files to open in read/write mode; the real MPU stream now contains note events.
The native frontend now loads game sessions through `--game`; a live user
screenshot confirms the actual introduction and nonzero MIDI output consumed by
the host callback. Independent native listening, instrument fidelity and interactive
gameplay remain unverified. Browser ZIP import is implemented; real browser
execution was observed by the user in Firefox, and browser MIDI is confirmed
audible. Chrome rendering after the Canvas2D change still needs confirmation.

## Sources

- [iced 0.14 tour](https://github.com/iced-rs/iced/tree/0.14.0/examples/tour):
  native/web setup, Trunk input and WebGL features.
- [iced feature definitions](https://docs.rs/crate/iced/0.14.0/features).
- [allocation-counter](https://docs.rs/allocation-counter/0.8.1/allocation_counter/):
  host-only test instrumentation; not linked into production emulation.
