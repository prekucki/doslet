# PRINCE graphics drivers and VGA waits

The local game executes `VS640480.386` through a small private graphics ABI.
Replacing verified driver routines with Rust implementations is feasible in
principle, but the measured startup workload spends only 1.922% of its logical
execution steps inside this module. A separate vertical-retrace polling loop
accounts for about 22.3% of those steps and is the stronger initial optimization
candidate. VGA wait acceleration is implemented at PC composition; driver
replacement remains future work.

Evidence was collected on 2026-10-14 from the ignored, read-only installation.
This report contains metadata and observations, not original game binaries.
The run covers startup through the introductory letter, without guest input.
Step fractions are not measured fractions of host CPU time or promised speedups.

## Reproduce the observation

The diagnostic example uses the existing loader and unmodified session execution.
It observes DOS file requests, code-segment changes, driver entry/return sites,
VBE calls and VGA polling. MIDI and PCM are drained into bounded buffers without
playing audio. Host observation and printing happen outside `Session::advance`.

```sh
cargo run -p doslet-headless --example inspect_svga --release --locked --offline -- PRINCE 0
cargo run -p doslet-headless --example inspect_svga --release --locked --offline -- PRINCE 300000000
cargo run -p doslet-headless --example inspect_svga --release --locked --offline -- PRINCE 300000000 baseline
cargo run -p doslet-headless --example inspect_svga --release --locked --offline -- PRINCE 300000000 accelerated
```

`0` inventories the files without execution. Observation advances one logical
step at a time; `baseline` advances bounded 10,000-step batches without tracing
and disables VGA wait recognition. `accelerated` uses the same batched observer
with wait acceleration enabled, allowing a comparison of the final snapshot,
frame/time and MIDI/PCM hashes.
The limit is at most one billion steps. Mapping and poll-site tables have fixed
limits. The observer identifies an original module on first execution using its
entry table and complete body, allowing changes to header state. It keeps that
mapping afterward; it does not establish safety for arbitrary hot replacement,
self-modification or another game. It is a diagnostic, not a host benchmark.

Local output is under ignored `target/svga-analysis/`. The final observed and
baseline runs agree on the reported register/stack snapshot, guest time,
frame revision, indexed/RGBA hashes, MIDI bytes/hash and PCM frames/hash:

| Result | Both runs |
| --- | --- |
| Status / steps | Running / 300,000,000 |
| Guest time | 36,363,636,363 ns |
| MIDI bytes / FNV-1a 64 hash | 2,228 / `93B7CA54323C6EF5` |
| Drained stereo PCM frames / hash | 776,942 / `B6C2D02DD819CB85` |
| Indexed pixels / RGBA hash | `117EDCBFB7C99027` / `10273A19A740150D` |
| Frame / revision | 640x480 / 1,855,767 |

Agreement validates the observation against this reference run, not an HLE
implementation, physical hardware fidelity, audible PCM or interactive gameplay.
The accelerated 10,000-step run also matches every reported field in this table
and the reported final register/stack snapshot after 300 million steps.

## Shared module layout

The installation has ten modules: AT, C0, CX, HD, S3, T4, TR, TS, VS and WD,
each with suffix `640480.386`. Sizes range from 3,530 to 4,442 bytes.
Their private header is 84 bytes; `.386` is not an MZ executable here.

| File offset | Observed meaning |
| --- | --- |
| `00`, `04` | Initial magic words `12345678`, `87654321` |
| `0A` | Cached bank byte, initially FF |
| `0C` | Protected video selector; initially A000, patched to 01C0 in the VS run |
| `0E` | Initial real-mode video segment A000 |
| `10` | Screen-address offset, zero in the recorded run |
| `14` | Scanline stride, 640 bytes |
| `18` | Writable module-data selector, patched to 01B8 in the VS run |
| `1C..48` | Twelve relative 32-bit entry offsets; `28` is zero |

Nine drawing functions have identical bodies across all ten files after masking
the signed displacement of decoded five-byte `CALL rel32` instructions. Every
masked call targets the module's bank switcher: `54` for card-specific modules,
`122` for VS. Other instruction bytes, branches and function sizes match.
This was checked with GNU objdump starting at each entry, not by treating the
entire header/data area as instructions. Linear disassembly through data can
produce false IN/OUT instructions.

| Header slot | VS entry | Bytes to next entry | Meaning / remaining limit |
| --- | --- | --- | --- |
| `1C` | `02B2` | — | Mode setup and VBE capability checks |
| `20` | `0434` | 916 | Single-color line rasterization; stack arguments |
| `24` | `1074` | 144 | Filled rectangle; register arguments |
| `28` | zero | — | Absent entry |
| `2C` | `0BDC` | 250 | RAM-to-video rectangle copy |
| `30` | `0CD6` | 250 | Video-to-RAM rectangle copy; not exercised in this run |
| `34` | `0DD0` | 192 | Another RAM-to-video copy path; complete contract pending |
| `38` | `0E90` | 484 | Bitmap decoding/drawing path; complete contract pending |
| `3C` | `1104` | 40 | Write one pixel |
| `40` | `07C8` | 1,044 | Line rasterization from a byte/color sequence |
| `44` | `112C` | 46 | Read one pixel |
| `48` | `042E` | — | Public bank-switch wrapper |

Byte lengths include alignment/data before the next entry. These labels describe
disassembled behavior; they do not identify the graphics SDK or its author.

Card-specific bank mechanisms differ. S3 uses CRTC ports 3D4/3D5, unlocks
register 38 with 48 and updates the low nibble of register 35. AT uses indexed
ports 1CE/1CF and register B2. T4 and TS use port 3CD with different bit packing.
TR uses sequencer ports 3C4/3C5, register 0E and XOR 02. C0 uses graphics-controller
register 0E, CX and WD use register 09, and HD combines miscellaneous-output and
extended sequencer bits. Prefixes alone do not establish every chipset model.
Some module initializers use vendor BIOS modes; S3 also requests VBE mode 101h.
No acceleration commands appear in the shared drawing routines: CPU instructions
write pixels through a banked video-memory window.

VS checks VBE mode 101h using 4F01, requires a 64-KiB window, supports window
granularities of 4/8/16/32/64 KiB, sets mode with 4F02 and switches banks with
4F05. Its BIOS wrapper uses DPMI INT 31h/0300 to simulate real-mode INT 10h.
See [DJGPP VESA documentation](https://www.delorie.com/djgpp/doc/ug/graphics/vesa.html.en)
and [DPMI 0300 documentation](https://www.delorie.com/djgpp/doc/dpmi/api/310300.html).

## Actual loading and call boundary

The game opens `C:\VS640480.386` at step 9,179,046, reads 4,442 bytes into
linear address `00012480`, and executes the original module at base `00234038`
at step 9,191,063. Its code selector is `01B0`, with 32-bit default code.
The first entry is initialization at offset `02B2`. Protected far calls use
an eight-byte return frame containing EIP and a selector slot.

| Observed entry | Entries / 300M steps | Calling convention evidence |
| --- | --- | --- |
| Initialization | 1 | Far return, EAX=0 on observed success |
| Filled rectangle | 5,038 | EAX=y1, EDI=x1, ECX=x2, ESI=y2, BL=color; inclusive endpoints; far return without argument cleanup |
| RAM-to-video copy | 50,226 | Stack: source, x, y, width, height, source skip after each row; `RETF 24` |
| Sequence-colored line | 8,723 | Seven dword stack arguments; `RETF 28`; complete input/return contract pending |

The first two full-screen fills consume 162,006 and 163,324 logical steps,
including device/interrupt work. Rectangle fill clobbers several registers;
the copy routine saves/restores its general registers but changes arithmetic
flags and can decrement a height argument in stack memory. A replacement cannot
assume that every function preserves registers or that popped arguments leave
their backing memory unchanged. Segment state, bank/cache state, flags and
direction-sensitive accesses are part of the behavior to compare.

There are 5,766,365 observed steps inside the loaded module (1.922%). Kernel
code servicing its BIOS/DPMI calls lies outside this count. No other module was
observed executing. The run records 249 host VBE bank-service arrivals; those
are BIOS-shim arrivals, not necessarily every internal bank-switch request.

## Confirmed vertical-retrace waits

The two polling loops are in the game's flat code, outside the driver:

```asm
; DX=03DAh. First finish the current retrace, then wait for the next one.
001DE7B4: in   al, dx       ; EC
001DE7B5: and  al, 08h      ; 24 08
001DE7B7: jnz  001DE7B4    ; 75 FB
001DE7B9: in   al, dx       ; EC
001DE7BA: and  al, 08h      ; 24 08
001DE7BC: jz   001DE7B9    ; 74 FB
```

The first loop performs 511 reads, the second 22,286,579. Completed repeated
iterations contain three instructions, making their combined contribution
approximately 66.9 million steps, or 22.3% of this run. The finite run can stop
mid-iteration, so this is an approximate fraction. Neither loop is a HLT wait.

## Implemented VGA wait acceleration

The PC recognizes these two five-byte sequences structurally, independent of an
absolute PRINCE address. PcBus emits a small notification generation only on an
actual active VGA status read. PC compares it before/after the completed CPU
instruction, then inspects the candidate at the actual IN site. Unrelated CPU
transfers and earlier host-side bus reads do not trigger discovery. A complete
normally executed taken IN/AND/Jcc iteration must finish before activation, at
the next IN boundary, with DX selecting the active VGA input-status port.
Initially support only `IN AL,DX; AND AL,08h; JZ/JNZ -5`. Do not infer that an
arbitrary repeated port read is idle or skip loops with counters, memory writes,
other I/O, unsupported conditions or debugging/single-step requirements.

The VGA chip exposes the guest-time deadline of the next bit-3 transition,
derived from its current raster phase, pixel clock and fractional pixel state.
The PC machine combines this deadline with PIT/DSP deadlines, pending eligible
interrupts, remaining execution budget and a bounded slice such as 1 ms. Masked
interrupts retain their pending state. IF-disabled waits advance the same
hardware clocks and retain pending PIC requests; eligible IRQs take precedence.

It skips only whole three-instruction iterations ending strictly before the first
relevant event. It preserves loop phase and the register/flag result of ordinary
iterations. If a deadline falls inside an iteration, execute the tail normally;
IRQ handlers must see the same interrupted architectural state. Continue to
advance PIT, VGA, DMA/PCM and guest time while the CPU performs no useful work.
This batching lives in PC/runtime composition; the reusable CPU does not depend
on VGA. Each candidate instruction/batch rechecks ordinary-RAM code identity,
CS cache, CR0, DX and active status port. A batch also requires the previously
produced AL and arithmetic flags to match the current taken-loop sample. TF,
RF/VM, interrupt shadows, paging and unusual code wrapping/MMIO/A20 aliases
fall back. High EAX bits are preserved. Reading VGA status has no read side
effects in this model; a future attribute-controller flip-flop requires revisiting
equivalence. The runtime/PcMachine enable switch and headless `--no-vga-waits`
disable recognition as well as batching for reference measurements.

There are two distinct scheduling operations. Fast-forwarding guest hardware
avoids interpreted polling iterations even in a headless run. Sleeping the host
is appropriate only after the guest reaches its wall/audio pacing target: the
native execution thread can wait until a scheduled deadline or input/control
wakeup, and
the browser Worker can yield and schedule its next turn. Neither operation
should block the iced/window thread, change the guest's CPU HLT state or use
host display refresh as the VGA clock. A fixed host sleep of 16 ms is insufficient.

Differential tests cover both loop
polarities, partial budgets, both code widths, raster wrap/mode changes, IRQs
at every instruction boundary, concurrent DMA/PCM, code changes and allocation
behavior. Fractional raster tests check the status bit one nanosecond before and
at the deadline in all supported modes and both status-port selections. A
one-million-step runtime regression allocates nothing during execution. Profiling
counts `vga_wait_steps` and `vga_wait_batches`; CPU counters retain logical units,
while bus-read counters reflect actual dispatched accesses. Real PRINCE output
and native/WASM performance comparisons are recorded separately below.

### Recorded native comparison

Six alternating 300-million-step runs compared the final implementation with a
saved pre-change release binary. Both have profiling enabled. The measured
boundary is `Session::advance`, excluding setup, sampling, image conversion and
host audio. No competing build/test job ran during this final series.

| Variant | Timed session runs | Median |
| --- | --- | --- |
| Original interpreter | 10.346, 10.480, 10.515 s | 10.480 s |
| VGA wait batching | 8.791, 8.733, 8.726 s | 8.733 s |

The median elapsed time falls by 16.7% for this workload and instrumented native
boundary. This is not a measurement of whole-app CPU utilization, external
synth buffering or performance for other games.

All six runs agree on full printed registers/segment caches, control registers,
guest time, PCM count, MIDI count, file-access counters and frame pixel count.
The independent accelerated inspector run also preserves the original reported
CPU/stack state, frame revision and complete indexed/RGBA/MIDI/PCM hashes.
The final profiling run skips 66,289,200 logical instruction units in 191,796
batches, retaining all 300 million logical steps and 3,593 IRQ entries. Actual
VGA status reads fall from 22,286,841 to 190,441. Local detailed results are
ignored `target/svga-analysis/native-vga-wait-final.json` and
`target/svga-analysis/prince-vga-wait-accelerated.txt`.

### Recorded browser comparison

The release Trunk bundle was compared with a saved previous production Worker
bundle using isolated headless Firefox and the same local game ZIP. All 263 ZIP
input files match the local native installation. Setup, window/UI rendering and
host audio are outside the timed loop; the loop includes execution, MIDI drain
and stream/timestamp hashing, and PCM drain. Frame conversion occurs afterward.

| Variant | 300M-step timed runs | Median |
| --- | --- | --- |
| Previous production Worker | 13.549, 13.582, 13.604 s | 13.582 s |
| VGA wait batching Worker | 12.029, 12.113, 12.084 s | 12.084 s |

The median elapsed time falls by 11.0% at this browser boundary. All six runs
agree on steps, status, guest nanoseconds, frame dimensions/hash, MIDI byte/hash
and MIDI drain timestamp hash. This is a Firefox Worker measurement, not a
Chrome timing result or whole-app CPU-utilization claim.

Reproduce after a release browser build:

```sh
python3 tests/browser_smoke.py --prince --benchmark --benchmark-baseline target/svga-analysis/web-baseline
```

The optional baseline directory contains saved generated Worker JS/WASM, kept
outside Git. Detailed local output is ignored
`target/svga-analysis/browser-vga-wait-final.txt` and
`target/prince-wasm-benchmark.json`.

The full Firefox smoke test also passes Canvas2D startup, ZIP execution,
pause/resume/reset, bounded transfer channels, progress during an 800-ms window
stall, host-MIDI mock recovery and a real PRINCE run beyond 300 million steps.
That run has a nonblank 640x480 frame, 269 MIDI note-ons, nonzero Rust worklet
samples and zero reported underruns. Its audio sink is muted; these assertions
do not constitute an independent listening test or real external MIDI playback.

## Proposed graphics HLE

A verified module's entry points could dispatch to Rust fill/blit routines at
the runtime boundary, on both native and WASM targets. Keep RAM/VRAM under the
single machine owner, operate on borrowed ranges and split work at finite
budgets/device deadlines. Validate the loaded code/ABI rather than trusting a
filename or magic header. Unknown or changed code must execute normally.

Begin with differential tests against original rectangle-fill and RAM-copy
execution. Compare VRAM, bank/cache, stack memory, registers, flags, returned
control flow and emulated peripheral time, including bank boundaries, swapped
coordinates, odd widths and source stride. Native graphics HLE is not direct
execution of guest x86 code or hardware virtualization; the remaining program
still runs in the interpreter. The measured step fraction supports investigating
VGA wait batching first, without promising that driver HLE alone will fix CPU use.
