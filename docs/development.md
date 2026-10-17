# Developing Doslet

Validation commands, implementation status, headless diagnostics and
execution-engine details. For everyday use, see [using Doslet](usage.md).

For native desktop/headless remote debugging, RAM patches and mixed 16/32-bit guest
inspection, see [debugging with GDB](debugging.md).

For the optional DOS/4GW host, direct BIOS/DOS dispatch experiment, allocation
telemetry and bounded game comparisons, see [experimental DPMI](compatibility/dpmi-host.md).

For repeatable scene tests without a window or audio device, see
[portable guest states and Heroes fixtures](save-states.md). The optional
Protobuf format uses checked-in Rust bindings and does not require `protoc`.

To investigate idle behavior without a game-specific recipe, add
`--observe-idle` to the desktop command or choose **Observe idle loops** in
Details. The observer works independently of Heroes adaptive idle. **Idle
candidate** identifies sampled recurring execution with input queries, not a
proven empty loop; Details lists writes, device accesses and uncertainty reasons.
MCP `status` exposes the same window in `idle_observer_*` fields. Keep the guest
running and avoid input for at least 250 ms of guest time to collect a window.
Details also shows **Synchronization**: a frequently observed supported
RAM/port condition, comparison or mask, taken/fallthrough evidence (aged between
bursts), value changes, and the last traced step across which a watched RAM value
changed. IRQ-pending and device-access flags describe that step boundary; they
do not identify the writer. This runs limited interpreter bursts even with JIT
enabled. MCP exposes the same diagnostics in `synchronization`. **Provisional budget**
shows the observed recurrence peak, reserve, next known service source and
remaining time/nominal steps. It requires four recent samples at unchanged MHz.
It does not measure useful-work demand or prove that the condition waits for that
service. The toolbar shows **Trace: port** or **Trace: RAM** when trace evidence
exists even if the display-based candidate test rejects the screen. Disable the
Heroes idle recipe when comparing guest state with reference execution.
A local Heroes menu trace independently discovered the dword comparison at
206FC0h against RAM 2810A8h (value 2 compared with zero), and VGA port 3DAh tests
with mask 08h at 22427Ah. These addresses are observations, not decoder recipes.
The sampled run did not capture the RAM counter's writer, so IRQ/timer ownership
is not established by this diagnostic. Each 500-ms window at 133 MHz traced
4,608 scheduling units; this is coverage evidence, not a host overhead benchmark.
This diagnostic does not reduce CPU use by itself. A local Heroes menu run
recorded 279 samples, 6,559 input queries and recurring PCs in one window with
the Heroes recipe disabled. It rejected candidacy because the display revision
changed. That is real-game diagnostic evidence, not validation of generalized
sleeping, map behavior or battle compatibility.

To try actual address-independent waiting, use `--adaptive-idle` or **Enable
learned idle** in Details. It enables tracing automatically and replaces the
fixed Heroes recipe. `Trace: RAM/port` means observation; `Idle N%` reports actual
time advanced without executing the CPU. This experimental heuristic learns
changing countdown guards and caps each wait at 250 us. Disable it for exact
reference comparisons or if game responsiveness changes.
Details also reports **Learned guards** arrivals and the last checked IP and eligibility reason:
relearning counter changes, a due counter, an approaching service deadline or
an input constraint. The observation point refreshes learning even when sparse
trace bursts miss that function. The reason describes the last evaluated guard;
input grace and time spent elsewhere can also prevent current waiting.
The learned policy does not use pointer position or screen edges to infer CPU
demand. Actual input still reserves a short grace period before waiting.
**Empty-poll waits** reports the additional budget at empty relative-motion
queries. Its work peak excludes the emulator's own waits; increased work leaves
less spare time. It targets short polling intervals while reserving hardware
service time, without game addresses or screen-specific rules. It remains an
experimental cadence change; check animation speed, responsiveness and audio.
Two candidate points retain separate evidence. A live city capture found 96
positive and four zero countdown values in 100 calls, next to a static zero
guard. A regression test models this competing-guard sequence; actual city
coverage cannot be inferred from the menu test. The subsequent live Necropolis
city measurement at 133 MHz still advanced only 0.424% of 19.58 guest seconds
through countdown waiting, with no input wakes. After adding empty-poll budgets,
a live Tundara city run advanced 41.20% of 23.01 guest seconds through cooperative
waiting, with one input wake and observed inter-query work around 98–123 us.
These are runtime idle counters on different city screens, not a paired host
CPU benchmark or evidence of identical guest state. Audible playback and game
responsiveness still require user observation.

The ignored `local_heroes_learned_idle_experiment` test boots the local read-only
Heroes image with a RAM overlay. After arrival-based learning, a menu run at
133 MHz advanced 8.31 of 10 guest
seconds through learned waiting, with ongoing nonzero PCM output. This verifies
activation in that menu, not puzzle/battle compatibility or audible playback.

## Comparison baseline

The annotated tag `baseline/pre-adaptive-cpu` points to commit
`cd57ff9c4280095685c4c9343aeff6e26d8898b1`, the `origin/main` revision selected
before the adaptive-profile and portable-state changes. Use this immutable tag
for comparisons; `origin/main` can move.

Build both revisions with the same toolchain, release options and CPU target.
Use separate build directories, identical guest inputs and clock settings, a
warmup, and at least three alternating pairs. The baseline predates portable
states: replay an identical input sequence from startup for comparisons with
that revision. Snapshot comparisons require two revisions that support the same
state format. Report guest-state and timing checks separately from host speed
and interactive audio observations.

The workspace test command uses `RUST_MIN_STACK=8388608` for debug tests that
construct large machine states. This changes the test-thread stack limit, not
the emulator's guest memory. The `mise test` task and CI set it for tests too.
MCP/GDB integration tests require permission to bind local TCP sockets.

## Bounded synchronization analyzer

Use `--analyze-synchronization` for diagnostics and `--adaptive-cpu` for the
separate experimental throughput controller. Both are off by default. The latter
uses configured MHz as a ceiling and disables competing heuristic waits. Manual
`set_cpu_mhz` remains available. GDB attachment suspends analysis/control and
restores their selection after detach.

MCP `set_synchronization_analysis` accepts `enabled` and optional `adaptive`
booleans, so a scene can be compared without restarting it. MCP `status` publishes
`synchronization_analysis` as structured fields, including graph coverage,
observed steps, gaps, invalidations, candidate predicates, writer PCs/IRQ contexts,
partial writes, complete episodes, work/wait times, a work histogram and predicted
MHz. Details shows the same model. `host_audio_underruns` is separate from
`guest_audio_service_deadline_misses`. The latter counts an unacknowledged SB16
DMA block IRQ when the next block of the same continuous transfer completes.
`guest_audio_service_deadline_checks` counts completed observation windows and
`guest_audio_service_deadline_source` names that scope. Before the first complete
window, misses/source are `null`, rather than claiming zero measured misses.
Only the matching 8/16-bit IRQ acknowledgement closes its service obligation.
Pause, reset, reprogramming and single-cycle transfers do not invent deadlines.
Counters survive DSP reset within a session. They do not measure guest buffer
refill deadlines, OPL/MIDI sequencing or audible continuity; those remain unknown.
Candidate deadline misses refer to learned episode periods, not guaranteed
hardware deadlines. A new DMA IRQ acknowledgement miss restores the configured
CPU ceiling when adaptation is enabled; historical misses at enable time and
diagnostic-only mode do not retune the CPU. Host missing PCM frames never count
as a guest IRQ deadline miss.

Current recognition covers flat 32-bit code and direct backward JZ/JNZ loops
on absolute RAM comparisons, a simple register load/compare chain, or VGA
`IN AL,DX; TEST/AND AL,08h` or `TEST AL,AH` with an observed AH value of 08h.
It rejects returning guards as a substitute for a
caller's complete frame. The graph follows at most 64 instruction nodes and
three levels of direct calls. Unsupported instructions, unknown operands,
indirect transfers, unavailable code, changed generations and exhausted bounds
remain explicit incomplete coverage.

RAM observation starts with a 4,096-step discovery window every eight million
scheduling units. Only a discovered predicate with a complete bounded graph
extends that window to at most two million steps. An evidence gap ends the
extension after at most 4,096 further steps. A traced RAM work/wait episode must begin and end inside a contiguous
observed interval; no time across a gap is charged as measured work. Repeated
fallthrough/catch-up visits remain part of the work phase. Each candidate needs
eight fresh complete episodes at the current MHz. RAM budgets additionally need
repeated observed decrements by a writer inside an actually entered IRQ. A
pending IRQ alone does not establish a writer. Runtime-service and device RAM
changes outside the observed CPU bus are reported as unobserved and discard
budget eligibility. They are not attributed to an adjacent guest instruction.

VGA uses a separate boundary measurement. Port instructions already end native
RAM/register batches. The analyzer observes the admitted IN/test/branch sequence
there, retaining ordinary JIT execution between visits and the existing strict
VGA wait-batching proof. It accumulates only confirmed wait iterations; all other
elapsed guest time, including interrupts, other loops and unknown code, is
charged as a **work upper bound**. The `boundary_accounting` field distinguishes
this measurement from instruction-trace episodes. Short trace gaps do not erase
these independently observed boundaries. A returning guard still cannot qualify
as a wait, and no new general skipping proof is introduced.

Caller and IRQ instruction probes feed the same VGA boundary observer. Otherwise
a port loop completed inside one probe would disappear from the presentation
model, even though it executed normally. Time traced inside those probes remains
work; recording a boundary does not credit it as waiting. An initial taken poll
establishes the hardware phase, so later immediate exits in new phases can finish
episodes without inventing a period for the initial partial frame.

The analyzer also excludes elapsed time already skipped by the independent
strict idle engine, after its unchanged CPU/RAM/input and device-event checks.
`proven_idle_nanos` reports this contribution. An empty input query or a
cooperative delay does not supply this evidence. Proven idle time alone does
not establish a clock source, complete episode or eligible CPU budget.

The prediction uses the largest current work sample, a 25% period reserve and a
100-us margin. The prediction's maximum covers the last 16 complete episodes;
the displayed historical maximum remains separate, so an old heavy phase does
not permanently prevent relearning. Adjustment checks run between bounded batches, with a 100-ms
minimum reduction cadence and a gradual step of approximately 1/16 of current
MHz. Missing evidence or a missed learned episode deadline restores the ceiling.
After a reduction, a bounded 500-ms to 2-second relearning interval allows fresh
measurements. It includes the initial partial frame, eight complete episodes and
jitter already observed at the configured ceiling;
code invalidation, interrupted coverage or missed deadlines cancel that interval.
Device clocks continue independently. This is a heuristic throughput policy,
not strict loop skipping or instruction-count equivalence.

Focused checks:

```sh
cargo test -p doslet-runtime --locked session::analyzer
cargo test -p doslet-pc --features profiling --locked observation_tests
cargo test -p doslet-bus --locked observation
cargo test -p doslet --release --locked local_heroes_synchronization_analysis -- --ignored --nocapture
```

The last command requires the local read-only Heroes installation and ISO. It
boots to the scripted menu boundary and records three alternating diagnostic
off/on pairs with audio synthesis and no output device. Wall duration is not
host process CPU time or an audible-playback check. Live scene navigation,
matched host-CPU measurements, music continuity and generalized caller-loop
coverage remain acceptance work; no game CPU saving is established by these
synthetic tests.

An initial native menu measurement at 133 MHz used three alternating five-second
pairs. With diagnostics off, process CPU times were 4.40/4.32/4.46 seconds,
guest progress was approximately 5.00 seconds, and no host audio underruns were
added. With the initial unconditional two-million-step observation windows,
CPU times were 5.25/5.24/5.24 seconds, guest progress fell to
3.42/3.36/3.42 seconds, and underruns increased by 70,171/72,644/69,269 frames.
No complete episodes qualified. This failed acceptance and motivated the bounded
discovery windows above; it does not measure the revised implementation.

The next live menu run with bounded discovery used 4.35/4.37/4.34 CPU seconds
with analysis off and 4.45/4.40/4.37 with analysis on. All six five-second samples
advanced about five guest seconds. Underrun increments were 434/183/145 off and
63/177/421 on. Puzzle samples, with the controller enabled in the on condition,
used 4.88/4.92/4.89 CPU seconds off and 4.84/4.83/4.84 on; only the first off
sample added underruns (259 frames). Both scenes stayed at 133 MHz with zero
complete episodes. These differences do not establish savings from adaptation.
These runs preceded VGA boundary accounting. The release build used native JIT
and did not enable the Cargo `profiling` feature; instruction observation itself
was the source of the initial additional interpreter work.

### Live VGA boundary measurements

The following native JIT runs used three alternating off/on pairs per scene,
with one second to settle followed by five seconds of measurement. The on
condition enabled both analysis and the controller. All samples advanced about
five guest seconds and stayed at 133 MHz. CPU values cover the whole process;
audio values are increments in host underrun frames. These runs preceded the
additional strict-idle time accounting above.

| Scene | CPU seconds off | CPU seconds on | Underrun frames off | Underrun frames on |
| --- | --- | --- | --- | --- |
| Puzzle | 4.98 / 4.94 / 5.01 | 5.07 / 5.01 / 5.03 | 204 / 193 / 270 | 343 / 0 / 0 |
| City, no building dialog | 2.94 / 2.93 / 2.97 | 2.85 / 2.91 / 2.93 | 221 / 0 / 0 | 0 / 0 / 0 |
| Discovered map, manual scrolling | 2.74 / 3.25 / 3.27 | 3.03 / 3.03 / 3.17 | 0 / 0 / 0 | 0 / 0 / 0 |
| Battle, player's turn | 4.83 / 4.82 / 4.80 | 4.79 / 4.79 / 4.85 | 448 / 47 / 86 | 64 / 0 / 0 |

VGA wait sites were discovered at guest addresses `0x224274` and `0x22427a`;
these are observations, not recognition constants. The latter produced 34–35
complete episodes per enabled six-second interval, including settling. Recent
work upper bounds were about 162–167 ms, generally leaving too little confirmed
waiting for the configured reserve. Battle samples reported 6/5/4 learned-period
misses; these are not guest audio deadline measurements. Interactive checks reported
smooth battle music and normal cursor/unit-selection response. A prior map run
was excluded because the viewport showed unexplored black terrain. Scrolling
trajectories in the replacement run were manual and not identical.

A diagnostic puzzle cross-check at 133 → 33 → 133 MHz recorded recent episode
periods of 166.832 → 200.199 → 166.832 ms and work upper bounds of
164.86 → 184.17 → 161.64 ms. A listening check reported smooth music at 33 MHz. The work
upper bound did not scale with CPU throughput: it includes other waiting loops
that the current VGA model cannot separate. This limits its demand estimate.

These measurements establish real VGA source/episode discovery and removal of
the initial severe observation overhead. They do **not** demonstrate automatic
CPU reduction, recovery under increased work, or battery savings. Generic
caller-loop analysis for returning RAM countdown guards and continuous writer
attribution remain incomplete; the plan's final acceptance criteria are unmet.

After adding strict-idle accounting, the headless menu check passed with
off/on wall times of 100.70/100.12, 100.25/101.37 and 102.42/101.89 ms.
`proven_idle_nanos` remained zero in all three enabled intervals, so this
additional evidence source did not resolve the menu's unidentified waiting.

### Returning countdown guards and IRQ sources

The diagnostic model also admits an absolute flat-32-bit `CMP [RAM],0` followed
by a forward `JNE` to a plain `RET` within 256 bytes. It verifies a direct caller
against the actual stack return address and records observed backedges to that
caller. Two fixed guard records retain code generations and execution context.
These records do not become complete work/wait episodes merely because a guard
returns early.

After discovery, IRQ entry starts an independently bounded observation of at
most 4,096 steps. Actual bus reads and writes establish load/decrement/store
chains, including stores through indexed operands. Attribution can follow
16-/32-bit handler transitions; it ends on architectural interrupt return,
a nested IRQ, an unobserved write or the step limit. A return to a different
context ends attribution as incomplete, rather than carrying the old IRQ
identity into the continuation. Ordinary foreground batches keep their JIT path.
IRQ tracing cancels an in-progress idle proof. A foreground VGA condition can
resume only after its exact architectural return, excluding IRQ service time;
an intermediate IRET through a protected-mode bridge does not establish that
return. The suspended condition remains bounded to 20 ms and is discarded if
the exact return never arrives. VGA boundaries observed inside the IRQ probe
remain visible even if its instruction budget expires.

MCP `guards` reports caller/backedge evidence, watched RAM, actual writer PC,
IRQ vector, write value/time, partial writes and observed decrements.
`observed_interval_nanos` is the interval between captured decrements, not a
guaranteed timer period; intervening interrupts can be unobserved.
`guard_irq_steps` and `guard_interrupted_traces` expose observation cost and gaps.
No field here authorizes a CPU budget without complete caller-phase coverage.

In the real headless Heroes menu check, generic discovery found guard `206FC0h`,
operand `2810A8h`, and direct callers `2092E2h` and `217DA2h`. The second caller
had observed backward transfers. Two enabled windows captured 11 and 13
decrements at writer `219FABh` inside actual IRQ vector 8, with 63,274 and 61,438
observed IRQ steps. These addresses are evidence only and are not recognition
constants. A third enabled window had no discovered guard. This establishes
source attribution, not complete caller work/wait coverage or adaptive savings.

A live native menu check through direct MCP used three alternating off/on pairs
at 133 MHz, with one second of settling and five seconds per sample. Guest time
tracked wall time in all six samples (5.008–5.013 guest seconds). Host audio
underrun frame deltas were 0/0/0 with diagnostics off and 65/0/0 with diagnostics
on. Enabled windows, including settling, captured 174/180/183 decrements from
IRQ vector 8 and 765,535/782,853/760,069 IRQ observation steps. This confirms
live source attribution and pacing for this menu sample, not CPU headroom:
the running process was unavailable in the measurement shell's process view,
so host CPU usage was not measured. The adaptive controller remained disabled;
diagnostics were switched off after the comparison. Audio smoothness was not
independently checked by listening for this particular run.

A diagnostic experiment focused the existing strict idle proof on the discovered
caller `217DA2h`. It produced no successful proof: the returning loop accessed
devices, which violates that proof's unchanged-device contract. The extra batch
boundary and proof hint were removed because they did not establish additional
waiting. Idle diagnostics now distinguish device accesses from RAM changes or
write-log exhaustion and input-state changes. Caller-phase classification still
requires explicit treatment of these device effects; it cannot reuse an early
return as an idle classification.

### Complete caller effect inventory

After a supported returning guard's nonzero read, a separate diagnostic probe
follows execution back to the same post-compare instruction, stack pointer,
direct caller and flat context. It traces at most 32,768 steps and starts at most
once per 250 ms of guest time. IRQ entry/return is attributed explicitly; nested
IRQs, unavailable effects and service continuations prevent a complete result.
The inventory holds 512 effect groups, reserving 16 for ports/MMIO so stack
traffic cannot conceal device accesses. Overflow remains visible. Full details
are borrowed separately from the lightweight HUD statistics. Fixed-capacity
buffers are allocated at analyzer setup; tracing does not allocate.

Ordinary RAM writes include their previous value from a side-effect-free bus
view. Ports and MMIO are never read to obtain a previous value. A separate
2,048-byte ledger reconciles overlapping writes, and reports changed bytes at
return. Overflow, missing prior values, unobserved writes or changed/A20-aliased
mapping make the RAM result unavailable. CPU and input snapshots and the modeled
CMOS register state are compared separately. These are diagnostic measurements;
neither a completed iteration nor equal endpoint states supplies a frame budget
or changes the existing strict idle-skipping contract.

The read-only headless Heroes check at 33 MHz captured two complete iterations
through guard `206FC0h`, direct caller `2092E2h`, and the surrounding menu event
path. Each used 1,219 steps and 147,758 ns, with no IRQ, unobserved write or effect
overflow. CPU, input, written RAM bytes and modeled CMOS state returned to their
entry values. All four device operations were CMOS writes: select register
`0Fh` through port `70h`, write zero through `71h`, select `0Fh` again, then write
nine. This explains the earlier strict proof's device-access rejection. It does
not establish that every menu iteration is idle: timed updates and IRQ audio
work still require complete work/wait accounting. See the
[Heroes inventory](compatibility/heroes-waits.md#complete-menu-caller-observation)
for the observed sites and remaining acceptance work.

Three adjacent off/on headless interpreter pairs (four million steps per window,
with audio synthesis/draining) took 106.45/104.13, 102.51/109.89 and 103.48/111.26 ms
of host wall time. One enabled window discovered no guard. These short sequential
windows do not measure live JIT CPU headroom, audible playback or adaptive savings.

### Continuous measured recurrence

A complete caller inventory with equal CPU, input, RAM and CMOS endpoint state
can now install an exact post-compare observation boundary. Interpreter batches
yield at that instruction even inside a cached block. Native traces intersecting
the boundary use decoded execution; other native blocks remain eligible.

After four observed IRQ decrements establish a fresh source, the machine journals
up to 2,048 distinct written RAM bytes per actual interval. Hash storage is
allocated once during setup and reused without per-step allocation. Writable
native RAM borrows are unavailable while the journal is valid. The PC selects
decoded batches once per slice instead of repeatedly attempting native store
traces that would be rejected. An overflow rejects the interval and permits
native execution again; ending the journal also restores the ordinary path. IRQ entry, mapping
changes, unmodeled services and changed CPU/input state also reject or cancel an interval.
Pure queries already admitted by the strict idle proof, including a zero-motion
mouse query, use that same service classification; their CPU/RAM/input changes
remain covered by endpoint comparison.
The only admitted device operations are writes selecting CMOS register `0Fh`
and writes to that scratch register. The entire CMOS snapshot must be restored.
Other port or device-memory accesses reject the interval, even if their final
values are unchanged. Intervals time out after 5 ms; absent boundaries expire
after 250 ms.

IRQ source observation also starts when the interrupted foreground is inside a
16-bit BIOS wrapper. Actual bus addresses and the delivered vector establish
attribution; the flat caller's code/mapping checks and exact interrupt-return
boundary remain mandatory. Rejecting all such entries left artificial source
gaps at lower throughput.

Only fully executed, verified intervals contribute to `recurrence.measured_wait_nanos`.
This evidence augments continuous VGA work/wait accounting. Successive zero
arrivals at the established countdown guard also delimit complete caller episodes,
with a nonzero return starting their waiting phase. These records use the same
eight-episode qualification, recent work maximum and reserve as direct waits.
The selected caller remains stable when another caller visits the same guard.
Evidence is not extrapolated from a sample and never authorizes instruction skipping.
IRQ service, timed updates
and incomplete intervals remain in the work upper bound. The original strict
idle proof still rejects every device access. MCP and Details expose recurrence
counts, rejected/cancelled intervals and measured waiting separately from skipped
idle time. Throughput relearning allows at least nine measured periods, bounded
between 0.5 and 2 seconds, so eight new episodes can qualify. Reduction/recovery
counters and the last recovery reason are available over MCP. Real-game adaptive
acceptance remains outstanding.

When no returning-guard boundary with fresh source evidence exists, a bounded
scout can observe an executed, taken direct backedge instead. Conditional and
unconditional branches share the same interval checks; a fallthrough is not a
recurrence. A visited
guard without observed IRQ writers cannot indefinitely block this alternative;
an active backedge retains its boundary when later caller traces finish.
The backedge requires a fresh, stable VGA cadence
with at least eight complete episodes; a backedge never supplies its own deadline.
After retuning, actual VGA exits keep this existing source fresh while the first
new complete episode is measured. Source freshness does not qualify a budget:
eight complete episodes at the new speed are still required. Without another
observed exit the source expires after 250 ms, as before.
The exact same CPU/input/net-RAM/CMOS and IRQ/device-access checks cover complete
executed iterations, including indirect calls. After eight consecutive unchanged
iterations, only individually verified intervals augment the VGA waiting total.
Changing animation work and incomplete intervals remain charged in full. A
5-ms interval limit cancels the journal while retaining the boundary for the next
frame. A backedge absent for 250 ms, changed code/context or lost VGA source ends the probe.
VGA source validation runs at complete arrivals before any wait credit, instead
of scanning candidates on every intervening instruction. Code mutation checks
and both time limits remain active away from the boundary.
Eight rejected or cancelled intervals abandon an unqualified probe. A qualified
boundary survives bursts of changing animation handlers, but expires after
250 ms without any unchanged interval; all changed intervals remain work.
An eight-entry table delays retrying an abandoned head for 250 ms. Losing an
already qualified boundary invalidates
the VGA work estimates. MCP exposes `recurrence.backedge` and
`recurrence.probe_rejections` and `recurrence.last_probe_rejection_reason`
alongside the existing counters. This path executes
all guest instructions and contains no game-specific address.

For local native-host measurements, build with
`RUSTFLAGS='-C target-cpu=native' cargo build -p doslet --release --locked`.
This binary targets the build host's CPU. Compare fixed and adaptive modes within
that same build, and record the flags; do not attribute differences from a prior
generic build solely to the controller. Do not apply this host CPU flag to WASM.

Each active timed candidate learns a reference period at the configured ceiling
from a full window of 16 complete episodes before the first reduction. A partial
initial window can underestimate the normal period and freeze that estimate
when throughput falls. Cadence uses
up to 16 recent periods and excludes one longest period from its mean. That
window survives gradual speed changes, while work-cost qualification starts
again. A speed increase discards the lower-speed cadence window while retaining
the ceiling reference. At the configured ceiling, a full 16-period window must
recalibrate it before another reduction. Otherwise at least two complete VGA episodes at the new speed are
required. The rolling cadence mean tolerates 3.125% plus 100 us of slowdown;
this is stricter than the spread allowed between individual ceiling-speed
frames. MCP exposes the measured mean as `cadence_mean_nanos`, separately from
the last period and the ceiling reference, and Details displays both means.
Recalibrating an existing reference requires 16 new ceiling-speed episodes, so
slower periods cannot become a larger accepted budget.

A VGA poll does not by itself prove a fixed presentation rate. After excluding
one longest ceiling-speed period, the remaining periods must span no more than
12.5% of the shortest plus 100 us to establish a cadence reference. Otherwise
`variable_cadence` is true, that candidate has no presentation deadline or MHz
prediction, and its old cadence floor is cleared. Other independently qualified
candidates may still provide a budget; with none, execution uses the ceiling.

An unfinished phase subtracts all confirmed waiting from elapsed guest time;
work exceeding the reserved budget restores the ceiling immediately. Occasional
long guest waits do not themselves establish insufficient CPU throughput.
Sustained cadence loss also restores the ceiling. If measured work still fits,
it establishes a conservative minimum for the current model, 25% above the
failing throughput; expensive work instead uses the ordinary work-budget model.
The minimum retains the measured slowdown that justified it. It survives ordinary
retuning, but a full fresh window at the configured ceiling can disprove it:
if the failed period now fits the recalibrated reference's unchanged tolerance,
the floor is cleared. Periods measured at reduced speed cannot clear it.
Model invalidation also clears the floor.
Countdown guards use the reference to bound their work budget, but their multiple
caller/update paths are not treated as a constant presentation cadence. MCP and
Details expose `reference_period_nanos` and `cadence_floor_mhz` separately.

An IRQ between the VGA port read and its conditional branch suspends that
condition until an actual IRET returns to the exact CS/IP/SP. The entire interrupt
duration remains work, including nested handlers and BIOS services. A changed
code version, missing return or 20-ms timeout discards the incomplete episode;
it must not merge two presentation periods and create a false cadence miss.
After an actual waiting episode establishes the source, a poll that immediately
exits in a later hardware phase also closes a period. Repeated exits in the same
phase remain catch-up work. The phase identity uses the modeled next retrace
transition captured at the actual IN instruction, without reading the port again.
It remains attached to that returned value across TEST/Jcc and interrupts; a
device transition before the branch must not relabel the earlier read.

Native MCP status reports process CPU ticks on Linux for host-cost comparisons.
Use deltas over matched wall-time windows and retain guest-time, effective-MHz
and audio-underrun deltas alongside them. These counters measure the whole
process, including presentation and audio; guest idle fractions are not a
substitute. See [MCP host counters](debugging.md#mcp-host-counters).

## Check the work

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUST_MIN_STACK=8388608 cargo test --workspace --locked
cargo check -p doslet -p doslet-time -p doslet-executable --target wasm32-unknown-unknown --locked
cargo check -p doslet-web-worker -p doslet-web-audio --target wasm32-unknown-unknown --locked
cargo run -p doslet-headless --release --locked -- bench --steps 5000000
cargo run -p doslet-headless --release --locked --example bench_dynamic
```

Native synthesis-worker tests run without an audio device:

```sh
cargo test -p doslet --locked native_audio::
cargo test -p doslet --locked audio_worker
```

They compare every output sample with the synchronous renderer at 44.1 and
48 kHz, including MIDI changes and timed PCM, and exercise pause/resume,
saturation, reset/output-switch cancellation, failed setup, error recovery and
shutdown with a full paused sink. App tests verify that saturation stops guest
progress before the next CPU chunk and that asynchronous errors reach the notice
while paused. These are synthetic scheduling/correctness checks, not measurements
of game performance, audible quality, CM-32L ROM playback or macOS Audio Units.

### Linux PulseAudio protocol probes

These opt-in tests require `pactl` and a running PulseAudio-compatible server.
Create dedicated null sinks; never use the physical default sink. Save each
returned module ID and unload those modules when finished. Do not change the
system default output.

```sh
pactl load-module module-null-sink sink_name=doslet-audio-probe channels=4 channel_map=front-left,front-right,rear-left,rear-right rate=48000
pactl load-module module-null-sink sink_name=doslet-audio-probe-output channels=2 rate=48000
cargo run -p doslet-pulse --locked --example synchronization -- doslet-audio-probe 600 controls
PULSE_SINK=doslet-audio-probe-output RUST_MIN_STACK=8388608 cargo test -p doslet --locked pulse_loopback_pause_and_reset_discard_old_server_audio -- --ignored --nocapture
PULSE_SINK=doslet-audio-probe-output RUST_MIN_STACK=8388608 cargo test -p doslet --locked system_mixer_loopback -- --ignored --nocapture
PULSE_SINK=doslet-audio-probe-output RUST_MIN_STACK=8388608 cargo test -p doslet --locked mixer_selection_prepares -- --ignored --nocapture
# Replace each placeholder with the ID returned by load-module:
pactl unload-module FOUR_CHANNEL_MODULE_ID
pactl unload-module TWO_CHANNEL_MODULE_ID
```

The synchronization probe captures only the null sink monitor. Four mono streams
at 11025, 22050, 32000 and 48000 Hz carry periodic markers to separate channels
of a 48-kHz capture. `controls` corks/uncorks the group every two seconds, with a
100-ms pause. Without it, playback runs continuously. Ordinal marker offsets
must remain within 1 ms, and counts must agree. Missing markers can invalidate
ordinal pairing; a reported offset is not necessarily a measured phase drift.
The diagnostic retains up to about 463 MB for a ten-minute capture.

On 2026-10-17, PipeWire 1.6.8 (`PulseAudio (on PipeWire 1.6.8)`) passed a
10-second steady run and a 30-second grouped-control run, with endpoint offsets
within one capture frame (21 microseconds). Sequential stream control failed
the 30-second test by about 5.3 ms. The grouped 600-second run, while builds/tests
also ran, failed marker correspondence: the first/last ordinal offsets were
1/-101632, 0/-154751 and -1/47233 frames relative to channel zero (570 paired
markers). This exceeded the original strict 1-ms threshold; it does not identify
whether lost markers or phase drift caused the mismatch. That threshold is not
an acceptance requirement for the optional system mixer. The current option
uses guest-rate PCM plus 48-kHz FM and MIDI streams.

The single-stream adapter's loopback test passed pause/queue retention and reset
discard of previously buffered positive samples before negative replacement
samples. It also disconnected only its own playback client four times: three
reconnections succeeded, then the lifetime retry budget was exhausted. These
results do not establish audible game quality, physical-device latency, a
standalone PulseAudio daemon's behavior, or successful runtime CPAL fallback.
Long-run rate changes and induced underruns remain unaccepted.

The optional two-stream mixer passed a separate virtual-monitor test on the same
PipeWire server: distinguishable stereo PCM and MIDI inputs summed to the
expected levels for at least 100 ms, paused without consuming queued samples,
and resumed after reset with only the replacement samples. Device-free worker
tests retain application-mixer equivalence at 44.1/48 kHz and verify exact native
PCM sample counts, saturation of one source, and group pause/reset.
An application-level test also switched between both modes and verified that a
failed synthesizer setup retained the old mixer and paused session.
These checks do not establish long-run clock stability or audible game quality.

The native-rate loopback test sends approximately 100-ms segments at 11025,
22050 and 32000 Hz. A 48-kHz PipeWire monitor captured 4794, 4794 and 4793 stable
samples respectively, and reset interrupted a paused rate transition. The test
uses the dedicated `doslet-audio-probe-output` null sink and test-only media role
to isolate saved server mute settings. This is synthetic evidence, not a
real-game listening test.

### Display validation

Display filter and upload-order tests run with
`cargo test -p doslet --locked presentation::`. On a host with a working wgpu
adapter, also run the opt-in renderer regression:

```sh
cargo test -p doslet --locked large_hq2x_frames_remain_visible_during_gpu_upload -- --ignored --nocapture
```

It renders nine 640×480 synthetic frames enlarged by HQ2x to 1280×960,
checking image pixels before and after asynchronous texture uploads. It requires
no window or game data and does not validate live gameplay or display timing.

For the local Heroes build, this ignored test compares 40 guest seconds at
50 MHz with background JIT and with the cached interpreter. Both retain Rust GM
synthesis, use the ISO storage adapter, and run without a window/audio device:

```sh
cargo test -p doslet --release --locked local_heroes_background_jit_pacing -- --ignored --nocapture
```

It reports wall time per 100 ms of guest time, maximum emulation-thread JIT
maintenance time and final RAM hashes. Run it without concurrent builds or other
emulator instances. It does not measure audible playback or window presentation.

The benchmark reports host execution steps per second, not emulated MHz or a
game performance claim. REP iterations count as separate execution steps so a
batch stays bounded.

On macOS, `cargo test -p doslet-apple-midi --locked` renders synthetic MIDI through
the system DLSMusicDevice into memory, without opening an audio output device.
These tests need access to macOS Audio Unit services (a restricted sandbox may
hide the component). They verify nonzero PCM, timing and snapshot replay, not
audible game compatibility.

`bench_dynamic` compares five million synthetic protected-mode register and RAM steps
against the cached interpreter in three alternating pairs after warmup, verifies
registers/flags/time and exposed RAM/VRAM, and reports median execution and compilation separately.
It is not a game benchmark. For the local PRINCE JIT
comparison, use the ignored test
`local_prince_dynamic_core_matches_reference_during_execution` in place of the
decode-cache test above. Set `DOSLET_PRINCE_EXE` when the installation is elsewhere.

To count native exit reasons and addresses separately from timing, run:

```sh
cargo run -p doslet-headless --release --features profiling --locked --example profile_dynamic_exits -- /path/to/PRINCE.EXE 1000000000
```

This opt-in diagnostic validates state against the cached interpreter at each
100-million-step checkpoint and writes metadata CSV. It is not a throughput
benchmark. See [native exit measurements](compatibility/native-core.md#measured-native-exit-reasons).

After building the web bundle, run the optional real-browser check with Firefox
and Python's standard library (no Node, Selenium or package installation):

```sh
python3 tests/browser_smoke.py
python3 tests/browser_smoke.py --prince
python3 tests/browser_components.py
```

The second command needs the local `target/prince-game.zip` created by the
`local_prince_zip_round_trip` test (see [using Doslet](usage.md#browser)). The isolated headless browser mutes its audio sink;
it checks nonzero rendered samples, ZIP execution, transfer channels,
pause/resume/reset, output recovery, and continued audio/CPU progress during an
800-ms window-thread stall. Web MIDI uses a mock device, including a port without
`clear()`; MT-32 checks cover automatic upgrade, translation, RPN replay and raw
SysEx output with explicit permission; this does not establish playback on a real external synthesizer.

The component check also needs `openssl`. It exercises real IndexedDB/Web Locks,
HTTPS Range/CORS, candidate configuration, DOS save acknowledgements and
save export/import/reset. A mocked Gamepad API drives the actual browser
transport, input router and guest gameport. The HTTPS fixture generates a
self-signed certificate in a temporary directory and uses an isolated Firefox
profile; it does not install certificates in system or user trust stores.
The temporary certificate, key and profile are removed when the test ends.

Capture the guest framebuffer without opening a window:

```sh
cargo run -p doslet-headless --locked -- demo --frame /tmp/doslet-demo.ppm
```

[`mise.toml`](../mise.toml) provides the same workflows as `mise run test`,
`mise run check`, `mise run demo`, `mise run run` and `mise run web`.
Tasks that use your own game files belong in an untracked `mise.local.toml`.
`mise run run` uses the optimized release profile;
`mise run run-debug` retains the debug profile for development.

## Implementation status

| Component | Implemented | Still needed |
| --- | --- | --- |
| x86 CPU | Selected real/protected-mode 386 operations, cached GDT/LDT segments, EIP32, 16/32-bit ModR/M/SIB, same-privilege gates, bounded REP | Full instruction/privilege coverage, paging, precise faults, timing |
| x87 | All documented 387 opcode handlers, raw real80/context transfers, extended-range arithmetic, BCD, root/remainders and bounded transcendental kernels | Full exception delivery, masked stack faults, some nonfinite transcendental operands; see the [audit](compatibility/x87-audit.md) |
| i8042 / CMOS | Controller/A20, keyboard command/ACK protocol, CMOS register bank and memory-size fields | Raw keyboard/mouse protocols and IRQs, RTC clock/IRQ behavior and persistence |
| PIC | Master CPU interrupt delivery; independent slave register bank at A0h/A1h; masks, fixed priority and EOI | Cascading and remaining operating modes |
| DMA | Independent 8237 state, PC page registers, bounded byte/word RAM transfers and auto-init | Full arbitration/cascade timing, MMIO and memory-to-memory |
| PIT | Binary modes 0/2/3, latches, gates, channel-2 output at 61h, fractional guest clock input and IRQ0 | Modes 1/4/5, read-back, BCD and precise periodic load timing |
| VGA/VBE | Mode-13h and banked 640×480×8 pixels, palette writes, borrowed scanout, configuration readback, selected VBE 1.2 BIOS calls | Text/planar modes, register rendering effects, raster timing, more SVGA modes |
| DOS/BIOS | COM/MZ/overlay setup, bounded memory arena, immutable input files/directories, optional sparse RAM writes, timer/user-hook shims | Full DOS kernel, extender compatibility, raw mouse hardware, persistent storage |
| MPU-401 / MIDI | UART setup, bounded output at 330h/331h; independent procedural synth, guest-timed WAV, native and Web Audio output | General MIDI instrument fidelity, serial timing, IRQs and intelligent mode |
| Sound Blaster 16 / AdLib | Independent DSP/mixer and Nuked OPL3 FM, selected DMA PCM playback, guest-clock OPL timers, stereo FM/DMA mixing, auto-init/pause/resume, block IRQs and bounded output queue | More DSP commands, precise timing, digital-PCM mixer gain and real-game FM audio acceptance |
| Frontends | Shared iced desktop/web view, native emulation thread, browser Worker/AudioWorklet, ZIP mount/program selection, optional system MIDI on both platforms, headless CLI | Further presentation-copy reduction |
| Executable inspector | Borrowed MZ metadata/relocations and LE/LX header detection | LE/LX loading and execution |

## Component layout

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

The portable emulation and MIDI synthesis libraries retain `no_std`, except the
OPL3 adapter's upstream dependency, which currently requires `std` throughout
the PC/runtime graph. See the [future-work list](backlog.md).
Components that own buffers use `alloc`. The optional `doslet-x86-jit` native
compiler adapter uses `std` and executable memory. CPU execution uses static bus
dispatch. Scanout borrows device
memory, and RGBA conversion writes into caller-owned storage. A regression test
checks **zero heap allocations while running the demo and converting its frame
after setup**, and while executing the relocated MZ fixture with console output.
It also covers mounted-file reads, MIDI queue output, protected execution and
banked VBE pixels with full scanout conversion after setup.

The iced image adapter currently creates an owned RGBA image when the guest frame
changes. That presentation boundary has an explicit copy and allocation cost;
the emulator does not clone RAM or framebuffer storage per execution step.

## A local game target

Keep your copy of the game in `PRINCE/`, or use a symlink to its installed
directory. That path is ignored by Git. The project does not redistribute game
executables, assets, configuration or saves.

```sh
cargo run -p doslet-headless --locked -- inspect PRINCE/PRINCE.EXE
```

The supplied executable has a DOS MZ stub and an appended LE program. Together
with `DOS4GW.EXE`, this establishes that the target goes beyond the current
real-mode demo. The [compatibility report](compatibility/prince.md) records
the inspected file, evidence, unresolved requirements and acceptance stages.

## Headless runners

Run a standalone real-mode MZ program with a finite execution budget:

```sh
cargo run -p doslet-headless --locked -- run-mz program.exe --steps 200000
```

The loader validates the declared image, memory requirements, relocation targets,
entry and stack before guest writes. The CPU still supports a selected 16-bit
subset; unsupported instructions and services stop execution explicitly. Overlays
and NE/PE/LE/LX programs are rejected. The original test program in
[`tests/fixtures/relocated_mz.rs`](../tests/fixtures/relocated_mz.rs) uses a relocated
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
[recorded startup result](compatibility/prince.md#doslet-startup-attempt).

## Profiling and execution engines

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
whole iterations strictly before the next raster/PIT/DSP event. Append
`--no-vga-waits` to disable recognition and batching for reference measurements.

Other busy-wait loops are skipped once proven idle. The session single-steps one
pass from a starting point while RAM records the previous value of every byte
written. The pass is proven when the CPU returns to an identical architectural
state (including segment caches, control registers and x87 state), every
written byte holds its old value, no port or device memory was accessed, no
interrupt arrived, mouse and keyboard driver state is unchanged and only pure
query services ran: INT 16h/01h, 02h, 11h, 12h; INT 21h/0Bh and 06h with DL=FFh;
INT 33h/0003h; INT 1Ah/00h; INT 10h/03h and 0Fh. While the CPU is back at that
state and no RAM write or device access happened since, whole passes are
accounted for as steps and time up to strictly before the next hardware event.
Attempts start after empty keyboard or mouse polls, at the last proven loop
head, and otherwise at a backoff from 20 thousand to 2 million steps. Append
`--no-idle-loops` to execute these loops for reference measurements.
On the local CIV intro (VGA, Roland, keyboard only) 897 million of 990 million
steps were skipped and host CPU time fell from 55.9 s to 5.1 s, with identical
final RAM and registers. [The driver report](compatibility/prince-svga.md)
records the supported scope and differential checks.

For diagnostic inventories of game polling, the optional `inspect_civ` wait
tracer and Python/objdump scanner correlate backward branches with executed I/O.
See the [Heroes wait inventory](compatibility/heroes-waits.md) for commands,
addresses and the distinction between confirmed waits and ordinary repeated work.
The native `--experimental-heroes-timer-waits` flag optionally batches three
version-specific Heroes countdown waits. It is disabled by default; the terminal
reports the first actual batch. These sites were inactive in the sampled main
menu, so the flag does not promise a reduction in menu CPU usage.

Native x86-64 and AArch64 builds of `doslet` and `doslet-headless` enable the
`dynamic-core` Cargo feature by default. Cranelift compiles hot blocks of up to
16 register-only dword instructions in protected 32-bit code. A terminal relative
Jcc/JMP back to the block's start can repeat inside one native call, keeping
registers and flags local until the loop exits or its step budget runs out.
RAM traces can also follow relative branches and internal exits, with at most
16 decoded instructions in a 240-byte span. They support LEA, dword memory-left
CMP/TEST, byte MOV/CMP/TEST (including AH), PUSH/POP32, near direct/indirect CALL,
RET/RET imm16 and dword XCHG with memory. CALL/RET can remain inside the same
prepared graph; an external target returns to native dispatch. Data segments must be flat and readable;
stack operations additionally require a full flat SS with a 32-bit stack, and
CALL/RET require full flat CS. The PC exposes RAM from 500h with A20 enabled,
excluding the VGA aperture and IVT/BDA. Every complete access is bounds checked.
Byte/dword MOV stores continue natively in ordinary RAM with writable flat segments.
Stores update touched page generations and exit before modifying an active code
page. Segment safety is cached until a used selector or descriptor changes.
Other stores, I/O, REP, real-mode code and unsupported forms use the interpreter.
The scheduler combines native blocks and validated ordinary-RAM
interpreter work in slices of up to 256 steps, checking IRQs and updating clocks
and counters once per slice. Device access, mode/IF changes and hardware deadlines
end a slice. Code bytes/generations and CS bounds remain checked. WASM keeps its
interpreter and does not depend on Cranelift. See the
[measurement report](compatibility/native-core.md) for workload-specific
results, warmup comparisons and correctness evidence.

Use `--no-dynamic-core` in either native frontend to select the interpreter with
its decode cache and ordinary scheduling, or build with `--no-default-features`
to omit the native JIT.
Headless `--no-decode-cache` disables both engines' caches for the byte-fetching
reference. Native CLI output reports actual compiled and executed block counts.

Optionally reuse observed code on subsequent launches:

```sh
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE --core-profile target/prince.core-profile
```

The same `--core-profile FILE` option works with headless runners. Profiles are
local hints containing bounded guest code bytes, never saved host machine code.
Keep them outside game directories and do not redistribute profiles of commercial
games. The desktop saves on pause, reset and normal shutdown; headless saves at
the end of a run, including budget exhaustion. Every profile block is decoded
again and checked against current RAM before execution. Relocated or changed
bytes fall back to interpretation. Automatic LE object/relocation analysis and
object-relative profile identities remain future work.

Compilation happens in explicit `Session::prepare_dynamic` maintenance calls
between execution batches, with at most two blocks per ordinary frontend call.
It may allocate and adds host latency; `Session::advance` remains allocation-free.
The code cache holds at most 4096 resident functions in 16 groups. When full,
preparation evicts the least recently used group and continues compiling;
hot groups remain usable. Genuine compiler failures stop further compilation
once, preserving interpreter fallback. See
[the native core contract](architecture.md#native-dynamic-core) for boundaries.

Selected interpreter instructions in default-32-bit code use a fixed single-instruction
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

Decode-cache profiling distinguishes CS context transitions and discarded entries
from page-write, mapping, bus-owner and access invalidations. These counters count
rejected entries, not writes to guest code. Collisions count lookups at occupied
slots with another key; evictions count successful replacements. Decode attempts
and failed decodes expose repeated unsupported-form probing. `fallback_hits` is
reserved for experiments and remains zero in the current implementation.
