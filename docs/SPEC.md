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

## Native launch profiles

Native `--config FILE.toml` loads a version-1 launch profile. Built-in defaults
are overridden by profile settings, then explicit CLI options; absent CLI MIDI
modes must not overwrite profile modes. Profile paths resolve relative to the
TOML file; CLI paths retain shell-relative semantics. Validate unknown fields,
types, version, source files and audio requirements before loading the session.
The supported profile scope is game name/executable, printable ASCII arguments
(126-byte PSP command tail), absolute DOS working directory on C:, audio/MIDI,
and C: plus an optional immutable CD-ROM image mounted under D:–Z:.
Launch setup and Reset preserve the same resolved settings. TOML is a native
frontend dependency; runtime setup receives byte slices and no host paths.
ISO file open/read/seek/close and attributes are read-only; CD default-drive
selection, directory enumeration and browser profile selection remain pending.
Native C: profiles support ro, overlay (default) and explicit rw modes. RW
persists closed modified files with bounded streaming and atomic replacement
in the native adapter; the core retains its sparse RAM write budget. See
[launch-profiles.md](launch-profiles.md).

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
IN/OUT, XLAT with segment/address overrides, DAA/DAS and basic flag controls.
INS/OUTS support byte/word/dword widths, address/segment overrides and one
element per REP step. Wide word/dword IN/OUT forms preserve register
aliases and use overridable bus operations that default to adjacent byte ports.
MOVS, CMPS, STOS, LODS and SCAS support byte/word
width and bounded REP/REPE/REPNE, yielding after one element. General registers
have 32-bit backing, with byte/word aliases preserving untouched high bits.
In 16-bit code, operand-size override 66h supports selected dword moves, ALU,
shifts/rotates, stack operations, near calls/returns and bounded string operations.
SHLD/SHRD support word/dword register or memory destinations, immediate or CL
counts masked to five bits, operand/address/segment overrides and defined result
flags. A zero count preserves the destination and flags; OF is updated only for
a one-bit shift. AF and multi-bit OF are undefined and retained deterministically.
Word counts from 17 through 31 have undefined architectural results and stop
explicitly in the current interpreter.
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
that unit and reports it through CMOS equipment bits. The allocation-free public
decoder classifies all 2048 D8–DF/ModR/M pairs: every documented 387 form selects
an execution handler; later P6/SSE3 and reserved encodings remain explicit errors.
The [coverage map](compatibility/x87.md) records formats, directions and pops.
Opcode coverage does not promise complete numerical or exception conformance.

The FPU retains raw 80-bit register encodings alongside normalized finite
64-bit significands. Basic arithmetic supports the extended exponent range,
masked overflow/underflow and subnormal results; binary64 is only a diagnostic
view. PC selects 24/53/64 significand bits for basic arithmetic and FSQRT. RC
selects rounding for arithmetic and memory conversions. Reserved PC=01 remains
an explicit unsupported arithmetic configuration. Register FST/FLD/FXCH preserve
encodings and tags; real80 memory transfers preserve special payloads and
unsupported encodings without interpreting them as arithmetic.

Real32/64 transfers and basic arithmetic share zero/infinity/NaN rules. Memory
source denormals and signaling NaNs raise their respective status flags before
conversion. Ordered and unordered comparisons distinguish quiet/signaling NaNs;
FXAM classifies registers, including empty and unsupported encodings. FILD and
FIST/FISTP preserve signed integer precision and bounds. FBLD/FBSTP implement
18-digit packed BCD, negative zero, rounding and masked BCD indefinite. Invalid
BCD digits remain explicit errors because their architectural result is undefined.

FLDENV/FNSTENV and FRSTOR/FNSAVE use operand-size-selected 14/28-byte
environments or 94/108-byte states, independently of address size. They support
real/protected pointer layouts, opcode and all eight logical ST(i) encodings.
Control instructions do not replace the saved last arithmetic instruction/data
pointers. FNSTENV masks exceptions and FNSAVE initializes only after successful
memory writes; reads finish before state restoration. Multi-byte guest writes
are not promised atomic if the bus faults partway through them.

All seven constants retain 64 significand bits and honor RC independently of
PC without raising precision. FRNDINT, FSCALE, FXTRACT, FSQRT, FPREM/FPREM1,
FYL2X, FYL2XP1, F2XM1, FPATAN, FSIN/FCOS/FSINCOS and FPTAN have bounded portable
integer algorithms. Transcendentals ignore PC and honor RC. FPREM/FPREM1 set C2
for partial reduction and quotient bits upon completion. Trigonometric inputs
with magnitude at least 2^63 leave the operand/TOP unchanged and set C2.
F2XM1 requires finite inputs in [-1,1]. Some nonfinite transcendental/scaling
operands remain explicit unsupported numeric domains.

Masked exceptional results, sticky flags and selected pre/post-completion
rules are implemented. An unmasked register precision exception commits its
rounded destination and stack pop before returning the diagnostic error;
invalid/zero-divide faults preserve the destination. Pending IRQ13/FWAIT
signaling, wrapped register results for unmasked range traps, masked stack
fault completion and complete memory-store exception/restart behavior remain
pending. The frontend still stops on reported unmasked exceptions.

Captured native x86-64 x87 fixtures test arithmetic, constants, logarithms,
roots, remainders, trigonometry and format boundaries. Finite transcendental
samples permit small result differences; they establish neither a universal
error bound nor bit-identical historical-387/C1 behavior. Large-angle reduction
uses a 192-bit pi/2 constant rather than the physical x87 approximation.
The [implementation audit](compatibility/x87-audit.md) records exact counts,
remaining limits and reproducible native-reference commands. These checks do
not establish game compatibility.
Dword ENTER builds bounded nested stack frames and BSF/BSR support both operand
widths, including their defined zero flag.

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
`IN AL,DX; AND AL,08h; JZ/JNZ -5` pattern and the corresponding
`TEST AL,AH` form with AH=08h on the active VGA input-status port.
After discovery and a complete confirming iteration, whole taken iterations can
advance together strictly before a VGA retrace/PIT/DSP event, remaining budget
or 1-ms bound. AL, flags and instruction phase remain equivalent to ordinary
stepping. For TEST, the final complete iteration executes normally to preserve
unmasked AL bits that can change with display-enable status between retraces.
Changing AH disables that candidate. IF-disabled waits retain pending PIC state; pending eligible IRQs,
interrupt shadows, TF/RF/VM, paging, changed code/CS/DX and inactive ports fall
back. Ordinary one-step/debug execution cannot batch. An experimental native
Heroes flag enables caller-selected memory waits: exact CMP-dword/JNE loops at
three known sites, a positive ordinary-RAM counter and matching arithmetic flags.
Whole iterations stop before PIT/DSP deadlines, with a 1-ms bound; IRQ eligibility,
shadow/debug state, paging and changed code force ordinary execution. The counter
is changed by guest code, never by the optimizer. This feature is off by default.
Architectural exception delivery and
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
operations, short/near relative jumps and conditional jumps, LOOP/LOOPE/LOOPNE/JECXZ,
near CALL/RET, byte IN/OUT through DX, byte/dword MUL/IMUL/DIV/IDIV of the
accumulator, CWDE/CDQ, MOVZX/MOVSX into dword registers and byte/word/dword
MOVS/STOS/LODS with or without REP. A repeated string still performs one
iteration per step. Other word operand overrides, other instructions and buses
without the optional code contract use the original interpreter. A decoder
rejection is remembered against the same code check, so the original interpreter
runs without another decode attempt until those bytes change.
Both paths consume one scheduling step and preserve the existing IRQ boundaries.

PC RAM tracks checked write generations for 4-KiB pages, including mutations
through wide write views. A code span checks both page generations, its RAM
owner identity and A20 mapping generation; its physical pages are resolved at
decode time and hits do not translate the address again. Entries are keyed by
linear address and decoded only for default-32-bit code, so they remain valid
across CS-cache/context changes. Offset-independent CS checks are resolved
once per context, detected through a registers lineage and CS reload version;
hits still check the instruction's complete offset range.
Counter exhaustion disables caching rather than wrapping.
MMIO, noncontiguous A20 spans, code-address wrap and failed whole-instruction
segment validation retain byte fetching and its partial fault progress. Storage
is fixed/reserved during setup; execution does not allocate cache entries.
`Session::set_decode_cache(false)` and headless `--no-decode-cache` select the
reference path. The profiling feature exposes hits, misses, invalidations and
fallbacks. Differential tests cover dynamic operands, faults and invalidation;
the opt-in local PRINCE test compares bounded execution through 300 million
steps, including full final RAM/VRAM, scanout/palette, MIDI, PCM and guest time.

Native x86-64/AArch64 frontends enable a Cranelift dynamic core by default through
the `dynamic-core` feature. Its first scope is bounded blocks of 2–16 register-only
dword MOV/ALU/CMP/TEST, INC/DEC, XCHG and NEG/NOT in protected 32-bit code. A final
short/near relative Jcc/JMP back to the block's start can form a native loop;
the 16-instruction bound includes that branch. A separate RAM trace
supports dword register MOV/ALU memory sources, LEA, memory-left CMP/TEST,
byte MOV/CMP/TEST (including high-byte registers and MOV stores), PUSH/POP32,
near direct/indirect CALL, RET/RET imm16, dword memory XCHG and relative branches through at most
16 reachable instructions in a 240-byte span. Stack accesses require full flat
SS with a 32-bit stack; CALL/RET additionally require full flat executable CS.
Their targets can transfer directly to another instruction in the same prepared
graph; external targets return to dispatch with exact stack state. All used data segments
must be flat, fully readable 32-bit ranges at each entry. An optional bus view
borrows a RAM backing span with an optional excluded interval. The PC requires
A20 enabled, starts at 500h to exclude IVT/BDA accounting, and forbids the VGA
aperture even though backing bytes exist there. Every complete one- or four-byte
access checks both bounds and exclusion before touching memory; a failed check exits before that
instruction with the previous state and exact completed-step count. Byte/dword
MOV stores, PUSH and CALL stack writes can continue inside native execution. Store
segments must also be writable; successful validation is cached against used
segment selectors/descriptors and the execution key. Each store increments the
generations of touched 4-KiB pages. Writes to an active trace page or exhausted
generations exit before the store. PUSH ESP uses the old ESP; POP ESP finishes
with the popped value. Indirect CALL resolves its source before the stack write.
Memory XCHG resolves both operands from the old registers and completes all guards
before changing either; RAM exchange cannot interleave with hardware events in
this single-owner machine. Other stores, word-operand overrides, far calls/returns and device operations
remain interpreter boundaries.
Entry validates full CS bounds and
RAM code generations; changed generations require exact byte comparison before
reusing compiled code. Debug flags, paging, interrupt shadows and budgets that
cannot accommodate the whole register block retain ordinary stepping. The machine ends
execution slices strictly before hardware deadlines and delivers eligible IRQs first.
Each slice combines native blocks with validated decoded ordinary-RAM/near-branch
work for at most 256 steps. MMIO, I/O, faults, mode/IF changes and unsupported
instructions remain boundaries. Hardware clocks and execution counters advance
once per slice; RAM writes still invalidate code immediately. The bus must opt
into the ordinary-RAM batching contract, which defaults to disabled.

Without the native core, including WASM and `--no-dynamic-core`, the machine
uses the same slice boundaries for cached interpreter instructions. Data-segment
and CS windows are resolved once per slice, because batched instructions cannot
load segment registers. Each instruction validates every operand, stack slot and
branch target as plain RAM or a valid CS offset before its first effect, then
performs the accesses at those linear addresses; a failed check ends the slice
before that instruction and the original interpreter handles it. A divide error
leaves only IP changed, which the slice restores before handing over.

Discovery and native execution allocate nothing. Explicit host preparation between
batches may allocate and request executable memory through `doslet-x86-jit`, the
optional `std` compiler adapter. Its private unsafe invocation boundary accepts
borrowed general registers, EFLAGS and EIP, plus an optional bounded RAM slice
valid only during that call. Writable traces additionally borrow the physical
page-generation table exclusively. Read-only compiler calls reject writable traces;
read-only CPU calls may execute a register prefix but never stores. The adapter
never retains host RAM pointers or accesses devices. Straight-block flag liveness eliminates overwritten arithmetic flags
while preserving carry dependencies and complete flags at every native exit.
Native loops carry registers and full arithmetic flags between iterations in
generated code and store architectural state only when returning. They check
the remaining hardware-bounded slice budget before each whole iteration and
return the exact completed-step count. A remainder too small for another
iteration uses the existing path. Loop flag generation is conservative; it does
not yet eliminate overwritten flags across the backedge.
RAM/CFG traces preserve full flags at every instruction boundary and check the
remaining hardware-bounded budget per instruction, allowing exact partial
progress on internal branches and unavailable memory. The RAM borrow prevents
concurrent mapping changes or device writes during that native call. Generated
stores guard active code pages and update generations before continuing. Execution
statistics distinguish RAM-trace steps/entries from total native work.
Preparation accepts at most eight blocks per call. Executable storage holds at
most 4096 resident functions across 16 independently owned modules. Preparation
evicts the least recently used module when full, clearing all its handles before
freeing code; execution never compiles or evicts. Trace and register fallback
capacity is checked together. Cold imported templates are eagerly attempted once,
then evicted blocks are prepared again only when execution makes them eligible.
Compiler failures latch once and preserve interpreter fallback.
`--no-dynamic-core` disables JIT at runtime; `--no-default-features` omits it from
native application builds. WASM has no Cranelift dependency and stays interpreted.

Opt-in exit profiling, configured before native compilation, aggregates reasons,
entry/exit addresses, counts and native steps in a fixed 8,192-slot table allocated
at setup. Overflow is reported as dropped observations. Actual native returns,
including zero-step guard exits, are distinguished from refused entries. Normal
generated code contains no exit instrumentation. Profiling is diagnostic evidence
and is not used to report uninstrumented throughput.

An optional bounded execution profile saves observed code bytes and address hints.
On a later launch these bytes are decoded/compiled before execution and bound only
after matching actual RAM and current segment assumptions. Changed relocation
bytes fall back to discovery; identical register code may bind at another address.
Full static LE/LX object analysis and
relocation-aware object/offset identities are not implemented in this first core.
See [architecture.md](architecture.md#native-dynamic-core) for cache/profile bounds.

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
execution. It supports case-insensitive C: paths and a shared table of 96 DOS
handles with aliased open descriptions, attributes,
open/close/read/seek, current-directory change/query and stdout/stderr console
writes. The default drive remains read-only. An optional sparse RAM overlay
permits read/write opens and writes to existing guest files without changing
input buffers or host files. Standard launches reserve 1 MiB of changed-data
storage as 256 chunks of 4 KiB and a fixed staging buffer during setup. Merged
reads borrow base/chunk storage; exhaustion
returns DOS error 8. Zero-length writes truncate/extend, including zero-filled
gaps. Create/truncate reserves up to 64 new file records during overlay setup;
guest creation, reads and writes do not allocate. FCB find-first/find-next searches
ordinary current-directory 8.3 files, including created files, and publishes
directory-result fields in the DTA. Handle-based INT 21h/4Eh and 4Fh searches
support case-insensitive 8.3 masks, C: paths, current directories, ordinary
files and inferred subdirectories. Their 21-byte DTA prefix contains the search
context, so independent/copied DTAs continue independently of later directory
changes. Results include attributes, deterministic time/date, current overlay
length and a NUL-terminated name; errors set CF with DOS codes 2, 3 or 18.
Search execution allocates nothing. Volume labels and long-name aliases are
not provided. Persistence and modified-executable loading
are pending. Each file read is bounded by DOS CX (at most 65535 bytes).
Console output uses a fixed diagnostic tail.
The conventional-memory arena owns 1024 fixed block descriptors. PRINCE's
233 small DOS buffers exceeded the former 64-descriptor limit despite remaining
free memory; the larger bounded table preserves allocation-free DOS calls.
No host filesystem call or per-step allocation occurs in the DOS service path.
ASCII FCB parsing is bounded to 255 input bytes and supports 8.3 wildcards.
Only C: is available; AH=0Eh supports selecting it. Directory names are inferred
from mounted file paths, with a fixed 63-byte current-directory limit. Relative
file/attribute/overlay paths use that state; absolute paths start at the root.
Default-drive, break-check flag, InDOS/critical-error flag pointers, DTA, vector
get/set, DOS-version, US country information with an
ASCII far-call uppercase callback, deterministic date/time, handle timestamp
queries and device-info services implement selected observed startup calls.
AH=36h reports sectors/cluster, available clusters, bytes/sector and total
clusters for C:, its default-drive alias and a mounted CD. Invalid drives return
AX=FFFFh without repurposing carry. C: free space is the remaining bounded RAM
overlay budget, including native RW launches; the service never queries host
storage. Read-only C: and CD report no write space. Sector counts are rounded
into representable power-of-two clusters; file/extent limits remain independent.
Handle-based directory enumeration, a full DOS ABI and non-ASCII name/code-page
handling remain pending.

BIOS extended-memory queries match installed RAM, and A20 enable/disable/query
services reflect the actual gate. Other unavailable INT 15h functions return
AH=86h/CF=1. DPMI, XMS and VCPI installation probes report absence through their
defined status registers; no external host is installed. Their explicit IVT
shims prevent startup from running uninitialized RAM through a null vector.
INT 2Fh/AX=1680h releases the current execution batch and returns AL=00h.
The session remains running; `Session::timeslice_yielded()` reports this stop
until the next `advance` call. Native and Worker schedulers return to their
event loop before executing another batch. This service does not sleep,
advance hardware time beyond executed steps, or wait for an interrupt. Guest
INT 2Fh vector replacements retain ownership of their handler.
The runtime accepts one immutable ISO 9660 image through `Session::with_cdrom_image`
or one bounded positional provider through `Session::with_cdrom_source`,
and implements MSCDEX INT 2Fh services 1500h, 1502h–1508h, 150Ah–150Eh
for drive discovery, primary-volume metadata and sector reads. Invalid image
layouts are rejected during setup for resident images and during the first
descriptor read for positional sources; out-of-range reads return DOS/MSCDEX errors.
Without a configured image, the installation probe reports BX=CX=0. The native
launch-profile loader configures an ISO image under D:–Z:. Absolute file paths
on that drive support DOS open/read/seek/close, read-only attributes, handle
device information and timestamp queries. Writes return access denied. The
independent `doslet-iso9660` crate handles primary-volume directory records,
case-insensitive names and file-version suffixes. Positional sources share a
16-MiB cache of 128-KiB chunks across file and raw sector reads, with at most four
requests in flight. Browser local images retain an immutable Blob; HTTPS sources
require exact 206 responses and a pinned strong ETag. Three bounded attempts
use 10-second per-attempt and 35-second overall deadlines. Range/CORS failures
remain visible; there is no automatic full-image download.
Joliet/Rock Ridge names, multi-extent/interleaved files, CD default-drive/CWD,
FCB/FindFirst enumeration, MSCDEX device requests and audio commands are pending.
MSCDEX sector reads are limited to 32 sectors per call, bounding guest copies to
64 KiB; larger requests stop explicitly. ISO lookup is limited to 4096 directory
records per call and setup scans at most 64 volume descriptors.

BIOS/DOS HLE services are installed at explicit F000h shim addresses. A guest
INT must follow its current IVT vector, including vectors replaced by guest code.
Only arrival at an unchanged installed IRET shim invokes the host service.
An installed IRQ0 shim updates BDA ticks/daily rollover and decrements the BIOS
floppy-motor timeout (also used by CIV for delays), invokes the current
INT 1Ch user hook and sends PIC EOI through guest instructions while preserving
AX. Modified shims execute as guest code. INT 1Ah/AH=02h reads hours, minutes,
seconds and the daylight-saving flag from the installed CMOS registers, returning
CF set while its update-in-progress bit is set. The initial clock uses BCD and
24-hour format; RTC ticking and format conversion remain pending.
INT 10h/AH=0Fh
reports the default text or mode-13h configuration.
INT 10h/AX=0003h restores the selected 80x25 text configuration, initializes
mode/geometry/page/cursor fields in the BDA and clears eight B800h text pages
to spaces with attribute 07h. AX=0083h preserves those text bytes while resetting
the configuration. B800h currently uses ordinary PC RAM; presentation borrows
that RAM and expands CP437 glyphs directly into caller RGBA storage at 720x400,
with attributes, blink and cursor. Full VGA text-memory mapping remains pending.
INT 10h/AH=01h/05h sets cursor shape/active page; AH=06h/07h scrolls/clears a text
window; AH=08h/09h/0Ah reads/writes cells and AH=0Eh performs text teletype output.
INT 10h/AH=02h/03h
sets/queries the BDA cursor position for pages 0-7; setting the active text page's
cursor also updates the VGA CRTC cursor address. The query returns the BDA cursor
shape in CX. Invalid pages remain explicitly unsupported. Palette services
AX=1010h/1012h/1015h/1017h set/query single or bounded blocks of six-bit DAC colors.
INT 16h/AH=00h/01h/02h/05h/10h/11h uses a guest-visible 15-key BIOS ring.
INT 21h/AH=01h/06h/07h/08h/0Bh/0Ch consumes/queries/flushes the same input, with
DOS extended keys returning zero then scan code. Empty blocking reads yield in
guest STI/HLT retry loops, preserving timer IRQs and bounded host execution.
Native iced and browser Worker adapters support BIOS injection or physical AT
keys. AT mode uses scan sets 1/2, make/break sequences, guest-time typematic and
IRQ1. The installed IRQ1 shim updates the BIOS ring when the guest executes it;
a replaced guest handler receives the hardware interrupt instead. Keyboard and
joystick events pass through a bounded contributor-aware input router. The
optional 201h gameport exposes two axes and two buttons with guest-time charge
deadlines. Native gilrs and browser Gamepad adapters support device selection,
axis/button mapping and deadzone settings.
Implemented BIOS/termination services also include INT 10h/AX=0013h, INT 20h and
INT 21h/AH=4Ch, alongside the DOS services above and bounded AH=09h string
output plus AH=02h single-character output, including tab expansion. DOS carry
and keyboard zero results update the saved interrupt frame so IRET restores
them correctly. The console reserves 4096 bytes during setup and discards its
older half when full; `console_truncated()` reports lost diagnostic history.
Output continues to the guest text screen without reallocating.

DOS filesystem implementation must separate names/handles from the host mount.
The game directory should be a read-only input mount, with saves and other writes
in a separate overlay. No game files are bundled, modified or uploaded by the
bootstrap inspector.

## Initial device scope

| Device | Implemented foundation | Required follow-up |
| --- | --- | --- |
| i8259 | Master interrupt delivery plus independent slave register bank at A0h/A1h; fixed priority, masks, initialization and EOI | Cascading, full ICW/OCW behavior, auto-EOI and rotating/level modes |
| i8237 | Independent four-channel register/transfer state; two PC controllers, page registers, byte/word RAM transfers, counts, masks and auto-init | Full arbitration/cascade timing, memory-to-memory and MMIO transfers |
| i8254 | Binary modes 0/2/3, count latch, low/high accesses, gates, batched clocks and IRQ0 through the functional timeline; channel 2 gate/output at 61h | Modes 1/4/5, read-back, BCD and exact periodic load-edge timing |
| VGA | Linear A0000h aperture, mode-13h pixels, six-bit palette writes, miscellaneous/sequencer/graphics/CRTC configuration readback | Text/planar modes, register effects on rendering/memory, raster events and vendor SVGA extensions |
| VBE | 512 KiB VRAM, banked 640×480×8 mode 101h, borrowed scanout; BIOS 4F00/01/02/03/05 and far-call bank switch | More modes, LFB/PM interface, scanline/display-start controls and timing |
| MPU-401 | UART command/status/acknowledgements, 1024-byte output queue and allocation-free drain at 330h/331h | Synthesizer/host audio, serial timing, IRQs and intelligent mode |
| SB16 DSP / CT1745 | Control probes, rates/mixer state, selected 8/16-bit mono/stereo playback through DMA, auto-init/pause/resume, bounded stereo PCM queue and block IRQ acknowledgements | ADC/legacy/ADPCM commands, busy/reset timing, cascaded IRQ routing and applying mixer gain to audio |
| i8042 / AT keyboard | Controller commands/A20, bounded replies, scan sets 1/2, make/break, IRQ1, guest-time typematic, LED/query/ID/reset commands | Scan set 3, mouse/auxiliary protocol |
| ISA gameport | Two axes/two buttons at 201h; guest-time charge deadlines | Additional joysticks and gameport MIDI |
| MC146818 | CMOS register bank and installed-memory fields at 70h/71h; initial BCD date/time | RTC ticking, clock-format conversion, alarms, periodic IRQs and persistence |

Only the first sixteen bootstrap palette entries are EGA colors; the remaining
entries form a test color cube. This is not complete VGA BIOS palette emulation.
VGA configuration banks retain defined register fields, enforce CRTC protection
and color/mono port selection, and report absent SVGA extension registers as FFh.
Programmable register effects on raster/planar memory are not yet implemented.
INT 10h AX=1000h/1001h updates the attribute palette/overscan registers with
six-bit values. Selected extension BIOS probes (6F00h, 5F00h with BX=0 and
AH=12h/BX=0080h) retain registers rather than returning an installed-vendor
signature. The absent Tseng bank register at 3CDh floats high and ignores
writes; generic VBE bank selection still uses the supported BIOS interface.
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
Uninstalled WSS codec windows 530h–537h, 608h–60Bh, E84h–E87h and F44h–F47h
float high on reads and ignore writes so DOS setup probes detect no codec.
The uninstalled legacy Sound Blaster base windows 210h, 230h, 240h, 250h, 260h
and 280h behave the same; the emulated DSP remains at 220h.
PIC advanced configuration is not yet validated; use its documented subset.
PIT unsupported control modes return an error. The machine adapter's current
port error identifies the rejected port, not yet the precise chip command.
Mode 0 loads on the next clock edge, pauses under a low gate, raises OUT once
at terminal count and continues wrapping the 16-bit counter without more IRQ
edges. Its first low-byte write stops counting and clears OUT until the high
byte completes the reload. All three channels receive guest PIT clock ticks;
only channel 0 requests IRQ0. Port 61h bit 0 controls channel 2's gate and bit 5
reads its output. Speaker audio generation remains pending.

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
the imported ZIP mount and temporary writes. Explicit persistent stores retain
the changed-data overlay: native versioned files use an OS writer lock and atomic
manifest publication; browser IndexedDB uses an exclusive Web Lock and a fenced
transaction. Close/commit/namespace changes wait for the durable acknowledgement.
Stores bind to the immutable base identity, so reopening requires the same game
data. Export/import exchanges a versioned overlay with a base-identity check.
Neither backend writes to the immutable base; legacy native `rw` retains its
explicit direct-writeback behavior. SQL storage is not implemented.
BIOS/DOS keyboard mapping and DOS mouse input are available. The selected
INT 33h driver supports reset, visibility, position, button histories, ranges,
graphic cursor masks, motion counters and event callbacks (000Ch/0014h).
Callbacks execute guest far-call code through a bounded interrupt wrapper,
preserving full general registers, segment registers and FLAGS. Raw PS/2
mouse-controller protocol and IRQ timing remain pending.
Native pointer capture uses a local iced runtime/winit patch: Locked with
Confined fallback, relative device motion and captured button events. A bounded
128-event stream releases capture on overflow/disconnection. Esc is consumed
to release capture; focus loss, pause, reset and termination release it too.
Capture generations reject delayed events from a previous session. Native and
browser profiles independently support `input.hide_cursor` over the running
game image; disabling capture preserves absolute movement and passes Esc to DOS.
Browser capture uses DOM Pointer Lock from a display click and sends relative movement
directly to the machine Worker. Raw INT 33h/000Bh motion counters continue at
guest cursor bounds; only logical cursor coordinates are clamped. Fractional
host movement is retained without allocating or retaining offscreen movement
debt. The selected relative adapter uses one logical unit per motion unit;
full historical mouse-driver acceleration and mickey-ratio scaling are pending.

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

Native macOS additionally supports `apple` output through the system
DLSMusicDevice General MIDI bank. The host-only adapter receives routed GM
messages and renders bounded blocks into the existing Sound Blaster/CPAL mixer.
Pause preserves synthesis state; Reset recreates the unit. Output-switch replay
restores channel settings and notes, without preserving envelopes/effect tails.
The Apple FFI stays outside portable emulation crates. Apple output is explicitly
unsupported on other platforms and in the browser. Synthetic tests verify system
bank PCM, guest timing and replay; they do not establish game playback quality.

Optional host MIDI bypasses internal synthesis: native midir ports and browser
Web MIDI outputs. Discovery/permission is explicit. The default stays Rust synth.
Running status and partial messages are framed before sending; native SysEx is
bounded to 1024 bytes. Browser GM ports do not request SysEx permission;
marking a port as MT-32 explicitly requests it. Game dialect and output dialect
are separate choices. GM remains the default; Roland MT-32 SysEx upgrades the
game mode automatically and is reported in the inspector. MT-32 to GM outputs
use the ROM-free approximation; raw MT-32 outputs preserve SysEx. Native moont
supports GM through GmDevice, with CM-32L instruments and eight melodic parts.
GM to external MT-32 ports remains unsupported. Output transitions replay RPN 0,
channel state, held/sustained notes, and for raw MT-32, bounded DT1 packets only
for guest-written state. The router owns the partial wire message across output
transitions; exact voice envelopes/effects are not restored. Native moont
restoration services its event queue between complete messages, discarding
setup PCM without advancing guest/output time. Synth/dialect switches reuse
the native audio stream rather than reopening it. Browser ports with
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

The ROM-free MT-32-to-GM translator library accepts Roland DT1 SysEx (device
10h and channel-addressed 00h–0Fh, checksum-validated), the all-parameters
reset, part channel assignment, patch/timbre/rhythm memory, master volume,
reverb mode/time/level and display text. Notes follow MT-32 part routing, key
shift and reversed panning; bender range, including RPN 0, becomes GM RPN 0.
Channel volume follows the CM-32L part-level (about 48·log10) and linear
master-volume curves with 6 dB headroom. Per-preset level trims (−24..+6 dB),
octave corrections for four presets and the reverb send per mode and time were
measured against CM-32L v1.02 on the macOS GM bank; other GM synths share them.
Rhythm keys 35–75 use the GM kit; CM-32L sound-effect keys, aftertouch, partial
reserve, reverb character, RQ1 requests and per-key rhythm level/pan are not
translated. GM choices are Doslet's own; an opt-in local CM-32L ROM test
compares preset order, power-on programs and rhythm layout with its tables, and
`mt32_calibration` in `doslet-apple-midi` reports the calibration measurements,
without copying ROM data. Native and browser applications select translation by game and output dialect.

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
