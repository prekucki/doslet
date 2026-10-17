# Synchronization analysis and adaptive CPU budget: implementation handoff

Status: bounded implementation and live validation in progress, 2026-10-17. The original
acceptance criteria below remain the target. See
[implemented scope](../development.md#bounded-synchronization-analyzer) for the
admitted direct-loop subset, explicit attribution gaps and outstanding live
measurements. Do not interpret the synthetic controller test as Heroes acceptance.

Current delivery audit:

- Bounded static/runtime observations, source attribution, recurring-caller
  accounting, structured diagnostics and the opt-in controller are implemented
  for the documented subset. Diagnostic mode does not enable the controller.
- Differential, allocation, controller, workspace, lint and WASM checks pass.
  Headless Heroes demonstrates reduction and recovery with interpreter/JIT;
  it does not establish live audio or gameplay acceptance.
- Earlier three-pair menu measurements were inconsistent. After read-phase and
  variable-cadence fixes, two live pairs saved 33.2% and 50.2% CPU, but listening checks
  reported audible gaps/clicks during the comparison. The third adaptive sample
  was interrupted and fixed 133 MHz restored. Host load was suggested as a
  possible cause; attribution and audible acceptance remain unresolved despite
  zero additional host PCM underruns and guest DMA IRQ ACK misses in the sampled
  windows. See the recorded numbers in
  [Heroes wait analysis](heroes-waits.md#continuous-recurring-caller-measurement).
- A repeated third menu pair saved 36.7%, with smooth audio reported in a
  listening check. Puzzle pairs saved 13.0–31.8%; city pairs stayed at 133 MHz and
  added 1.7–2.0% CPU cost. The city trace exposed repeated indirect event dispatch
  outside the returning-guard model. A subsequent bounded backedge-recurrence
  extension passes synthetic state/effect checks. Its first live native-CPU
  build measured roughly 17 ms of work and 150 ms of waiting in city episodes,
  but retuning repeatedly lost fresh evidence. Regression tests reproduce and
  cover both the source-freshness gap and an insufficient relearning interval.
  Longer tests rejected an approximately 11% cadence slowdown at 19 MHz and
  tightened the mean slowdown limit to 3.125% plus 100 us. They also exposed
  VGA polls hidden inside an IRQ observation scope after a redirected IRET.
  Shared port observation and bounded IRQ attribution fix those regressions.
  The corrected 240-second headless TUTORIAL-city run held 78 MHz throughout
  its last minute, with zero misses in 5,170 DMA IRQ service windows and final
  cadence means within the ceiling reference tolerance. Its first live city
  comparison saved about 4–5% in two pairs but added about 16% in the third,
  so repeatable savings were not established. A further IRET-bridge fix keeps
  suspended VGA conditions until the exact foreground return; both port polls
  now complete 353 episodes in the final headless minute, versus 237/353 before.
  Moving source validation to completed recurrence arrivals reduced measured
  diagnostic overhead from 18.1% to 12.7%. Subsequent adaptive city pairs still
  varied from 6.1–10.6% additional CPU cost at 125 MHz to 46.8% savings at
  41 MHz. A regression reproduced a premature initial cadence reference;
  calibration now requires the full 16-period ceiling-speed window. Subsequent
  menu checks exposed an unverified guard monopolizing discovery and a missing
  conditional-backedge path. The scout now allows a guard without fresh source
  evidence to yield to a VGA-backed probe, including actually taken direct
  conditional branches. Differential regression tests cover the conditional
  recurrence and its normal exit. The next native build saved 12.0–24.5% CPU
  in menu pairs and 8.5–12.1% in city pairs, but puzzle pairs regressed, with
  guest time falling behind wall time and missing host PCM frames. A cadence
  floor survived even when fresh ceiling evidence undermined its original
  failure. Regression-tested revalidation now requires a full ceiling-speed
  window after recovery and clears only floors disproved by that independent
  reference. Current-build live validation and audible playback remain pending.
- Portable Protobuf fixtures now support repeated native headless comparisons
  from identical guest states, without `protoc` during builds. Three alternating
  pairs per scene measured small menu/city wall-time gains, but puzzle and
  map-edge input regressed by 8.4% and 5.4%. All trials retained guest PCM and
  had no DMA deadline misses. See the [recorded fixture comparison](heroes-waits.md#portable-state-headless-comparison-2026-10-17).
  Battle capture and current-version live playback validation remain outstanding.
  Earlier scene observations do not validate a
  changed controller. Guest audio diagnostics now cover SB16 DMA block IRQ ACKs
  before the next block completes, with separate observation/miss counts and
  prompt controller recovery. Other music-service deadlines remain unknown;
  host PCM underruns and learned episode misses are distinct counters.

## Task for a fresh session

Implement a bounded hybrid static/runtime analyzer that discovers how guest
loops synchronize with clocks and measures their work and waiting phases.
Then add an opt-in controller that reduces CPU throughput when measured work
consistently finishes before the guest's own deadline. Start with Heroes, but
do not encode Heroes addresses or recognize screens by mouse position.

Read AGENTS.md, docs/SPEC.md, docs/architecture.md, docs/development.md and
docs/debugging.md first. Inspect the worktree before editing: existing debugger,
MCP and idle work may be uncommitted. Preserve unrelated changes. Do not commit
or push unless requested. No Node tooling. Game files remain read-only; use RAM
write overlays. Do not patch EXEs as part of this task.

## Objective and evidence boundary

Reduce host CPU use while preserving music, animation and scrolling. Discover
spare CPU capacity from guest execution rather than classifying screens.
A fixed 8 MHz target is not a requirement; it is an example of potentially
sufficient throughput for some workloads.

The current city result in docs/development.md is about 41% cooperative guest
idle time. That is neither host CPU savings nor proof of how much spare capacity
exists. Earlier city captures used different screens and are not controlled A/B
benchmarks. Do not promise a specific saving before measuring it.

## Existing implementation to reuse

- `crates/runtime/doslet-runtime/src/session/observation.rs`: bounded sampling.
- `session/synchronization.rs`: 512-step interpreter trace bursts every two
  million scheduling units; RAM/port conditions and branch observations. Changes
  at a step boundary do not establish the writer or interrupt ownership.
- `session/cooperative.rs`: cooperative waits and `PollBudget`. Empty INT
  33h/000Bh motion queries measure inter-query work excluding previous waits.
  The budget targets 250 us minus observed work, additionally bounded by device
  service horizons. This is a cadence heuristic, not a demand estimator.
- `session/idle.rs`: strict proofs; keep these separate from heuristics.
- `crates/machines/doslet-pc/src/timing.rs`: device timing integration.
- `apps/doslet/src/audio.rs`: host PCM refill watermarks, bounded guest lead.
- `apps/doslet/src/adaptive_meter.rs`, `apps/doslet/src/app.rs`: live diagnostics
  and settings. Locate MCP status serialization and existing tests before edits.

The existing learned countdown guards have two candidate slots and revalidate
code/context at batch arrivals. Observation also works with JIT via bounded
interpreter bursts. Neither the current countdown predicate nor empty input
proves that the caller has no useful work. Existing optional Heroes recipes
must remain independently selectable; avoid stacking competing controllers.

## Stage 1: model and bounded static analysis

Analyze loaded guest code, not just EXE bytes: relocation, packing and code
mutation matter. Reuse existing decoder information rather than inventing
another instruction semantics implementation.

Define fixed-capacity records for code context/version, basic blocks, edges,
loop candidates, predicates, dependency chains and explicit unknown effects.
Start with direct branches, backward edges, supported compare/test conditions,
simple register dependencies and absolute RAM operands. Follow direct callees
only within bounded depth/size. Unknown indirect targets, unsupported operands
and exhausted capacity must report incomplete coverage, not harmlessness.

A loop record must distinguish the waiting branch, useful-work branch and
exits. A guard that immediately returns may belong to a larger caller loop;
do not treat its tiny recurrence cost as the whole frame's work.

Use code-page generations and execution context to invalidate records after
self-modification, mapping/segment changes, executable replacement or reset.
Never decode across unavailable memory through side-effecting device reads.
Start with explicitly supported flat 32-bit code; reject unsupported contexts
clearly, with 16-bit/address-wrap support a later extension rather than a guess.

## Stage 2: runtime attribution and complete episodes

Add bounded opt-in observations of actual writes to watched predicates, with
writer PC, affected byte range, guest time and interrupt context. Capture guest
instructions, device/DMA changes and runtime service writes distinctly. A
pending IRQ is not evidence that its handler wrote a counter. Account for
overlapping and partial writes; record ambiguity if a source is unobserved.

Attribute reads only from values returned to the guest; never reread MMIO or
ports to probe them. Link known port predicates to modeled device transitions.
For RAM counters, require observed writers and repeated transitions before
labeling them clock-derived. Distinguish counter reset by foreground code from
decrement in an IRQ handler. Learned periodicity is an estimate, not a hardware
deadline guarantee.

Trace complete bounded work/wait episodes, using entry/exit observation points
between trace bursts where possible. Never charge untraced gaps as known work.
Track calls, memory/device effects, IRQ entry/return, frame-related activity and
input servicing within each phase. Loops with counters that change during
waiting are not strict unchanged-state proofs.

Keep at least these states explicit: candidate, structurally understood,
observed source, measured complete episodes, incomplete/invalidated. A confidence
score must not replace a missing safety condition.

## Stage 3: diagnostic budget, with no execution changes

For each candidate report completed episode count, observed period/deadline,
work time, waiting time, work maximum and distribution, missed deadlines,
source attribution, unknown effects, age and current MHz. Measure guest work
excluding cooperative waits; keep host execution time in a separate metric.

Publish structured MCP fields and readable Details diagnostics, including the
reason a budget is unavailable. Do not export only an opaque Debug string.
Report a predicted lower CPU budget with a reserve, but do not act on it yet.
Do not infer a condition deadline from the nearest unrelated PIT/DSP event.
Cross-check predictions by manually changing MHz on the same scene and observing
complete episodes. Sparse samples alone must not yield a confident work budget.

## Stage 4: opt-in adaptive controller

Use configured MHz as a user ceiling and retain manual fixed speed. Maintain a
separate effective MHz, with minimum, hysteresis and bounded adjustment cadence.
Reduce gradually only after repeated complete episodes show spare capacity.
Increase promptly on measured deadline misses, growing work or model invalidation;
fall back to the user ceiling when required evidence is absent. Relearn timing
after changes without oscillating between reset and immediate reduction.

An initial work/period estimate can suggest speed, but validate after every
adjustment: throughput need not scale linearly. Preserve safety reserve for work
variance and IRQ service. No model based only on unchanged display, cursor
position, empty input or a full audio buffer may authorize reduction.

Keep device time independent of CPU budget. Respect IRQ delivery and bounded
execution. Host PCM low-watermark refill remains active before the queue empties;
separate host scheduling starvation from guest music-service starvation. Report
audio underruns and guest service deadline misses independently.

Do not add generalized loop skipping in this stage. Lowering effective speed is
heuristic and may change guest execution counts/state. Any future exact skipping
requires the existing strict-proof contract, not this model's confidence alone.

## Cost and architecture requirements

Preserve no_std libraries, static bus dispatch, machine ownership and bounded
batches. Allocate fixed analysis storage only during setup; no per-step or
per-event allocations. Put generic instruction/context observations in CPU/bus
contracts and PC/IRQ attribution in the machine/runtime layer.

Select observation-enabled versus ordinary execution outside hot loops where
practical, using static specialization. Verify the disabled path does not gain
unconditional per-instruction callbacks. Measure both enabled overhead and the
disabled regression on interpreter and JIT; retain WASM compilation.

## Validation and acceptance

Deliver stages separately in reviewable changes, updating implemented scope in
SPEC/architecture/development as functionality lands. Diagnostic stages must
not enable the controller implicitly. Run focused tests while developing and
the repository validation suite at each coherent stage:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p doslet-pc --features profiling --locked
cargo test -p doslet-runtime --test allocations --locked
cargo check -p doslet-web-worker --target wasm32-unknown-unknown --locked
```

Synthetic tests must cover IRQ-written counters, foreground-only counters,
VGA polling, busy work with no waiting, frame skipping/catch-up, nested calls,
unknown side effects, partial writes, code mutation, changed contexts, stale
evidence, capacity exhaustion and audio deadlines. Verify supported 16/32-bit
boundaries or explicit rejection. A trace interrupted by its budget must not
be reported as a complete episode.

With controller and other heuristic waits disabled, compare analyzer on/off
against reference execution at equal scheduling boundaries: registers, flags,
IP, RAM, device state, emitted events and step accounting. Cover JIT and
interpreter. Allocation tests must actually exercise observation and adaptation,
not only default-disabled paths.

For adaptive runs, do not demand identical instruction counts or RAM at equal
step counts across different CPU speeds. Instead test device-time invariants,
deadline behavior and responsiveness, and record real-game evidence separately.

Use Heroes menu, puzzle, city without a building dialog, world-map scrolling,
and battle. First verify the current MCP/GDB endpoints and visible instance;
use the ports configured for the current process. Do not
assume MCP mouse navigation works; it previously disagreed with the game's
relative cursor. Let the user navigate if necessary. GDB breakpoints invalidate
host timing samples; detach for benchmarks.

Record baseline versus enabled host CPU time in at least three alternating
pairs at comparable scenes. Report effective MHz, complete-episode coverage,
work/wait distributions, deadline misses, input response and audio underruns.
Keep guest idle percentage distinct from actual host CPU savings. Verify music
and gameplay live; nonzero PCM alone does not establish audible continuity.

Acceptance: explain at least one real Heroes synchronization path with observed
writer/source and complete work/wait episodes; demonstrate automatic reduction
and recovery under increased work without game-specific addresses; publish
measured savings and limitations. If a scene cannot be modeled, show why and
execute normally rather than manufacturing an idle classification.
