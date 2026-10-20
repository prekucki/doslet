# HEROES.EXE wait and polling inventory

The local Heroes of Might and Magic main menu keeps executing while music
plays. This investigation identifies polling sites and separates them from
audio processing, event dispatch and ordinary backward transfers. The inventory
also documents an optional native experiment for three counter-wait sites below.

## Recorded scope

Two ISO-backed runs recorded 50 million individual execution steps: startup
steps 0..45M, and main-menu steps 350M..355M after Esc at 100M. Both used the
RAM file overlay; the original installation, ISO and configuration were unchanged.
The trace recorded no HLT execution and dropped no entries. The main-menu
sample includes 3,433 INT 33h/000Bh relative-motion queries.

The executable SHA-256 is
`0e87d3025f437158dfb9099ca2921c83bac5bf9e4708bad04e4d5fc8297218a2`;
SB16.DIG is
`c8a1e7a0e14c8b696e665520e338711e4c20cc3bb4733f83527add41dd6f6b38`.
Addresses below are linear guest addresses for this loaded version. The nested
LE header is at file offset 37CF4h. Its executable object, with load bias
18E000h, occupies 19E000h..23F527h; the data object is excluded from scanning.
Object flags and layout follow the
[Open Watcom LE definitions](https://github.com/open-watcom/open-watcom-v2/blob/master/bld/watcom/h/exeflat.h).
The loaded SB driver uses 16-bit code at base 17B50h.

## Wait sites

| Addresses (hex) | Condition and behavior | Recorded evidence |
| --- | --- | --- |
| 224274..224277; 22427A..22427D | VGA retrace end/start, port 3DAh, bit 3 through TEST AL,AH | Second loop repeated 1,855,270 times across both traces. This form already has validated batching. |
| 22E077..22E07A; 22E07C..22E07F | VGA retrace using TEST AL,08h; port derived from BIOS CRTC base + 6 | Not executed in these traces; not batched. |
| 22F783..22F789; 22F78B..22F791 | VGA retrace using TEST EAX,8; outer LOOP at 22F793 counts frames | Not executed in these traces; not batched. |
| 17E67..17E6A; 17E6C..17E6F | SB driver waits for VGA transitions; outer LOOP at 17E71 counts frames | Startup repetitions 3,781 and 173 respectively; not batched. |
| 18131..1813C | DSP write readiness: port 22Ch, busy bit 7, bounded timeout | 15 ready reads; no polling repetitions. |
| 18160..18165 | DSP read readiness: port 22Eh, bit 7, bounded timeout | Both reads already ready. |
| 1817D..1817E | Fixed reset-pulse delay through 20 port 226h reads | 19 LOOP repetitions; this is a counted delay. |
| 18186..18198 | Reset response: wait on 22Eh, read 22Ah, compare AAh, bounded timeout | First response succeeded. |
| 1E281C..1E2823; 1E2881..1E2888; 1E331A..1E3321 | Tight loops until dword at 281098h becomes zero | Three new pure memory-wait candidates; absent from both recorded paths. |
| 205C24..205C31 | Wait until pointed-to timer reaches zero while calling event pump 206FC0h | Cooperative wait; cannot omit the event pump. |
| 22B181..22B18B | INT 21h/AH=2Ch until DOS seconds change | 10,898 startup repetitions. |
| 22B191..22B198 | Same clock query, counting iterations to calibrate delay | 12,977 startup repetitions; preserving the measured count matters. |
| 22B1CF..22B1D6 | Calibrated counted delay with clock queries | Branch follows decrement flags, rather than comparing returned time. |

The three memory waits read the second slot in a six-dword timer array starting
at 281094h. Routine 219F90h decrements positive slots, with a store at 219FABh.
Several callers write 281098h, including 1CBCADh, 1CBE41h, 1CC2A4h,
1E27BFh, 1E328Dh and 205BF1h. The cooperative wait also uses that timer.
The callback registration and runtime delivery are examined in the
[counter-wait audit](#counter-wait-audit).
These cold sites do not explain the sampled main-menu CPU load.

## Counter-wait audit

All three tight waits execute the same two-instruction operation: compare the
32-bit value at 281098h with zero, then branch back if unequal. They do not call
the event pump, modify registers, or write memory. The guard at 281168h can bypass
them. Their surrounding routines update object direction/frame/position fields;
these are paced object operations rather than the main-menu event loop.

The producer is now traced through the game's timer machinery:

1. Initialization at 21A22Ch registers 219F90h; 21A239h requests frequency 40h,
   or 64 Hz. An alternate initialization path at 21A059h does the same.
2. Frequency conversion at 22F938h computes 1,000,000 / Hz. The observed timer
   slot 2 is active (state 2), points to 219F90h and stores period 15,625 us.
3. Code at 22F299h installs the IRQ0/vector-8 handler at 22EFFEh. It programs
   PIT channel 0 through ports 43h/40h and accumulates per-slot elapsed time.
4. The handler dispatches due callbacks at 22F0ACh. In an actual main-menu run,
   219F90h's stack return address is 22F0B2h, confirming that dispatch path.
5. 219F90h decrements each strictly positive timer slot and increments a tick
   serial at 2831D0h. For the watched slot, the store at 219FABh runs with EAX=4.

An opt-in diagnostic observes each instruction without changing guest code or
data. In main-menu steps 350M..360M (1.212121212 seconds of guest time), it records
76 callback entries, zero visits to all three wait branches, zero cooperative-wait
visits, and no changes to 281098h: it stays zero. Actual callback completion
intervals range from 11.944485 to 28.458666 ms, averaging 16.107277 ms over 75
intervals. The configured 15.625-ms period is therefore nominal; interrupt
latency, callback dispatch and the PIT/software timer phase must be retained.
Observer runtime is not a performance measurement.

An additional scripted New Game/Standard Game attempt sampled steps 350M..440M
and 440M..550M. Both windows also recorded no counter writes or visits to these
waits. The final captured frame showed the animated background without a menu;
this attempt does not establish that gameplay was reached or that the waits are
inactive during gameplay. The main-menu audit was repeated after adding guards
against counting a pre-interrupt PC as an executed instruction, with the same
76 callbacks and interval statistics.

A value of 2 requests two future decrements; it is not invariably a 31.25-ms
delay because the first tick may be imminent and delivery may be delayed.
Negative values are a special case: the callback skips them, while the tight
loops test inequality rather than positivity. Such a value would keep those
loops running unless another path rewrote it. The cooperative variant at
205C24h instead exits on nonpositive values and calls 206FC0h between checks.

These tight loops are suitable candidates for **whole-iteration batching**, not
for setting the counter to zero or jumping directly to the exit. A safe future
implementation needs to validate the actual compare/branch bytes, data mapping,
unchanged memory and flags; preserve eligible interrupts and the phase between
compare and branch; stop strictly before PIT, DSP/DMA or other memory-changing
events; and disable batching for debugging or unsupported state. The ordinary
guest IRQ handler must still perform the decrement, and audio must advance on
the same timeline. Replacing the loop with HLT would change architectural state
and is not an equivalent general solution.

### Experimental native patch

An explicit native flag now enables whole-iteration batching at just these
three instruction offsets:

```sh
target/release/doslet --config games/HoMnM.toml --experimental-heroes-timer-waits
```

It is off by default and is retained on Reset. No executable or mounted file
is modified. The PC layer accepts caller-selected sites and operand addresses;
the native launch adapter supplies the version-specific Heroes values. Every
batch checks the exact nine-byte CMP/JNE sequence, 32-bit code/data access,
ordinary RAM, a positive counter and invariant arithmetic flags. Pending IRQs,
interrupt shadows, disabled interrupts, paging and debug state force ordinary
execution. Batches stop before the next PIT/DSP event and are bounded to 1 ms.
The counter still changes only when normal guest code runs.

The terminal first prints that the experiment is enabled. A separate one-time
`experimental timer waits active; batched ... logical steps` message appears
on the first batch in each session. Without that second message, these sites
have not accelerated execution. The sampled main menu does not use these loops,
so this patch does not promise lower CPU there.

Headless comparison uses `DOSLET_PROBE_HEROES_TIMER_WAITS=1` and prints
`MEMORY WAIT batched_steps=...` at the end. Do not combine a performance test
with instruction-by-instruction auditing: a one-step budget cannot batch.
Differential synthetic tests cover countdown exit, partial budgets, IRQ0,
SB IRQ/DMA/PCM, complete conventional RAM, VRAM, registers, flags and guest time.
Fallback tests include changed code, zero/negative values, incompatible flags,
IF/TF, pending IRQ, paging and the STI shadow. These tests establish selected
batching semantics, not lower CPU in an unrecorded gameplay path.

A paired ISO-backed run to 360M steps, Esc at 100M, with and without the patch
matched SHA-256 for low RAM, 15-MiB extended RAM, physical VRAM, RGB frame and
MIDI output (empty in this SB configuration). Final CPU registers/flags and PCM
frame/nonzero counts also matched. Both reported zero batched memory-wait steps,
as expected for this main-menu path; no native CPU reduction is claimed there.

Reproduce the main-menu audit with the exact local executable version above:

```sh
DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog \
DOSLET_PROBE_HEROES_TIMER_AFTER=350000000 \
cargo run -p doslet-headless --release --features profiling --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 360000000 '100000000:01:1B'
```

`DOSLET_PROBE_HEROES_TIMER_AFTER` is version-specific and opt-in. It reports
callback callers, counter writes, tight/cooperative wait visits and measured
callback completion intervals, then prints the live timer-slot configuration.
It uses instruction-sized execution once the requested step is reached and
does not alter original files, registers or memory outside normal execution.

The outer DSP retry path at 18127..18155 performs recovery/acknowledgment before
retrying. Sound-card probing at 22DABC..22DBC3 performs port writes and indirect
calls. Neither can be treated as a side-effect-free polling loop.

## Repetition that performs useful work

| Addresses (hex) | Work performed | Main-menu sample |
| --- | --- | --- |
| 23BE81..23BE9D | Convert and pack audio samples while advancing input/output pointers | 13,299 repetitions |
| 23D356..23D38C | Scale stereo samples by volume and accumulate into a mixing buffer | 13,299 repetitions |
| 216D70..216D8C | Walk an event-handler list and invoke handlers | 24,038 repetitions |
| 22F04E..22F07F | Update timer counters and phases across slots | 2,145 repetitions |
| 22F094..22F0BC | Dispatch timer callbacks, including an inner counted loop at 22F0B4 | 2,002 outer repetitions |

About 41.7% of the sampled main-menu execution steps occur in 16-bit code.
That is an instruction-frequency observation, not a host CPU-time attribution.
Backward transfers to shared return helpers are also common. A lower jump target
alone does not establish a loop: the scanner follows direct control flow and
stops at returns. Static HLT-looking bytes in tables are not evidence of idle.

## Live GDB audit

On Linux, the headless GDB adapter was exercised against this same executable
with the read-only ISO, default 33 MHz CPU setting, Esc at step 100M, attachment
at 350M, and a 360M hard budget. This is a debugger/correctness observation;
the one-step debugger path is not a throughput measurement.

A 100,000-step hotspot window, 350M..350.1M, dropped no entries. Its two highest
counts were 1,296 at 147EAh and 1,162 at 11FB74h. Breakpoints and individual
steps established that both are useful copies:

| Linear PC | CS base / code default | Bytes and decoded operation | State before → after one step |
| --- | --- | --- | --- |
| 147EA | 14490 / 16-bit | `F3 A5`: REP MOVSW, 16-bit addresses | CX 0010 → 000F; SI 03E0 → 03E2; DI 0F5A → 0F5C |
| 11FB74 | 11F490 / 16-bit | `67 F3 66 A5`: REP MOVSD, 32-bit addresses | ECX 14 → 13; ESI 685C → 6860; EDI 2B0846 → 2B084A |

The PC stays on the REP instruction for each bounded element. Treating these
hot addresses as empty loops would discard actual memory transfers. The event
handler path at 215FC4h..215FECh and list walk at 216D70h..216D8Ch also appeared
frequently (645 observations at individual sites); inspection shows a callback
invocation and next-node traversal, not an invariant polling body.

The loaded code confirms the three comparisons of dword 281098h at 1E281Ch,
1E2881h and 1E331Ah. They were not established as executing waits in this short
window. A breakpoint did catch their producer at 219F90h at step 350,192,368,
guest time 42,447,315,151 ns. Its six-slot decrement loop and increment of
2831D0h match the timer audit above; the second slot was zero on entry and the
tick counter was 09A1h. This distinguishes a confirmed producer visit from an
unobserved consumer wait.

While stopped, GDB changed 281098h from zero to one and restored zero; it also
changed the first opcode at 1E281Ch from 83h to 90h and restored 83h. Neither
modified value was executed while changed. Readback verifies temporary RAM
editing, not the safety of an executable gameplay patch. The EXE SHA-256 before
and after remained the value recorded above. No EXE patch was produced.

The [GDB guide](../debugging.md) provides the startup command, hotspot controls,
register/address conventions and explicit 16-bit GNU objdump workflow. GDB's
default 32-bit disassembly mislabels both REP instructions in these 16-bit
segments, so code width and prefixes must be checked before classifying a loop.

## Reproduce and inspect other paths

### Complete menu caller observation

The generic synchronization analyzer now records a bounded caller iteration
after discovering a nonzero returning countdown guard. In the read-only,
ISO-backed headless menu run at 33 MHz, two enabled measurement windows each
captured the path from just after the compare at `206FC0h` back to that same
instruction boundary and stack frame, with direct caller `2092E2h`. Each trace
contained 1,219 steps over 147,758 ns. There were 262 effect groups, no dropped
effects, no IRQ entry and no unobserved service/device writes. The third enabled
window did not discover a guard.

The full architectural CPU state and input state matched at return. A bounded
ledger of the first old value and latest written value for each modified RAM byte
found zero net changes. A separate snapshot of all currently modeled CMOS
registers and the selected index also matched. The only observed device operations
were these writes, each executed once:

| Linear writer PC | Port | Value |
| --- | --- | --- |
| `16D04h` | `70h` | `0Fh` |
| `16D0Ah` | `71h` | `00h` |
| `14D5Ch` | `70h` | `0Fh` |
| `14D63h` | `71h` | `09h` |

These transactions explain why the existing strict idle proof rejected the
iteration for device accesses even though CPU, RAM, input and modeled CMOS
state returned to their initial values. The analyzer neither suppresses the
writes nor expands the strict skipping contract. The chip currently models CMOS
configuration registers, not RTC ticking/interrupts; snapshot equality is scoped
to that implementation.

This is a complete observed caller iteration, not a complete timed frame or
proof of the whole main loop. The guard's zero branch performs timed work, and
IRQ handlers mix audio and update counters. The continuous measurement described
below now accounts for those paths. Controlled host CPU measurements and adaptive
acceptance across all playable scenes remain required.
The recognition code contains no Heroes addresses; the addresses here identify
the recorded executable only.

### Continuous recurring-caller measurement

An unchanged diagnostic return can install an exact post-CMP boundary for
ordinary execution, including compiled batches. Each actual interval journals
up to 2,048 distinct RAM bytes and compares complete CPU/input state, mapping,
IRQ entries and modeled CMOS state. Only restored writes selecting and updating
CMOS scratch register `0Fh` are admitted device effects. Other device accesses,
interrupts, changed state, journal overflow or unmodeled services reject the
interval. The strict idle proof and its device-access exclusions are unchanged;
this measurement does not skip the interval's instructions.

Four observed IRQ decrements and a fresh actual writer are required before an
unchanged interval can count as waiting. Successive zero arrivals delimit the
complete countdown episodes; timed work, IRQ handlers and all unverified time
remain in the work upper bound. VGA periods use that same continuously measured
waiting total. The controller retains a ceiling-speed cadence reference while
testing lower speeds and restores the ceiling on missing evidence or a measured
deadline miss. See [implementation bounds](../development.md#bounded-synchronization-analyzer).

Two ignored local tests use the hardware and mounts from `games/HoMnM.toml`,
booting at 33 MHz to the established menu entry before restoring its 133-MHz
ceiling. Both synthesize and drain audio without a physical output device:

```sh
cargo test -p doslet --release --locked local_heroes_adaptive_recurrence -- --ignored --nocapture
cargo test -p doslet --release --locked local_heroes_recurrence_overhead -- --ignored --nocapture
```

The first runs the interpreter and an actually executing native JIT for 60 guest
seconds, reports reduction/recovery counts, and checks restoration of the ceiling
when adaptive mode is disabled. It also rejects a controller still stuck at the
ceiling at the end. The second warms the engine and alternates
three analyzer-off/on pairs at fixed MHz, recording host wall time and Linux CPU
ticks over two guest seconds. Adjacent windows are not identical machine-state
replays. Neither test establishes audible music, scrolling or battle behavior.

After the cadence-window correction below, the 2026-10-16 60-second run recorded
identical per-second guest diagnostics with the interpreter and native JIT:
80 gradual reductions, two recoveries, a transient minimum of 23 MHz and
43 MHz throughout seconds 50–60. The learned cadence floor was 38 MHz. Measured
recurring waiting totaled 50.457 seconds; the last VGA period was 166.8 ms with
23.2 ms charged as work. This demonstrates reduction/recovery and settling in
the menu, not an optimal clock or accepted host CPU savings. Its host wall times
are not performance evidence because other builds were running concurrently.

The fixed-133-MHz overhead test subsequently measured these medians across three
off/on pairs, with two guest seconds per window:

| Engine | Analyzer off, CPU ticks | Analyzer on, CPU ticks | Relative overhead |
| --- | ---: | ---: | ---: |
| Interpreter | 164 | 194 | 18.3% |
| Native JIT enabled | 166 | 194 | 16.9% |

The previous implementation repeatedly attempted native store traces while the
RAM journal prevented writable borrows. Selecting decoded batches once per
slice during a valid journal reduced the JIT-enabled observed median from 233
to 194 CPU ticks in adjacent runs. Native execution remains available outside
valid journals; the differential test covers both the recorded interval and its
return to native execution. These are process CPU counters, not milliseconds.
At fixed MHz the analyzer still costs CPU; savings require a successful adaptive
reduction and a separate comparison against analyzer-off execution.

The next live menu run exposed a controller limitation: a five-second fixed
window consumed 452 CPU ticks, whereas adaptive mode returned to 133 MHz after
two recoveries, retained a 131-MHz floor and consumed 507 ticks in its measured
window. Both windows kept guest time aligned with wall time and added no host
audio underruns. This was a failed savings comparison, not adaptive acceptance.
At fixed 133 MHz, 22.3 further guest seconds of status sampling captured typical
166.8–183.5-ms periods and isolated 333.7/350.3-ms periods, also without underruns.
Thus one doubled period did not establish insufficient throughput.

The correction retains a 16-period cadence window across retuning and excludes
one longest period from its mean. Continuous work accounting still reacts
immediately to the reserved work budget; a long phase dominated by confirmed
waiting is not an in-flight work miss. Synthetic cases distinguish isolated
long waits, sustained slower cadence and expensive unfinished work. Recovery
discards the lower-speed period window before making another reduction, avoiding
attributing old slow periods to the new speed.

The next live menu comparison (three alternating pairs in the
order fixed/adaptive, adaptive/fixed, fixed/adaptive) still failed consistency:

| Pair | Fixed CPU ticks / wall ms | Adaptive CPU ticks / wall ms | Effective adaptive MHz | CPU saving after wall-time normalization |
| --- | ---: | ---: | ---: | ---: |
| 1 | 459 / 5028 | 411 / 5024 | 105 | 10.4% |
| 2 | 459 / 5010 | 413 / 5050 | 105 | 10.7% |
| 3 | 453 / 5011.5 | 516 / 5023.5 | 133 | -13.6% |

All six windows kept guest time within 0.2% of wall time. The first fixed window
added 325 missing host PCM frames; the other five added none. A listening check reported
smooth audible menu music. The two successful windows share one calibration;
the third followed a fresh calibration and retained a 133-MHz floor on the
retrace-end predicate. Its reported periods ranged from about 1.02 to 3.20 seconds
while the retrace-start predicate normally reported 166.8–183.5 ms. These are
diagnostic observations, not evidence of a constant deadline at both sites.
The raw snapshots are in the local ignored
`target/heroes-menu-live-cadence-1007096.json`.

The next correction rejects a variable ceiling-speed cadence as a deadline
model and clears its learned floor. A regression test also exposed a source-phase
error: the phase was queried after Jcc, even though Jcc uses the earlier IN
result. The analyzer now preserves the phase captured at IN across the rest of
the condition and any IRQ. Tests cover a device edge between IN and Jcc, paired
retrace-end/start polling, and rejection of variable reference periods. Live
acceptance of these changes and current-version comparisons in the remaining
scenes are still outstanding. Repeating the 60-second headless test with these
corrections retained the same per-second interpreter/JIT results and final
43 MHz (`target/heroes-cadence-read-phase.log`); this startup did not exercise
the intermittent retrace-end cadence seen in the live calibration.

### Guest audio IRQ service measurement

The subsequent local run with DMA IRQ diagnostics used the same startup and
60-second adaptive menu procedure (`target/heroes-audio-deadline.log`). Both
interpreter and executing native JIT recorded 2,094 complete consecutive DMA
block service windows and zero IRQ acknowledgement misses, counting startup
as well as the measured menu interval. The guest used command C6h, 8-bit stereo
DMA at 22,050 frames/s; the complete DSP diagnostics matched between engines.
The controller still finished at 43 MHz, with two episode-based recoveries.

This establishes that the generic block-IRQ diagnostic observes a real Heroes
audio path. It checks acknowledgement before the next block of a continuous
transfer completes; it does not observe guest buffer-refill deadlines or audible
playback. A new miss restores the adaptive controller's configured ceiling;
synthetic tests exercise that response and distinguish historical misses,
diagnostic-only mode, wrong-width acknowledgements, pause/reset, and CPU clock
changes. MCP exposes observation counts and the explicit source, returning
unknown/null until a complete window exists. Native Details separates those
counts from missing host PCM frames.

The next live instance was verified as Heroes in its main menu, with the current audio and variable-cadence fields. With no debugger
attached or concurrent build, two alternating fixed/adaptive pairs measured:

| Pair | Fixed CPU ticks / wall ms | Adaptive CPU ticks / wall ms | Adaptive MHz at window boundaries | CPU saving after wall-time normalization |
| --- | ---: | ---: | ---: | ---: |
| 1 | 886 / 10011 | 593 / 10029 | 118 → 50 | 33.2% |
| 2 | 887 / 10012.5 | 442 / 10024 | 50 → 50 | 50.2% |

Each window added 215–216 observed DMA block IRQ service windows, zero missed
acknowledgements and zero missing host PCM frames. Guest time tracked wall time
within 0.1%. The first adaptive window still included settling after a cadence
recovery; it is not a steady-speed measurement. The two adaptive windows share
one calibration. A third fixed window measured 894 ticks / 10014.5 ms; the fresh
adaptive calibration was interrupted after a listening check reported audible gaps or
clicks. The controller was disabled and the configured 133 MHz restored.

A subsequent fixed-speed 10-second window added no missing host frames or guest
IRQ misses and kept real-time pacing. These counters do not establish audible
continuity or identify the cause of the reported gaps. A repeat comparison was
interrupted before its adaptive window completed and is excluded from the table.

A restarted instance completed a fresh third menu pair:
fixed 133 MHz used 839 CPU ticks / 10009.5 ms, while adaptive 66 MHz used
532 ticks / 10027.5 ms (36.7% lower CPU cost after wall-time normalization).
The fixed window added 511 missing host PCM frames; the adaptive window added
none. Both added 216 guest DMA IRQ checks and zero misses. A listening check reported
smooth audible music during the repeated adaptive menu measurement. This
does not retrospectively explain the earlier audible disturbance. The three
completed pairs therefore span two processes, each with its own fixed/adaptive
comparison; their adaptive endpoints and calibration histories differ.

MCP relative movement and clicks then loaded the built-in TUTORIAL save and
opened View Puzzle. The controller returned to 133 MHz while entering gameplay;
no game files were modified by this navigation.

### Puzzle and city coverage before backedge recurrence

The restarted instance completed three alternating pairs per scene, with approximately
five-second windows, analyzer-off fixed 133 MHz, and a fresh adaptive calibration
before the third pair. The first two adaptive windows shared their calibration.

| Scene / pair | Fixed CPU ticks / wall ms | Adaptive CPU ticks / wall ms | Adaptive MHz | Normalized CPU saving |
| --- | ---: | ---: | ---: | ---: |
| Puzzle 1 | 483 / 5012 | 330 / 5023.5 | 78 | 31.8% |
| Puzzle 2 | 475 / 5014 | 325 / 5028.5 | 78 | 31.8% |
| Puzzle 3 | 477 / 5012 | 416 / 5023 | 99 | 13.0% |
| City 1 | 282 / 5011.5 | 287 / 5013 | 133 | -1.7% |
| City 2 | 280 / 5012.5 | 286 / 5018 | 133 | -2.0% |
| City 3 | 281 / 5011.5 | 287 / 5026.5 | 133 | -1.8% |

All twelve measurement windows added zero host PCM missing frames and zero
guest DMA IRQ ACK misses; each observed 107–109 block service windows. Host PCM
missing frames did increase during puzzle calibration outside those measurement
windows, so steady-window results must not hide that transition cost. A listening check
reported smooth audible music in both scenes. The city was the TUTORIAL town
screen with animated buildings and no building dialog. Raw measurements and
structured candidate/caller snapshots are in the local ignored
`target/heroes-city-puzzle-live-before-backedge.json`.

The city result is a coverage failure, not a measured requirement for 133 MHz.
The analyzer found VGA episodes of about 167 ms but no returning countdown guard
or recurring caller. Consequently, most of each period remained conservative
work, even when it consisted of repeated event dispatch. A separate bounded GDB
sample executed 500,000 steps (15.037594 ms of guest time) and recorded 7,547
visits to each of the linked-list dispatcher instructions at 216D70h–216D8Ch,
with zero dropped hotspot entries. That path invokes handlers through
`CALL [ESI+8]`; the enclosing event loop returns from 219D8Fh to 219D1Ah. These
counts locate executed work; they do not by themselves prove waiting. GDB was
detached and the game resumed after the sample. The local code capture is
`target/heroes-city-code-1109343.bin` and is not redistributed.

The next implementation admits dynamically observed unconditional backedges as
recurrence boundaries, after fresh VGA cadence evidence exists. It requires eight
consecutive unchanged executions before crediting any interval, then independently
checks CPU/input/net-RAM/CMOS state and IRQ/device-access marks for every interval.
Animation, changing handlers and interrupted or incomplete intervals remain work.
No Heroes address is used in discovery and no instruction is skipped. New live
comparisons are required for this extension and for the requested native-CPU
release build; the table above does not validate either change.

The first live native-CPU build discovered a recurring city
boundary at 215FC4h without a returning IRQ guard. Complete VGA episodes changed
from approximately 160 ms of charged work to approximately 17 ms of work and
150 ms of verified waiting per 167-ms period. This demonstrated missing coverage
but not stable adaptation: gradual MHz changes repeatedly discarded the anchor
and restored 133 MHz. A regression reproduced a freshness error: retuning resets
the open episode, so its replacement can finish two presentation periods after
the last complete episode, exceeding the 250-ms source timeout even though VGA
exits continue. Source freshness now follows those actually observed exits;
the eight-complete-episode requirement for a new budget is unchanged. The raw
diagnostic and transition snapshots are retained locally in
`target/heroes-city-native-backedge-1131114.json`. Live acceptance of that fix
is still required. A 120-second headless run then exposed another reset: eight
consecutive changed animation-handler executions discarded an already qualified
boundary. Qualified probes now retain that boundary for at most 250 ms without
an unchanged interval; unqualified probes still stop after eight failures.
Regression tests cover a burst of 16 changing iterations with no wait credit,
and bounded release of the journal when changing work or nonrepeatable services
continue. No changed execution is reclassified as waiting.

With that fix, the 120-second headless city run retained the same qualified
boundary and settled at 19 MHz. However, it completed 265 VGA episodes in the
last 50 seconds (about 189 ms per episode, versus a 170-ms ceiling reference).
That approximately 11% slowdown fitted the old 12.5% mean tolerance, so this
result is not accepted as preserved animation cadence. The controller now uses
a 3.125% plus 100-us mean slowdown threshold, while retaining the wider admitted
spread of individual ceiling-speed frames. A regression covers sustained 5%
slowdown, not only doubled periods. The updated headless run also checks that
at least 4,000 music DMA service windows complete during the four-minute scene,
and reports the current cadence mean explicitly. Live comparisons remain required.

The stricter run exposed missing VGA observations after an IRQ return changed
the continuation. The IRQ observer retained its old attribution scope until
the 4,096-step limit and consumed foreground port loops without notifying the
VGA episode observer. The two polls then appeared to have different cadences;
one stale period grew beyond one second and kept restoring 133 MHz. Temporary
entry/exit counters confirmed the lost polls inside the IRQ probe. They were
removed after diagnosis.

IRQ attribution now ends conservatively at a redirected IRET. Caller, IRQ and
foreground instruction probes also share the port boundary observer; traced
IRQ/caller time is still charged as work. Synthetic regressions reproduce the
old foreground-write misattribution and a two-phase VGA poll whose first phase
finishes inside an IRQ trace while its second outlives the trace budget. Both
pass with the fix, along with the initial-wait phase regression.

With `RUSTFLAGS='-C target-cpu=native'`, the corrected headless TUTORIAL-city run
completed 240 guest seconds. It recorded 5,170 DMA IRQ service windows with zero
misses and held 78 MHz throughout the last 60 seconds without another recovery.
Final cadence means were 166.832 and 168.010 ms, against ceiling references of
170.169 and 171.564 ms. Four recoveries occurred during initial learning and
lower-throughput validation. This validates the recorded guest workload and
cadence checks, not audible output or host CPU savings. Same-build live
comparisons are recorded separately.

The first live comparison of that build (native JIT, no GDB
client) used three alternating 10-second pairs in TUTORIAL's Vulcania city.
Fixed 133 MHz consumed 6.15/6.14/6.11 CPU seconds; adaptive consumed
5.86/5.89/7.09 CPU seconds at 105/105/133 MHz. The third calibration raised its
cadence floor to 123 MHz and retained the configured ceiling. Thus the first
two pairs saved about 4–5%, but the third added about 16%; repeatable savings
were not established. No adaptive sample added host PCM missing frames or
guest DMA IRQ misses. The first fixed sample added 512 missing PCM frames.
No audible-playback check was recorded.

A subsequent fixed-speed diagnostic comparison found that completed work
episodes could omit many actual port boundaries. A synthetic protected-mode
IRET bridge reproduced premature disposal of a suspended foreground VGA
condition. IRQ attribution still ends at its first unmatched return, but the
independently bounded VGA condition now waits for the exact foreground return
or its existing 20-ms expiry. The repeated 240-second headless run passed with
5,170 DMA service windows and zero misses. Both port candidates completed 353
episodes in the final minute; the previous build completed only 237 at the
second poll and 353 at the first. Final cadence means were 167.944 and
168.010 ms, and throughput stayed at 78 MHz throughout that minute. Current
live comparisons remain in progress; the first live results above do not
validate this additional fix.

The next live build measured 6.03/6.11/6.10 CPU seconds at
fixed 133 MHz and 5.36/5.46/6.19 with adaptation, at 93/93/111 MHz. The first
two pairs saved 11.1% and 10.6%; the third added 1.3%. No adaptive window added
PCM missing frames or DMA IRQ misses. This improved measurement coverage but
still did not establish a repeatable CPU saving in every calibration.

Three alternating diagnostic off/on pairs at fixed 133 MHz isolated the
analysis cost. CPU seconds were 6.05/6.07/6.05 off and 7.15/7.13/7.21 on,
giving a median wall-normalized overhead of 18.1%. Moving source validation
to actual recurrence arrivals reduced this median to 12.7%:
6.14/6.19/6.11 seconds off versus 6.85/6.96/6.89 on. Code mutation checks,
5-ms journal cancellation and 250-ms expiry still run away from the boundary;
source validity is mandatory before any interval receives wait credit. The
synthetic source-loss, mutation, state-equivalence and allocation checks pass.
Both builds use `-C target-cpu=native` and native JIT. The paired windows use
the same city view, but do not restore identical guest RAM or animation phase.
Host PCM missing frames occurred in both conditions; none of these windows
missed a guest DMA IRQ service deadline.

The same process then completed three adaptive city pairs. Fixed CPU seconds
were 6.08/6.21/6.20; adaptive seconds were 6.73/6.59/3.30 at 125/125/41 MHz.
Wall-normalized savings were -10.6%, -6.1% and 46.8%, so the lower analysis
overhead did not resolve calibration sensitivity. No adaptive window added
host PCM missing frames or guest DMA IRQ misses. The first fixed window added
393 missing frames. The 240-second headless test still passed at a final
78 MHz with 5,170 DMA service windows and zero misses.

Inspection found that the first ceiling reference could be frozen after eight
episodes even though its rolling window holds 16. A regression with a fast
initial half-window reproduced the premature reference. Initial calibration
now requires all 16 ceiling-speed episodes, as recalibration already did.
The 3.125% mean slowdown limit and prompt work/audio recovery remain unchanged.
This removes the demonstrated sampling inconsistency. Its 240-second headless
city run passed at a final 99 MHz, with 5,170 DMA service windows and zero misses;
that result alone does not establish improved savings.

Live menu validation then exposed a separate selection failure: a frequently
visited returning guard had no attributed IRQ decrements and credited zero
recurring intervals, yet prevented the scout from trying any direct backedge.
The controller correctly stayed at 133 MHz, but could not measure the spare
capacity. A regression reproduced that starvation. A freshly observed VGA
source can now admit a direct-backedge probe when the selected guard has no
fresh source evidence. Guards with observed sources and existing backedges
retain their boundary; all interval equality checks still apply.

That selection change alone did not restore menu adaptation: the ignored menu
test failed with zero verified recurring wait. The live instance tried 38
backedge probes and rejected all 304 changed intervals. Inspection of the
existing menu recipe identified a conditional `JZ` at its outer backedge;
the generic scout admitted only unconditional branches. It now admits taken
direct conditional branches too, requiring the observed destination and all
the existing interval checks. An interpreter/JIT differential regression covers
the repeated conditional path and its normal exit after state changes. This
extends generic discovery, without importing the recipe's game addresses or
skipping instructions. The corrected headless menu test passed with interpreter
and JIT; both finished at 78 MHz after 60 guest seconds. The 240-second city
test passed at a final 99 MHz with 5,170 DMA service windows and zero misses.

The native build (SHA-256 `6307a359b9708ee52f92961e459c9b4cbdb610f16502c94c082c75e0def7fb57`,
`-C target-cpu=native`, JIT enabled) then completed three 10-second pairs per
scene after background builds/tests had stopped. Menu CPU seconds were
9.07/9.11/9.09 fixed versus 7.99/6.89/7.76 adaptive, saving 12.0%, 24.5% and
14.8% after wall normalization. Adaptive MHz were 99, 83 and 83→125; the last
window includes a recovery. City pairs were 6.13/5.91/5.94 versus 5.62/5.28/5.23,
saving 8.5%, 10.8% and 12.1%, at 125→99, 99 and 118→78 MHz. These adjacent
animation windows do not restore identical guest state. None added host PCM
missing frames or guest DMA service misses.

Puzzle validation rejected this version. Its first two adaptive windows used
10.55 and 10.64 CPU seconds versus fixed 9.56 and 9.41 seconds. Guest time
advanced only 9.786 and 9.657 seconds per roughly 10 seconds of wall time;
the windows added 9,266 and 17,068 missing host PCM frames. DMA deadlines alone
did not reveal this host-side shortfall. The cadence floor reached 133 MHz and
remained there even when ceiling-speed measurements changed its reference.
A regression reproduced the stale floor: a fresh reference could make the
original failed period acceptable, yet the floor survived. Recovery now requires
a full ceiling-speed calibration before reduction, and retains the failed mean
so only that independent calibration can disprove its floor. Reduced-speed
measurements cannot relax it. Current-build puzzle, map, battle and repeat
menu/city comparisons remain required; no audible acceptance is claimed.

### Experimental menu idle and live CPU clock

The desktop **Details → CPU MHz · live** control changes CPU throughput without
resetting the game. The MCP `set_cpu_mhz` tool accepts `{"mhz": 33}` with the
same behavior; status reports `cpu_mhz`. Both controls refuse changes while GDB
is attached. The profile file is not rewritten.

For this executable version, enable **Details → Try Heroes menu idle**, or:

```sh
target/release/doslet --config games/HoMnM.toml --no-dynamic-core \
  --experimental-heroes-menu-idle
```

This experiment targets the outer menu loop at 217DA2h, rather than the three
counter waits. A live desktop sample measured 816 iterations with event type
zero in 30.075188 ms of guest time. There was one timer callback and one timed
update, with no visits to the known VGA/counter waits in that window.

The policy checks the current loop signature, callback 2092DCh, empty event,
animation countdown 2810A8h and reentrancy guard 2832C0h. It waits at most 0.5 ms,
advancing devices at their individual deadlines and resuming CPU for eligible
IRQs or input changes. It changes polling cadence; it is not an exact-state
optimization. Disable the toggle to return to ordinary execution. Neither RAM
code nor original game files are patched. The accumulated idle-time label
confirms whether recognition actually occurred.

Host PCM uses two refill watermarks during this experiment. Their size adapts
to the largest observed callback block, with a minimum 30-ms high target and an
80-ms upper bound. The guest still runs its own audio handler at normal IRQ
delivery; the reserve is not a substitute for executing that handler.

The local ignored test includes Rust audio processing without a physical output
device, and alternates ordinary/experimental execution in the menu:

```sh
cargo test -p doslet --release --locked local_heroes_menu_idle_experiment -- --ignored --nocapture
```

This checks activation and runtime faults. Audible playback, clicks and full
gameplay require separate interactive verification; exact RAM hashes are not
expected to match after deliberately changing polling cadence.

Five adjacent off/on windows of 0.5 guest seconds at 133 MHz, after startup at
33 MHz and Esc at 100M steps, measured median host wall times of 390.62 ms and
62.80 ms. The experiment accounted for 403.8..419.1 ms of CPU inactivity per
enabled window. Rust audio processing remained enabled and DSP PCM production
continued. These are short sequential menu observations, not process CPU-time
measurements or proof of audible playback; the windows start at different guest
states.

On the live adventure map, the menu idle counter stopped increasing. A fresh
one-million-step GDB sample recorded about 14,305 visits to individual linked
list dispatch instructions at 216D70h..216D8Ch, with no dropped hotspot entries.
The map enters the dispatcher through 217A00h from 219E03h. Its outer loop starts
at 219D1Ah, fetches an event through 213F4Ch, traverses registered handlers and
returns through 219D8Fh. A captured event had type zero. The adaptive experiment now includes this specific outer loop, requiring
EBX=2832C4h, CL=0, event word [SS:ESP]=0 and completed iterator [EBX+8]=0,
as well as exact head and backedge bytes. It does not skip individual handlers.
The new recipe is experimental: synthetic guard tests establish recognition and
rejection behavior, not map or battle compatibility or measured CPU savings.

The controller grows waits from 125 us to 1 ms after repeated empty returns,
backs off on display changes, and reserves 100 ms of normal execution after
host input. It excludes map edge positions and held mouse buttons. Periodic
hardware events continue to run and eligible IRQs end parking. The configured
MHz does not change. This can alter polling cadence and is not an instruction-
or state-equivalence optimization. The earlier fixed-interval menu measurements
above do not measure the adaptive implementation.

### Adaptive controller validation

A local release interpreter run at 133 MHz alternated disabled/enabled five
pairs of 500-ms menu windows, synthesizing and draining audio. Median host wall
time was 395.537 ms disabled and 44.330 ms adaptive (about 8.9 times faster).
This is elapsed host time, not process CPU time or a battery measurement.
The guest PCM stream continued at 22050 Hz; audible output was not tested.
Map scrolling and battle behavior still need interactive validation on the new
binary. An interactive report of usable battle music and cursor response at
8 MHz concerns the earlier running binary, not validation of this controller.

Synthetic tests cover interval growth/capping, display/input backoff, exact
code/context/edge rejection, CPU eligibility and allocation-free waiting. The
workspace tests, Clippy, PC profiling/native-core tests and WASM worker check
pass. The adaptive policy remains disabled by default. Enable it with
`--experimental-heroes-adaptive-idle` or **Try Heroes adaptive idle** in Details;
keep the CPU at the desired active-gameplay MHz.

### Trace other paths

The opt-in host tracer records actual backward branches, successful port reads,
BIOS/DOS services, HLT and code-segment metadata. It observes the machine and
uses host allocations outside emulation. Once enabled, one-step execution makes
poll iterations visible; its runtime is unsuitable for a performance benchmark.

Capture startup and then the main-menu window, including RAM snapshots:

```sh
DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog \
DOSLET_PROBE_WAIT_AFTER=0 \
DOSLET_PROBE_WAIT_OUTPUT=/tmp/doslet-heroes-startup-waits.json \
cargo run -p doslet-headless --release --features profiling --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 45000000

DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog DOSLET_PROBE_RAM=1 \
DOSLET_PROBE_WAIT_AFTER=350000000 \
DOSLET_PROBE_WAIT_OUTPUT=/tmp/doslet-heroes-menu-waits.json \
cargo run -p doslet-headless --release --features profiling --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 355000000 '100000000:01:1B'
```

Scan loaded executable objects and sampled code-segment spans with Python
stdlib and GNU objdump; no Node tooling is involved:

```sh
python3 tools/wait_loops.py /tmp/doslet-probe-extended.bin \
  --le games/HoMnM/HEROES.EXE --bias 0x18e000 --observed-segments \
  --trace /tmp/doslet-heroes-startup-waits.json \
  --trace /tmp/doslet-heroes-menu-waits.json \
  --output /tmp/doslet-heroes-waits-all-high.json

python3 tools/wait_loops.py /tmp/doslet-probe-low.bin --ram-base 0 \
  --region SB16.DIG:0x17cba:0x18664:16 --observed-segments \
  --trace /tmp/doslet-heroes-startup-waits.json \
  --trace /tmp/doslet-heroes-menu-waits.json \
  --output /tmp/doslet-heroes-waits-all-low.json
```

The JSON inventories contain all decoded backward-branch/HLT candidates in the
declared regions, including ordinary work and suspected inline data. They carry
classifications, addresses, control-flow uncertainty and observed branch/port
counts. Matching uses source, target and code mode. They are diagnostic artifacts,
not a list of loops that can safely be skipped. Linear decoding, indirect flow,
overlays and unrecorded gameplay paths prevent a claim that every possible wait
has been identified. Game bytes and RAM dumps remain local, outside the repository.

Validation: the Rust trace regression compares machine registers, RAM and guest
time against an untraced run. Four Python tests cover executable-region selection,
polling versus backward transfers, flag waits versus pointer traversal, and trace
merging. Workspace Clippy with profiling passes. No additional acceleration or
native UI CPU reduction is claimed by this inventory.

## Portable-state headless comparison, 2026-10-17

The [portable-state tool](../save-states.md) now restores self-contained Protobuf
fixtures instead of navigating the live GUI for every trial. Menu, puzzle,
Vulcania city and a centered-pointer map fixture were visually checked. Every
capture compared the original and restored owner over one million further
steps. No battle fixture was captured in this pass.

Native builds used `RUSTFLAGS='-C target-cpu=native'`. Each trial loaded the same
scene bytes, warmed up for five guest seconds, then measured ten guest seconds.
Three pairs alternated fixed/adaptive order. No builds or other headless trials
ran concurrently. These are medians of wall seconds per actual guest second,
not desktop CPU utilization or steady-state controller results:

| Scene | Fixed 133 MHz | Adaptive | Cost change | Final adaptive MHz |
| --- | ---: | ---: | ---: | ---: |
| Menu | 0.8088 | 0.7766 | -4.0% | 88 |
| Puzzle | 0.8985 | 0.9739 | +8.4% | 105 |
| City | 0.5100 | 0.4986 | -2.2% | 88 |
| Alternating map-edge input | 0.4584 | 0.4831 | +5.4% | 125 |

All 24 trials retained approximately 22,050 PCM frames per guest second and
reported zero SB16 DMA deadline misses. The final scrolling PNG still shows
discovered terrain. These counters do not establish audible desktop continuity.
The small menu/city gains do not establish stable savings; puzzle and scrolling
still show performance regressions. The general controller is not accepted on
this evidence. The fixtures make those regressions repeatable without a GUI.

Local artifacts are `target/heroes-states/{menu,puzzle,city}.pb.bench.json`,
`scroll.pb.scroll.bench.json`, adjacent PNGs and `summary.json`. Fixture FNV-64
identifiers are `313bdba4dbe3c291` (menu), `d4a38ac30ec0e102` (puzzle),
`266083d545f74b46` (city) and `8be3ad3f9630ded2` (scroll). The measured executable
identifiers are `584f8050e3212b83` for menu and `26f335e7fa30fde7` for the other
scenes; the intervening change only removed corner clamping from scroll setup.
The later verification-only change compares complete timestamped PCM frames and
MIDI bytes rather than a sample digest; it does not change timed execution.
