# x87 interpreter performance

Next steps toward the Quake FPS target are recorded in the
[Quake optimization plan](quake-optimization-plan.md).

This report covers batching of common 387 forms in interpreter slices and the
exact binary64 fast paths of `doslet-x87`. Both keep the results of the
reference handler bit for bit. Measurements were taken on Linux x86-64
(Intel Core i9-14900HX, Rust 1.99.0, ordinary Cargo release builds) on
2026-10-21. They are host-specific and do not describe browser throughput.

## What was slow

Before this change, no x87 instruction entered the decoded instruction cache or
a batch block. Every one ended the current slice, so the machine checked IRQs,
advanced clocks and counters once more, failed the next block lookup and ran the
byte-fetching reference step. In the real-mode loop microbenchmark below an x87
instruction cost about 80–130 ns, against about 8 ns for a cached integer one;
FXCH and FNOP cost almost as much as arithmetic. Inside `doslet-x87`, the
reference operations themselves cost 25–30 ns for basic arithmetic, about 20 ns
for FLD real32 (three format conversions, two of them through the general
rounding routine), about 10 ns for FILD and FST real32, and about 100 ns for
FSQRT (64-iteration integer root).

In the measured Quake demo window, 23.0 million basic arithmetic instructions
ran under PC=24 and 0.46 million under PC=64 per 400 million steps (5.9% and
0.1% of all instructions); none ran under PC=53. All PC=24 arithmetic stayed
in the binary64 fast path. These counts came from temporary counters that are
not part of the code.

## Design

Block decoding accepts the forms listed in the
[specification](../SPEC.md#cpu-execution-contract); other decoder users still
reject x87 bytes, and single-instruction cache entries keep them on the
reference step. Each form kind has a prepared handler specialized for its
address size. The handler first checks every condition under which the
reference handler could fail or report an error: CR0.EM/TS, operand and push
tags, batch memory for the operand, and for numeric forms that all exceptions
are masked and PC is not the reserved 01. It then performs the same
`doslet-x87` calls, the same memory access widths and order, and the same
instruction/operand pointer updates (linear in real mode, offset in protected
mode). If a check fails, nothing changes, the slice ends and the reference step
executes the instruction. Real32 stores keep the reference's representability
check and restore the status word when they decline.

### Exactness

The fast arithmetic covers PC=24 and PC=53, all four RC modes and masked
exceptions. Both operands must be zero or normal with at most 53 significant
bits and an unbiased exponent within ±448: real32/64 memory operands, integers
below 2^53 and registers whose retained significand fits. Such operands are
exact binary64 values. For these the host computes s = RN53(x) of the exact
result x:

- sums: TwoSum gives x − s exactly;
- products: Veltkamp splitting and Dekker's product (no FMA) give x − s exactly;
- quotients: a Dekker product of q·b and the Sterbenz-exact a − p give the sign
  of the residual a − q·b, hence of x − q.

The exponent limit keeps every operand, split, product, quotient and error
term normal, as the error-free transformations require, and keeps results far
from overflow, underflow and denormals. Because |x − s| ≤ ulp(s)/2, the interval
between s and x contains no P-bit grid point and no P-bit midpoint for P ≤ 53,
except a 53-bit midpoint at its outer end, where hardware RN has already
chosen the even neighbour. The rounding of x therefore equals the reference
rounding of the 64-bit significand of s, decreased by one unit when |x| < |s|,
with a sticky bit; that reproduces C1 (rounded up) and PE. Under PC=24 the
error sign is computed only when s is itself a 24-bit grid point or midpoint.
Exact cancellation and zero operands follow the reference's signed-zero rules.
Operands with 64-bit significands (FLD real80, large FILD, PC=64 results or
constants), NaNs, infinities, denormals, unmasked exceptions, PC=64 and every
other operation use the reference methods.

Fast loads, integer loads, ST(i) loads and real32/64 stores write the same
diagnostic binary64 value, retained finite data, raw encoding, tag and status
as the reference, including retained exponents of zeros, which differ by load
path. The fast paths are enabled only on x86-64, AArch64, WASM and x86 with
SSE2, where binary64 operations round once. FSQRT keeps the reference integer
root: stable `no_std` Rust has no binary64 square root.

## Evidence

- `doslet-x87` compares each fast method with its reference method on cloned
  states, checking returned values and complete state bit for bit:
  192,000 randomized comparisons in the default test run, 163,840,000 in the
  ignored extended run, and 504,800 directed cases (ties and values one
  64-bit unit around them, binade starts, signed zeros, exact cancellation,
  the exponent limit, 64-bit significands under PC=24/53, store-format
  extremes) over 80 control words: every PC and RC, masked or partly unmasked.
  Mutations of the rounding, error terms, signs and tags are detected.
- `doslet-pc` runs 600 random programs (180,683 instructions; 288 end in a
  reference error such as a stack fault, an unmasked exception or PC=01) in
  flat protected and real mode, mixing batched and reference-only forms and
  integer instructions. Batched execution with and without fast paths must match
  the reference handler in registers, flags, complete FPU state, RAM and the
  final error; a failing instruction must be declined by the batch.
- A Quake v1.06 shareware demo replay from a saved state at 16 guest seconds
  compares the reference handler with batching plus fast paths over 400
  million steps: full portable states and drained PCM/MIDI are equal at all
  eight checkpoints.
- The DOS/4GW regressions `dos4gw_games_resume_from_states_under_dpmi_direct`,
  `local_prince_menu_pcm_and_keyboard` and
  `heroes_direct_follows_its_layout_in_the_app` pass.

```sh
cargo test -p doslet-x87 -p doslet-x86 -p doslet-pc --locked
cargo test -p doslet-x87 --release --locked fast_paths_match_extended -- --ignored
cargo test -p doslet --release --locked --bin doslet quake_x87_replay -- --ignored --nocapture
```

The Quake tests need `games/Quake.toml` and its ZIP. They boot the demo once,
keep its state in `target/quake-x87-demo.state` (override with
`QUAKE_DEMO_STATE`) and write nothing under `games`.

## Measurements

### Quake demo

Quake v1.06 shareware under `--dpmi direct` and `--no-aot`, interpreter only
(the desktop app has no dynamic core by default), 266 MHz profile. The replay
starts from the demo state at 16 guest seconds and runs exactly 400 million
steps (6.02 guest seconds); PCM and MIDI are drained, not synthesized. Each
variant runs twice in rotating order and the faster run counts.

| Build and variant | Host seconds | Guest s per host s | ns per step |
| --- | ---: | ---: | ---: |
| Base commit `4b39342` | 10.93 | 0.550 | 27.3 |
| This change, reference handler (`set_x87_batching(false)`) | 10.68 | 0.563 | 26.7 |
| This change, batched with reference arithmetic | 5.37 | 1.121 | 13.4 |
| This change, batched with fast paths (default) | 4.79 | 1.256 | 12.0 |

```sh
cargo test -p doslet --release --locked --bin doslet quake_x87_throughput -- --ignored --nocapture
```

`QUAKE_BENCH_STEPS` changes the replay length. The base commit was measured
with the same test before the variants existed. Dedicated prepared handlers
(instead of the cold decoded-instruction array) contributed about 0.3 ns/step
of the batched results; the fast paths about 1.4 ns/step.

### Real-mode loops

`x87_loop_microbenchmark` runs COM loops whose data lies on its own 4-KiB page,
with idle-loop proofs disabled, for 20 million steps after a warmup; each
iteration ends with a short JMP, which counts as a step. Values are ns per step
under CW 0C7Fh (PC=24, chop); PC=53 and PC=64 rows are printed too.

| Loop | Base `4b39342` | Reference handler | Batched | Batched, fast paths |
| --- | ---: | ---: | ---: | ---: |
| MOV/IMUL/ADD/MOV (integer) | 9.6 | 8.4 | 8.4 | 8.4 |
| FLD/FMUL/FADD/FSTP real32 | 105.6 | 94.7 | 30.2 | 17.0 |
| FLD/FDIV real32, FSQRT, FSTP | 111.9 | 104.2 | 43.7 | 34.7 |
| FLD/FLD/FXCH/FISTP/FSTP | 90.9 | 82.3 | 21.2 | 14.3 |
| 4 × FNOP | — | 47.1 | 5.7 | 5.7 |
| FLD real32/FSTP ST(0) twice | — | 69.5 | 17.0 | 11.4 |

Under PC=64 (037Fh) the FLD/FMUL/FADD/FSTP loop takes 26.2 ns/step with fast
paths: its arithmetic uses the reference methods, while loads and stores still
take their fast paths.

```sh
cargo test -p doslet-runtime --release --locked --test x87 x87_loop -- --ignored --nocapture
cargo test -p doslet-x87 --release --locked --test timing -- --ignored --nocapture
```

The second command times individual `doslet-x87` operations without CPU
dispatch. Under PC=24, a fast FLD real32 plus FSTP ST(0) takes 4.7 ns
(reference 23.7 ns), FADD plus FSUB real32 13.3–14.0 ns (47.0 ns) and FMUL plus
FDIV real32 18–23 ns (58.5 ns). Timings vary by a few percent between runs on
this shared host.

## Not covered

- FSQRT, FPREM, FSCALE, FXTRACT, transcendentals, FBLD/FBSTP and environment
  instructions are not accelerated; batched FSQRT uses the reference root.
- Compares, FIST/FISTP, FXCH, FST(P) ST(i) and constants are batched but call
  the reference methods.
- Unmasked exceptions, PC=01, stack faults and WAIT always run on the reference
  step, as do x87 instructions outside batch slices (single steps after IRQ
  checks or at budget ends).
- PC=64 arithmetic is exact but not accelerated beyond batching.

## Approximate PC=64 mode: design note, not implemented

An opt-in mode could execute PC=64 basic arithmetic as binary64 (rounding
64-bit operands to 53 bits first, then using the PC=53 fast path), while FIST
and memory stores keep honouring RC. It would be deterministic within the mode
but not bit-exact: values, C1 and PE would differ from the 80-bit result. It
would need a `doslet-config` hardware field recorded in saved states, so a
restored state continues in the same mode, plus profile, CLI and configurator
settings. A temporary experiment treating PC=64 as PC=53 in the fast path left
the Quake replay above unchanged (12.3 ns/step either way): only 0.1% of its
instructions are PC=64 arithmetic. The mode is therefore not implemented.

## Comparing further interpreter changes

The x87 batching and exact binary64 implementation above is the baseline for
further optimization. It is also the implementation in
[PR #2](https://github.com/prekucki/doslet/pull/2), whose original commit
`2ab1102` is already in the local history used for these experiments (parent
`39f33f7`). The PR was reviewed at `7a26d53`, which does not contain selective
interpreter code guards or REP instruction reuse.

Selective code guards skip the in-block code check after prepared instructions
that cannot access the mutable bus. Memory reads, writes, stack operations and
fallback handlers retain their checks. The classification occupies an unused
bit in the existing 16-byte prepared instruction; block capacity remains eight
instructions. `Session::set_selective_code_guards(false)` restores checking
after every instruction for comparisons. The optimization is enabled by default
on native targets; WASM retains the previous default pending browser measurements.
The switch is execution policy and is not part of portable guest state.

The paired test restores the same demo state for every sample, runs 10 million
untimed warmup steps, compares the warmed portable state, and then times
400 million steps. Six pairs alternate execution order and report medians,
ranges and paired ratios. Both sides disable the dynamic core and keep exact
x87 batching and fast paths enabled. The baseline disables selective guards.
Compilation, state loading and serialization, audio synthesis, presentation
and the GUI are outside timing; PC devices and PCM/MIDI draining are inside.

```sh
cargo test -p doslet --release --locked --bin doslet quake_x87_paired_throughput -- --ignored --nocapture --test-threads 1
cargo test -p doslet --release --locked --bin doslet quake_x87_code_guards_replay -- --ignored --nocapture --test-threads 1
```

`QUAKE_BENCH_STEPS`, `QUAKE_BENCH_WARMUP_STEPS` and `QUAKE_BENCH_ROUNDS`
override measured work, warmup and pair count. Use an even pair count to balance
order, and run without concurrent builds or other emulator measurements.
`QUAKE_COMPARE_STEPS` controls the separate correctness replay, which compares
complete portable state and cumulative PCM/MIDI hashes immediately after warmup
and at eight further checkpoints.

### Final native measurement

On 2026-10-21, the final implementation with the REP experiment removed gave:

| Policy, 400M steps / 6.015037 guest seconds | Median host seconds | Range, seconds | ns/step |
| --- | ---: | ---: | ---: |
| Check after every instruction | 4.923791 | 4.842528–4.960476 | 12.309 |
| Selective code guards | 4.798093 | 4.738159–4.850470 | 11.995 |

The ratio of medians is 1.02620 (2.6% higher throughput); the median of paired
ratios is 1.02080. All six pairs favored selective guards. This is a modest
native replay improvement on a shared host, not a measured desktop frame-rate
or browser improvement. The source state SHA-256 is
`43e684bbba7928cc5e889ed450afd66c0000d09a3dcf91a696ce680a4ac0e890`.

The final 410M-step replay (10M warmup plus 400M compared work) matched complete
portable states and cumulative PCM/MIDI at all nine checkpoints. The x86/PC
suites with profiling passed 177 tests, including random x87 programs against
the reference handler, read/write code invalidation, VGA effects, and the
self-modified x87 displacement regression from PR #2. All 16 runtime allocation
tests passed. These checks do not establish interactive gameplay or audible
output quality.

`cargo check -p doslet-x86 --locked --target wasm32-unknown-unknown` passed;
browser performance was not measured. The wider
`cargo clippy -p doslet-x86 -p doslet-pc -p doslet-runtime --all-targets --locked -- -D warnings`
also passed after importing PR #2's existing `too_many_arguments` annotation
for `doslet-x86-wasm/src/static_program/procedure_memory_finite_emit.rs:region_body`.
The focused x86/PC Clippy run with `--all-targets --features profiling` and
`-D warnings` passed, as did `cargo fmt --all --check` and the native release
application build.

### Rejected REP experiment

A separate REP reuse experiment retained the decoded instruction between
elements while preserving per-element bus access and work accounting. Its first
six-pair run changed the median from 4.984442 to 4.978303 seconds, too little to
establish a benefit on this shared host. Combined with selective guards it
regressed; after moving REP classification off ordinary instructions, the
combined median changed from 5.059086 to 5.030594 seconds, still less than the
selective-guard-only experiment. The REP implementation and its switches were
removed. REP execution keeps its previous behavior.

### Comparing with the desktop

An open GDB listener keeps ordinary batching enabled. An attached client changes
execution: even Continue advances one step at a time, so the x87 batch path is
not exercised. Detach before measuring ordinary interpreter throughput. A replay
starting from a saved demo state is also a different workload from the first
40 guest seconds of application startup; those timings cannot attribute a cost
to presentation or audio without a matching guest window.

## Measuring Quake frames

`quake_timedemo_fps` runs the local shareware `demo1` at 320×200 with digital
audio enabled. It starts Quake with `-condebug -nojoy -nocdaudio`, requests a
timedemo, and reads the result from the private in-memory `ID1/QCONSOLE.LOG`
overlay. It does not update the configured persistent game store.

```sh
cargo test -p doslet --release --locked --bin doslet quake_timedemo_fps -- --ignored --nocapture --test-threads 1
```

The test boots once and restores the same state for three complete timedemo
runs. `QUAKE_TIMEDEMO_ROUNDS` and `QUAKE_TIMEDEMO_MHZ` override the repeat count
and nominal CPU rate. It reports Quake's guest-clock FPS separately from
unpaced host frames per second. Host timing includes emulation, devices,
PCM/MIDI draining and console-log inspection, but excludes audio synthesis,
the window and GPU presentation. It therefore measures a throughput ceiling
for this demo, not displayed desktop FPS.

The start marker uses `+timedemo demo1 +wait +echo DOSLET_TIMEDEMO_START`.
The ordering follows id Software's [command buffer](https://github.com/id-Software/Quake/blob/master/WinQuake/cmd.c),
[host frame](https://github.com/id-Software/Quake/blob/master/WinQuake/host.c) and
[timedemo implementation](https://github.com/id-Software/Quake/blob/master/WinQuake/cl_demo.c):
the echo runs during the second frame, before the demo message reader starts
the game timer. Polling observes each boundary within 10,000 guest steps;
remaining marker work and result logging are included in the host window.
This approximation is small at 266 MHz but becomes less suitable at very low
nominal rates.

### Baseline before further address-path experiments

On 2026-10-21, with selective code guards enabled and the dynamic core disabled,
three runs reported 969 frames in 38.7 guest seconds (25.1 guest FPS).
Host times were 32.243168, 32.348909 and 32.197626 seconds: median **30.053 host
frames/s**, range 29.955–30.095. Each sampled window contained 2,570,620,000
scheduling steps and 38.655939 guest seconds. The starting state SHA-256 was
`83d388640132cc584368e39338eef1b861a9c1c91b77f9117a5add54edcb07b8`.
Reaching 60 host frames/s on this workload requires approximately twice this
throughput; raising the nominal guest MHz alone does not provide that gain.

A separate `perf record -e cycles:u -F 999` run of two 400M-step replay pairs
collected 20,329 samples without lost samples. The dominant core event placed
19.55% in `PcMachine::step_mode` (including the inlined interpreter loop),
10.72% in `Windows::linear`, 7.38% in `Cpu::batch_block`, and 4.35% in
`PcBus::read_u32`. These are sampled function costs, not separate measured
speedups; the profile motivates experiments on address checks and RAM access.
Profiled timings are excluded from the FPS results above.

### Native address and RAM-bus follow-ups

The native-only `Windows::linear` inlining experiment produced 31.265 unpaced
host frames/s, up from the 30.053 baseline median (4.0%). Its three host times
were 31.005824, 30.993403 and 30.916850 seconds. The demo still reported 969
frames in 38.7 guest seconds (25.1 guest FPS).

Six alternating-order pairs of the 400M-step replay compared the ordinary PC
bus with the borrowed RAM bus. The ordinary-bus median was 4.611429 host
seconds; the borrowed-bus median was 4.311689 seconds, a 1.06952 paired
throughput ratio, with all six pairs favoring the borrowed bus. A separate
410M-step differential replay matched full portable state and cumulative PCM
and MIDI at all nine checkpoints. The borrowed bus is now enabled by default on
native targets and disabled on WASM. The transient policy is omitted from saved
states; `Session::set_borrowed_interpreter_bus(false)` selects the ordinary bus
for reference comparisons. The timedemo harness uses the borrowed bus by default;
`QUAKE_TIMEDEMO_BORROWED_BUS=0` selects the ordinary bus.

With the borrowed bus plus a zero-prefix VGA fallback and native inlining of
`X87::ready` and `Cpu::batch_block`, the timedemo median reached 35.621 unpaced
host frames/s (27.202785 seconds), with a 35.513–35.832 range. These runs used
the same start-state hash as the baseline and again produced 969 frames in 38.7
guest seconds. This is an 18.5% throughput gain over the baseline, but it does
not reach 60 host frames/s; that target is still 1.68 times the measured
throughput. A one-run helper control measured 35.279 host frames/s.

A read-guard-skipping variant passed the 410M-step state and audio comparison,
but its three-run timedemo median was 33.727 host frames/s, below both the
35.279 helper control and the 35.621 helper result. That variant was removed.
The borrowed-bus measurements are native-only; its policy and the inlining
annotations retain the browser/WASM defaults. Browser performance was not
measured. These host-throughput results exclude the GUI
and do not establish displayed 60 FPS.

### Retained bounded-loop and RAM-inline changes

A separate candidate combines bounded interpreter loops with native
`BorrowedRamBus` read/write inlining. Three timedemo runs measured 25.765044,
25.695526 and 25.623271 host seconds, for a median of 37.711 host frames/s and
a 37.609–37.817 range. It produced the same 969 frames, 38.7 guest seconds,
25.1 guest FPS, 2,570,620,000 scheduling steps and start-state hash as the
baseline. That is 25.5% above the baseline median and 5.87% above the preceding
35.621 helper result.

The combined selective-guards-off/on 410M-step differential replay completed
successfully: full portable state and cumulative PCM/MIDI matched at all nine
checkpoints. The combined candidate therefore retains both the bounded-loop
change and the selective code guards. This confirms the measured candidate's
state and output equivalence for this replay; it does not establish browser
performance or displayed desktop frame rate.

The detailed run output is in `target/quake-timedemo-inline.log`,
`target/quake-borrowed-timing.log`, `target/quake-borrowed-helpers-replay.log`,
`target/quake-timedemo-helpers.log`, `target/quake-helpers-control.log`,
`target/quake-timedemo-read-guards.log`, `target/quake-timedemo-bounded-loop.log`,
`target/quake-bounded-loop-replay.log` and
`target/quake-bounded-loop-guards-replay.log`.

## DJGPP and compiled x87 work plan

Work started on 2026-10-21. The target is 60 actual Quake frames per host
second; the retained interpreter measurements above do not meet it. The first
implementation keeps the existing exact x87 semantics and interpreter fallback.

| Workstream | First deliverable | Acceptance evidence |
| --- | --- | --- |
| DJGPP/COFF | Port the bounded parser, symbolic sections, live layout watcher and segment-aware dispatch from `98b69a4` onto the current interpreter | Parser/binding/segment tests, then real Quake source-layout detection and full-state/audio replay |
| x87 helpers | Compile shared Rust FPU operations into a diagnostic WASM runtime; compare imported helpers with a merged module | Complete FPU state and output equality, then repeated timings of the same bounded mixed loop |
| Loop compiler | Admit a conditional early exit in an otherwise linear bounded loop, including memory-dependent conditions | Differential execution across tight grants, both branch outcomes, memory effects and exact exit EIP/work |
| Integration | Connect supported x87 operations and loop shapes to live DJGPP code after the helper proof | AOT instruction coverage, calls and side exits; paired real-game state/audio checks and repeated timedemo results |

Two 6.1 Sol agents own the x87 proof and loop compiler. A Luna agent audits
existing DJGPP work and handles isolated admission tests. The coordinating agent
owns the DJGPP port, review and integration. Builds and performance measurements
run serially; agents edit separate files. Native and browser performance require
separate measurements. A diagnostic helper proof alone does not establish
production AOT coverage or game speed.

The first DJGPP port retained the module-wide short-procedure rejection policy
and declined nonflat virtual operand slots. The admission follow-up below
replaces those restrictions. Recognizing COFF and compiling a module still
does not imply that its entries qualify for continued execution. Compiler
cache revisions isolate old modules and rejection markers from new admission
and code generation rules.

Supported AOT VGA operations already execute their logic inside generated WASM.
The new helper experiment targets shared Rust x87 semantics and call overhead.
The native Wasmtime configuration already enables compiler inlining; linking
helper bodies into the module makes them available to that optimizer but does
not guarantee that it will inline them or improve throughput.

### Production integration after the helper experiment

The diagnostic helper owns its FPU and uses explicit real-mode metadata. It is
not yet an operation in the production procedure IR. Production integration
needs typed FLD32/FMUL32/FADD32/FISTP32 operations and procedure-only lowering;
otherwise x87 still ends a compiled region before its loop backedge.

Native calls need a versioned, fixed-size, allocation-free transfer of the
complete FPU state at procedure entry and every complete or partial exit. The
architectural x87 save image alone omits internal retained values. During a
call the backend is authoritative; fallback resumes only after its FPU state
has been committed alongside GPRs, flags and EIP. A backend trap remains fatal.
The performance comparison must include this boundary cost. Browser helpers
can initially borrow the worker's owner through direct WASM calls; independently
compiled modules must not dereference each other's private Rust layouts.

Admission must check FPU presence and CR0.EM/TS as well as existing memory and
device grants. A FISTP store needs a complete destination and source-page proof
before conversion or pop. Each accepted helper counts as one original x86
instruction; refusal leaves its state unchanged and exits at that instruction.
The helper context must retain the original FOP, source EIP, CS/data selectors
and effective operand offset separately from its linear RAM address. DJGPP's
nonzero segment base makes that distinction observable in FPU state.

### First implementation results

The native COFF integration passed a real Quake replay of 200,000,000 steps
(3.01 guest seconds), comparing the complete portable state and cumulative
PCM/MIDI at four checkpoints. It installed 500 procedures and executed
1,439,348 instructions in 401,932 calls: 0.72% of scheduling steps and 3.58 instructions
per call, with 53 side exits and no source refusals. The test's two-sample
diagnostic timings still favored the interpreter; they are not a new FPS
benchmark. A separate automatic-AOT run followed three live code layouts and
rejected their short procedures, retaining the interpreter as intended.
Logs: `target/djgpp-quake-aot-replay.log` and
`target/djgpp-quake-auto-aot.log`.

The x87 helper proof compiles existing exact Rust operations into WASM. Native
tests compare each operation with the actual x86 interpreter for PC=24/53/64,
all rounding modes, signed zeros, subnormal values, infinity and NaN. Refusals
preserve the input state; execution allocation checks pass. The WASM comparison
checks complete serialized FPU state and an explicit integer frame against a
native reference before and after every measured run.

After a competing Cargo build finished, nine rotated samples of 2,000,000
iterations with CW=0x0c7f produced these medians under Wasmtime 45.0.3,
Cranelift Speed, Backtracking and compiler inlining enabled:

| Execution of four x87 operations plus integer loop work | Median ns/iteration | Range of host seconds per sample |
| --- | ---: | --- |
| Rust reference arithmetic in WASM | 111.778 | 0.222570–0.224967 |
| Rust exact fast loop in WASM | 47.869 | 0.094827–0.096620 |
| Generated WASM calling helpers in another module | 50.814 | 0.101260–0.106251 |
| Generated WASM and helpers in one merged module | 48.527 | 0.096534–0.097809 |

The imported/merged median ratio was 1.0471. A separate repeat with Binaryen
`wasm-opt -O3` after merging produced only a 1.0152 ratio; that extra pass is
not selected. Both variants preserved complete state. These are synthetic
native Wasmtime results, not Quake FPS or browser results. Timing excludes
compilation, initialization, reset, snapshots and the future production FPU
boundary transfer. The reference-versus-fast arithmetic difference reuses the
existing x87 optimization; it is not a newly delivered game speedup.

With the pinned Binaryen tools on PATH, reproduce the merged-module proof:

```sh
cargo test -p doslet-static-probe --locked --offline
cargo test -p doslet-wasm-aot --example compare-x87-helpers --locked --offline
cargo build -p doslet-static-probe --target wasm32-unknown-unknown --release --locked --offline
cargo run -p doslet-wasm-aot --example compare-x87-helpers --release --locked --offline -- emit target/x87-helper-program.wasm
wasm-merge target/wasm32-unknown-unknown/release/doslet_static_probe.wasm doslet_static target/x87-helper-program.wasm x87_program -o target/x87-helper-merged.wasm
cargo run -p doslet-wasm-aot --example compare-x87-helpers --release --locked --offline -- target/wasm32-unknown-unknown/release/doslet_static_probe.wasm target/x87-helper-merged.wasm 2000000 9
```

Results are recorded in `target/x87-helper-native-tests.log`,
`target/x87-helper-emitter-tests.log`, `target/x87-helper-benchmark-confirm.log`
and `target/x87-helper-benchmark-opt-confirm.log`. The helper remains diagnostic;
production x87 lowering and the complete FPU boundary wire are still pending.

Integration validation also passed 237 `doslet-aot` tests with the `wasm`
feature, workspace Clippy with warnings denied, and a wasm32 check of the
desktop/browser entry points, web worker, web audio, time and executable crates.
These checks establish build and test coverage, not browser performance.

## Measurement-first follow-up

The next decision uses executed region lengths and entry-specific work before
adding production x87 lowering. A 37.711-to-60 host-frame target needs a 1.591
throughput ratio. Amdahl's law gives covered fractions of about 74%, 56% and
50% for local speedups of 2, 3 and 4, respectively, but these are fractions of
baseline execution **time**, not instruction counts. Instruction coverage is
an inexpensive screening metric; it does not establish the speedup. Use 30%
instruction coverage as a working threshold before another AOT timedemo, not
as proof that 60 FPS is reachable.

The diagnostic replay measures run lengths between x87, CALL/RET, INT,
I/O and observed memory-predicate loop exits over the same 400M-step window.
It must preserve configured DPMI Direct dispatch: the existing bus-analyzer
observer deliberately reflects services and therefore changes the instruction
stream. `Session::debug_observed_step_configured` records actual post-service
CPU entries without that substitution. IRQ/service-only steps and repeated REP
iterations are separate categories. Instrumented timings are not performance
results. Boundary-relaxation histograms describe possible region lengths, not
the set of instructions the current AOT compiler can actually lower.

Per-entry counters separate accepted calls, translated instructions and side
exits. The initial measurement retained the module-wide admission heuristic;
the admission follow-up below adds entry selection. The real-game AOT comparison prints the
entries with the most translated work and checks their totals against the
aggregate. Its optional legacy timings require `QUAKE_COMPARE_TIMING=1`;
equivalence and coverage remain the default experiment.

The code audit identified these implementation constraints:

- Bounded cyclic fragments reuse the current bounded emitter. Legacy admission
  requires `finite_maximum_work()` even when an explicit work limit exists.
  The new opt-in `from_bound_graph_with_bounded_fragments` requires an explicit
  work limit, retains cycle and graph validation, and admits memory-counter
  fragments without finite-computation metadata. At this measurement stage,
  frontend defaults still used legacy admission. All 239 AOT tests passed,
  including 336 WASM prefix/refusal cases for the new policy and rejection of
  unsupported graph shapes. The PC currently requires the full per-entry
  maximum to fit its device grant; partial-quota
  emission alone does not change that admission rule. A budget exit may land
  inside a loop, and compiled execution resumes only at a registered entry.
- Nonflat virtual slots need checked CS-base addition to the slot address,
  while the loaded operand value remains unchanged. Source-byte holes must use
  the same EIP-to-linear conversion. Slot-bearing modules continue to require
  recompilation when the base changes. The original
  [Quake surface patch routine](https://github.com/id-Software/Quake/blob/master/WinQuake/surf8.s#L733)
  writes the colormap address into 32 operand locations; this establishes the
  mechanism, not their execution share in this replay.
- Local `f64` x87 values are a candidate guarded representation. Existing
  exact fast arithmetic requires PC24/PC53, masked exceptions, representable
  operands and exponent bounds of +/-448. Guards must cover new memory
  operands and intermediate results too. PC24 rounds after each operation;
  using `f32` would impose the wrong exponent range. PC53 still needs residuals
  for directed rounding and precision/C1 status. Every side exit must preserve
  the full retained FPU state, including stale empty slots and pointer fields.
- A callback for INT, I/O or an unsupported CALL must commit state, respect
  service/device budgets, and validate the continuation after returning.
  Simply resuming with old locals is insufficient when a service changes CPU
  context or transfers control.

### 400M executed-stream results

The two exhaustive passes completed on 2026-10-21. Both used the same cached
source state as the earlier paired interpreter replay,
`43e684bbba7928cc5e889ed450afd66c0000d09a3dcf91a696ce680a4ac0e890`, followed by
10M warmup steps. The configured measured-start state was
`2d238deaa0dabe46ad2cede9e8d769abf4cc0527cab6533a43a9a35dc392c046`.
Observation, idle/adaptive policies and compiled execution were disabled except
for the explicit diagnostic observer. The two observed passes matched state
and audio digests at all eight checkpoints; the second also matched complete
portable state and PCM/MIDI against ordinary execution at all eight points.

The window contained 400,000,000 scheduling steps, 399,999,611 CPU instruction
entries and 389 steps without an instruction entry. The observer records
attempts rather than architectural retirement. There were no unexpected
post-instruction PC transitions. REP accounted for 3,597,832 entries, including
3,348,586 continuations; every REP quantum is excluded from the run payload.
The inventory retained 17,796 byte/context identities and 238 executed backward
edges. Memory-predicate exits use a conservative provenance proxy, not a formal
loop proof. `Register` means no tracked memory origin, not proven independence.

| Admitted instruction classes; other boundaries retained | Mean nonempty run length | Share of all observed entries in runs of at least 32 |
| --- | ---: | ---: |
| Recognized scalar instructions | 20.235 | 58.318% |
| Scalar plus FLD32/FMUL32/FADD32/FISTP32 | 13.340 | 59.669% |
| Scalar plus all classified x87 | 53.777 | 83.378% |
| Above plus CALL/RET | 100.168 | 92.574% |
| Above plus observed memory-predicate loop exits | 151.475 | 94.942% |

These are boundary-relaxation estimates, not achieved AOT coverage. They assume
admission of the stated classes without proving supported lowering, graph
shape, numeric guards, source guards or device grants. Long scalar runs already
exist, so the current 0.7–0.8% AOT work ratio cannot be explained by x87 cuts
alone. The four-operation subset adds only 1.351 percentage points of long-run
work. It also creates short isolated x87 runs, lowering the unweighted mean;
that does not establish a performance regression. All classified x87 adds
25.060 percentage points over scalar-only. Window-edge runs are truncated.

x87 contributed 61,013,025 entries (15.253%); CALL/RET contributed 3,575,555
(0.894%). INT and port I/O together contributed only 13,927 entries (0.00348%), so
their frequency does not justify prioritizing callbacks over region admission.
This is a frequency conclusion, not a measurement of their CPU-time cost.
The x87 control-word histogram was 59,481,874 PC24 entries (97.490%) and
1,531,151 PC64 entries (2.510%), with all exceptions masked and no PC53 entries.
Most PC24 entries used truncation, followed by rounding toward positive infinity.
This supports investigating exact PC24 locals; operand/intermediate numeric
eligibility remains unmeasured.

A follow-up byte probe at four frequent unknown sites (`0x1310eb`,
`0x130cc5`, `0x13147e` and `0x155498`) found two-operand IMUL (`0F AF`).
The existing AOT lowering supports register and memory forms, but
`translation::instruction_length` and `near_flow` use the interpreter's
cached execution decoder, which rejects these encodings. The same helpers
also supply PC procedure instruction metadata. Installation can retain a
binding without that metadata, but tier-aware admission then refuses it;
ordinary unlabeled native execution is not necessarily blocked. This is
therefore more than a diagnostic classification gap, but not an unconditional
installation failure. The probe
read the warmed baseline state; it does not establish the bytes of every
executed self-modifying variant. The recorded histogram remains unchanged
and conservatively excludes these unknown forms.

A separate freshly booted 400M-step AOT replay started at state hash
`817e6233a4731b2c71a4f8a1a9aafb2e1c57b9e307073fa5687881cf0d5a9082`.
It is not the histogram's identical start window. Complete state and PCM/MIDI
matched at four checkpoints. Its 500 installed procedures executed 3,087,222
instructions in 797,084 calls, or 0.772% of scheduling steps. Of 136 called
entries, seven averaged at least 32 instructions, but together they contributed
only 77,559 instructions in 884 calls: 0.0194% of scheduling steps. Per-entry
selection alone therefore cannot supply the missing coverage in this replay.
No AOT timing comparison or timedemo was run.

The resulting order is: align instruction-boundary validation with existing
IMUL lowering, connect bounded fragments to measured loop candidates and
correct nonflat operand-slot addresses; use per-entry statistics to assess
the resulting coverage; then specialize a broader x87 set for exact PC24
locals. CALL/RET support follows if it still materially limits those regions.
The four-helper proof remains useful for semantics and fallback, but is not
the selected production performance strategy by itself.

Reproduce the diagnostic window:

```sh
QUAKE_RUN_STEPS=400000000 QUAKE_BENCH_WARMUP_STEPS=10000000 cargo test -p doslet --release --locked --bin doslet quake_executed_instruction_run_histogram -- --ignored --nocapture --test-threads 1
QUAKE_COMPARE_STEPS=400000000 QUAKE_COMPARE_TIMING=0 cargo test -p doslet --release --locked --bin doslet quake_aot_matches -- --ignored --nocapture --test-threads 1
cargo test -p doslet --release --locked --bin doslet quake_instruction_site_bytes -- --ignored --nocapture --test-threads 1
```

Full histograms and boundary sites are in `target/quake-histogram-400m.log`;
per-entry counts are in `target/quake-aot-entry-coverage.log`. Unit and prefix
validation logs are `target/quake-histogram-unit-tests.log`,
`target/quake-boundary-api-test.log`, `target/quake-entry-stats-test.log` and
`target/quake-bounded-fragment-tests.log`.
The baseline byte probe is in `target/quake-boundary-site-bytes.log`.

### Admission follow-up

The cache builder now converts virtual operand-slot addresses from EIPs to
checked linear addresses using the live CS base. Loaded immediates and memory
displacements keep their original values. Source-hole lookup uses the
instruction EIP and compares the resulting linear slots with linear source
addresses. Overflowing slots, slots outside the segment and virtual address16
forms decline compilation. A moved module with embedded slot addresses still
requires recompilation.

Two-operand IMUL now has metadata-only instruction-length, fallthrough and
disp32-template support. This lets tier-aware admission use the existing AOT
lowering; it does not add an IMUL handler to the interpreter's decode cache.

`Recipe::DESKTOP` enables bounded cyclic fragments with an explicit
128-instruction ordinary-loop maximum. Recognized copy kernels retain a separate
4096-instruction reservation; finite and RLE computations retain their own
derived bounds. Cyclic fragments remain ordinary non-atomic calls: the entire
declared maximum must fit the device grant. The replay accepts
`QUAKE_COMPARE_WORK_LIMIT` to compare smaller bounds without changing the
production default, and `QUAKE_COMPARE_LEGACY=1` selects the previous planner.
Long acyclic fragments are also admitted when their static longest path reaches
32 instructions. A shorter branch or memory refusal may do less work, so this
does not replace live per-entry selection. Acyclic regions reserve their full
longest path and exit before unsupported x87 or CALL. No instruction quota was
added to their emitter. `QUAKE_COMPARE_LONG_FRAGMENTS=0` disables this extension.
The offline conversion example retains its previous planner selection.

Native automatic AOT checks each entry every 10,000 accepted calls across the
module. Each entry receives at least 128 calls before it can be disabled for
averaging fewer than 32 translated instructions. Unseen entries remain available
for sampling. Disabled entries retain compiled functions, source protection and
counters, but lose interpreter boundary hints until a replacement module is
installed. This is a policy based on the sampled window, not a proof of later
cost. If no entry remains enabled,
the existing deferred module-removal path restores interpreter-owned RAM.
The replay enables the same selection with `QUAKE_COMPARE_ENTRY_FILTER=1`.

Compiler cache revision `coff-regions-128-entry-trials-v4` separates this policy
from earlier modules and failure markers. These changes do not implement x87
locals or inter-procedure WASM calls, and do not establish a frame-rate gain.

The first real-game compilation exposed a rotated loop entry whose closing
edge was a synthetic fallthrough. The emitter requires a charged JMP/Jcc
latch. The validator now rejects that orientation, and the emitter returns an
error defensively instead of panicking. The supported orientation is checked
against original x86 execution at every quota prefix; synthetic edges do not
add guest instructions.

The follow-up replays used the same fresh-start state hash
`817e6233a4731b2c71a4f8a1a9aafb2e1c57b9e307073fa5687881cf0d5a9082` and 400M
scheduling steps. Complete saved state and PCM/MIDI matched the interpreter at
all four checkpoints in each replay:

| Planner and trial policy | Entries | Translated instructions | Calls | Work / scheduling steps |
| --- | ---: | ---: | ---: | ---: |
| Earlier finite-fragment admission | 500 | 3,087,222 | 797,084 | 0.7718% |
| Bounded fragments, limit 4096, no selection | 763 | 3,211,419 | 797,252 | 0.8029% |
| Bounded fragments, limit 128, no selection | 763 | 52,617,074 | 1,360,389 | 13.1543% |
| Bounded fragments, limit 4096, shared initial trial | 763; 3 retained | 148,417 | 10,156 | 0.0371% |
| Long and bounded fragments, ordinary 128 / copy 4096, no selection | 804 | 65,095,908 | 1,601,530 | 16.2740% |
| Same planner, shared initial trial | 804 | 21,075,279 | 356,838 | 5.2688% |
| Same planner, minimum 128 calls per entry | 804 | 59,145,136 | 700,595 | 14.7863% |

The smaller shared limit reduced budget refusals from 3,311,762 to 106,204.
Both runs reported zero executed semantic/finite kernels; nevertheless the
production compiler now separates the copy-kernel reservation to preserve
that contract for other workloads. Byte-identity tests verify unchanged copy,
finite and RLE output and equal-limit compatibility. Entry selection with the
large 4096 limit removed short calls but could not solve its grant refusals.
These measurements motivated the 128 ordinary-loop limit and long-acyclic
extension; they are not FPS or throughput results.

The shared initial trial was discarded: evaluating every entry after the first
10,000 module calls disabled hot regions before they had received enough
samples. In particular, EIP `0x7f02` disappeared from the selected replay.
Giving each entry at least 128 calls preserves that region: it executes
34,856,459 translated instructions in 281,641 calls. The selected replay
averages 84.42 instructions per AOT call overall. It retains 90.86% of the
unfiltered translated work with 43.75% of its calls. At the last selection,
695 entries remain enabled, including 665 still awaiting enough calls;
109 entries have been disabled. These figures describe the measured window,
not a guarantee about subsequent gameplay or wall-clock speed.

The replay has no learned execution labels. At that stage, long regions ending
before x87 still needed separate executed-site metadata: source decoding could
reject the unexecuted boundary while constructing a tier proof. The follow-up
below addresses that limitation. Production x87 lowering
and CALL/RET continuations remain unimplemented. Coverage is below the 30%
screening threshold, so no AOT timedemo or FPS comparison was run.

Reproduce the selected replay:

```sh
QUAKE_COMPARE_STEPS=400000000 QUAKE_COMPARE_ENTRY_FILTER=1 QUAKE_COMPARE_TIMING=0 cargo test -p doslet --release --locked --bin doslet quake_aot_matches -- --ignored --nocapture --test-threads 1
```

Logs: `target/quake-admission-4096-replay.log`,
`target/quake-admission-128-replay.log` and
`target/quake-admission-filtered-replay.log`. The earlier baseline is in
`target/quake-aot-entry-coverage.log`.
The long-region comparisons are in `target/quake-long-regions-replay.log`,
`target/quake-long-regions-filtered-replay.log` (discarded shared trial) and
`target/quake-entry-trials-replay.log` (per-entry trials).

### Executed sites and PC24 local regions

Procedure bundles now optionally name executed EIPs and instruction lengths
through `execution_sites`, separately from frozen source bytes. The owner
validates bounds, ordering, source coverage, decoded lengths, mutable operand
holes and finite/copy contracts. An unexecuted x87 boundary remains protected
against mutation and generated stores, but its tier no longer determines the
scalar region's tier. Missing metadata retains legacy decoding. The builder
omits explicit sites if an executed instruction lacks ordinary boundary
metadata, preserving interpreter fallback instead of rejecting the module.
Unknown boundaries and absolute operand slots both refuse rebasing and require
recompilation. Compiler cache revision `coff-execution-sites-v5` separates these
bundles from previous modules and failure markers.

The 400M-step Quake replay with `QUAKE_COMPARE_TIERS=1` enables an empty label
table, so all instructions use GREY classification at 266 MHz while the AOT
owner must prove and charge tiers. It starts from the same saved-state hash
`817e6233a4731b2c71a4f8a1a9aafb2e1c57b9e307073fa5687881cf0d5a9082` as the
earlier admission replay, then enables this different accounting mode.
All four checkpoints matched full state, drained PCM/MIDI, execution-tier
counters and timing. Its 804 entries completed 57,256,663 AOT instructions in
665,041 calls: 14.3142% of scheduling steps and 86.10 instructions per call.
All translated work was charged to GREY; 36,210 tier-proof refusals remained.
This is not a replay with a learned mixed-tier profile. The previous 14.7863%
result had labels disabled. Synthetic tests separately cover mixed labels,
unexecuted cold x87 boundaries, mutation/protected pages, rebasing and invalid
metadata. The complete PC procedure suite passed 86 tests; app integration and
manifest parsing passed three and five tests respectively.

```sh
QUAKE_COMPARE_STEPS=400000000 QUAKE_COMPARE_ENTRY_FILTER=1 QUAKE_COMPARE_TIERS=1 QUAKE_COMPARE_TIMING=0 cargo test -p doslet --release --locked --bin doslet quake_aot_matches -- --ignored --nocapture --test-threads 1
```

Logs: `target/quake-execution-sites-tiered-replay.log`,
`target/quake-execution-sites-pc-tests.log`,
`target/quake-execution-sites-app-tests.log` and
`target/quake-execution-sites-manifest-tests.log`.

`doslet-x87::X87::pc24_region` exclusively borrows its owner and imports only
declared physical read slots. Its local `f64` values support FLD32, all six basic
memory/register arithmetic forms, FLD ST(i), FXCH, valid FIST32 and explicit pop.
Entry requires masked exceptions, PC24 and exact normal/zero binary64 operands
within the fast arithmetic domain. Every arithmetic input is guarded again;
an out-of-domain intermediate therefore exits before its next arithmetic use. Rounding
uses the existing TwoSum/Dekker residuals and a PC24 bit-grid operation, with
sticky PE and exact C1. Arithmetic does not construct retained Finite or real80
values. Commit materializes modified slots once and preserves raw encodings,
zero provenance, stale empty data, TOP/tags and instruction/operand pointers.
Dropping a region discards its local work. Numerical refusals do not call the
reference implementation or alter the accepted prefix.

The API is a numerical component, not production x87 procedure lowering. CPU
admission still needs FPU presence, CR0 checks, segment/RAM/source proofs,
precise work accounting and bounded device grants. FISTP must prove its memory
destination before conversion and pop only after a successful write. PC53/64,
special/denormal inputs and out-of-range integer stores retain interpreter
fallback. Cross-engine native FPU transfer and browser gameplay performance
remain unmeasured.

The diagnostic compiles whole Rust regions into WASM and compares batches of
1, 4, 16 and 64 four-operation iterations. Each timed batch includes FPU entry
and commit. Complete serialized FPU state and the integer checksum must equal
the reference after warmup and every sample. A separate randomized program
check exercises all supported register/memory forms, rounding controls and
prefix refusals against the integer/extended reference in both native code
and actual WASM. It runs outside the timed samples.

The component measurement used the pinned Rust 1.99.0 release profile (thin
LTO, one codegen unit), 2,000,000 iterations at CW `0x0c7f`, warmup and nine
samples in rotating order. WASM ran under Wasmtime 45.0.3 with Cranelift Speed,
backtracking allocation and inlining enabled. Binaryen merged the helper
modules without a separate `wasm-opt` pass. Median nanoseconds per iteration:

| Implementation | Native x86-64 | WASM / Wasmtime |
| --- | ---: | ---: |
| Reference arithmetic | 78.847 | 112.866 |
| Existing exact fast operations | 39.291 | 49.585 |
| Generated WASM, imported helpers | — | 49.753 |
| Generated WASM, merged helpers | — | 47.982 |
| PC24 locals, 1 iteration per region | 26.367 | 44.311 |
| PC24 locals, 4 iterations per region | 19.678 | 25.870 |
| PC24 locals, 16 iterations per region | 17.871 | 22.421 |
| PC24 locals, 64 iterations per region | 17.508 | 21.207 |

At 16 iterations per region, native samples ranged from 0.033983 to 0.036072
host seconds, and WASM samples from 0.043691 to 0.045363 seconds. The WASM local
region takes about 2.14 times less time than merged helpers for this fixture,
including import/commit of the selected FPU state inside the WASM owner. This
does not include transferring a native guest FPU into a separate WASM memory,
production source/device admission, guest memory accesses or CPU dispatch.
The small gain at one iteration reinforces the need for long regions. It does
not establish a Quake frame rate or browser speedup.
Inspection of the emitted WASM (`target/x87-locals-runtime.wat`) found inline
`f64.add`/`f64.mul` operations and no arithmetic helper calls in `run_locals`;
its only call is a panic path for the preflighted-pop assertion. The generic
Rust stack representation still uses linear-memory scratch (a 1200-byte frame)
alongside WASM locals. Statically assigning guest ST(i) values to WASM locals
in the procedure emitter is covered by the later integration below.

Native and WASM each passed 3 seeds × 4 rounding controls × 256 random batches
of at most 32 operations, with complete serialized-state comparison on every
exit/refusal. The component suite passed 57 tests (one diagnostic timing test
ignored); six probe tests cover reference-CPU equivalence, random programs,
refusals and allocation-free bounded local execution. Logs are
`target/quake-pc24-local-tests.log`, `target/quake-pc24-probe-tests.log`,
`target/quake-pc24-native-benchmark.log` and
`target/quake-pc24-wasm-benchmark.log`.

Workspace Clippy with warnings denied, formatting, and the application/worker
WASM checks passed. The final native release rebuild encountered a rustc
SIGSEGV in ThinLTO; the retry with `RUST_MIN_STACK=33554432` succeeded. The
failure and successful retry are retained in
`target/quake-pc24-native-build.log` and
`target/quake-pc24-native-build-retry.log`. This is build evidence, separate
from the replay and numerical equivalence checks above.

Reproduce the component comparison:

```sh
cargo test -p doslet-x87 --locked
cargo test -p doslet-static-probe --locked x87
cargo build -p doslet-static-probe --target wasm32-unknown-unknown --release --locked
cargo run -p doslet-wasm-aot --example compare-x87-helpers --release --locked -- emit target/x87-local-program.wasm
wasm-merge target/wasm32-unknown-unknown/release/doslet_static_probe.wasm doslet_static target/x87-local-program.wasm x87_program -o target/x87-local-merged.wasm
cargo run -p doslet-wasm-aot --example compare-x87-helpers --release --locked -- native 2000000 9
cargo run -p doslet-wasm-aot --example compare-x87-helpers --release --locked -- target/wasm32-unknown-unknown/release/doslet_static_probe.wasm target/x87-local-merged.wasm 2000000 9
```

### PC24 instructions in compiled memory procedures

The explicit `pc24_locals` recipe now lowers supported x87 instructions into
the ordinary memory-procedure compiler. Desktop automatic selection remains
scalar while the experimental recipe is measured. The supported forms are
FLD real32, FILD integer32, FLD ST(i), FXCH, basic add/multiply/subtract/divide
in memory and register forms (including reverse operands and pop), and
FIST/FISTP integer32. A subsequent slice adds ordered FCOM/FCOMP real32 and
register forms, FCOMPP, FNSTSW AX and FST/FSTP ST(i). Unsupported forms retain
an interpreter boundary.

A setup pass propagates relative TOP through the CFG and requires agreement
at joins and backedges. Backward liveness determines which original slots
must be imported; writes have separate permissions. Eight guest values stay
in statically assigned WASM `f64` locals across scalar operations and loop
iterations. TwoSum, Dekker products, quotient residuals and PC24 rounding are
emitted inline with no arithmetic helper import. FILD32 preserves all 32
integer bits; the precision control applies to subsequent arithmetic.

`doslet_x87::pc24_boundary` defines a 192-byte little-endian wire format.
Its immutable input prefix records control, status, tags, permissions and
selected values. Output records changed values, their origins, popped slots,
status and instruction/operand pointers. Commit validates the whole output
before changing the parked FPU and materializes retained real80/Finite data
once. Original encodings, signed zeros and stale empty slots remain exact.
Unmasked exceptions, PC53/64, unsupported numeric domains and missing declared
inputs decline. The runtime also checks FPU presence and CR0.EM/TS.

The boundary starts at byte 456 after the existing CPU frame and memory
descriptor; six segment selectors follow at 648. The compiler protects the
full 664-byte frame from advertised RAM and metadata spans. Each FIST store
proves its destination before numerical conversion and pops only after the
write. Budget, memory and numerical side exits preserve the completed prefix
and resume at its next instruction. The CPU validates scalar results before
committing staged FPU output. Invalid backend results are fatal after possible
guest-memory effects; they never authorize entry replay.

Validation completed so far:

- `doslet-x87`: 62 tests passed, including portable-boundary validation and
  FILD32 differential cases; timing tests remain ignored.
- `doslet-x86-wasm`: all 280 tests passed. New emitted-loop tests compare
  original x86 execution for all four rounding controls, eight initial TOP
  positions and 13 grants that cut at different instruction boundaries.
  They check retained FPU state, registers, flags, RAM and exact work.
- `doslet-aot --features wasm`: all 243 tests passed, including opt-in
  discovery, FOP preservation, reverse operands and nontrivial addressing.
- PC procedure integration: 94 tests passed, including Wasmtime execution,
  failed-store/numerical exits, tiers, device time and allocation-free calls.

The nonflat application test also passes through the COFF cache builder,
embedded manifest parser, runtime installation and real WASM invocation. Its
three-iteration mixed loop matches 32 interpreted instructions, including
nonflat FIP, FOP, operand offset and selectors. All four nonflat app tests pass.

The first real-game replay with this recipe starts from the same saved-state
SHA256 `817e6233a4731b2c71a4f8a1a9aafb2e1c57b9e307073fa5687881cf0d5a9082`
and uses the same 400M scheduling steps, active all-GREY labels, per-entry
128-call trials and 128-instruction ordinary limit as the earlier scalar run.
All four 100M checkpoints match full portable state, drained PCM/MIDI and
execution-tier/time counters. There are 816 installed entries and 89,866,484
translated instructions in 1,150,341 calls: **22.4666%** of scheduling steps,
compared with **14.3142%** for the earlier scalar recipe under the same mode.
This is 6.02 guest seconds. All translated work is charged to GREY; source
refusals are zero. The denominator remains scheduling steps, not a strict
retired-instruction count. No timedemo is run below the 30% coverage threshold;
these results do not establish a frame-rate or browser speedup.

```sh
QUAKE_COMPARE_STEPS=400000000 QUAKE_COMPARE_ENTRY_FILTER=1 QUAKE_COMPARE_TIERS=1 QUAKE_COMPARE_TIMING=0 QUAKE_COMPARE_PC24=1 RUST_MIN_STACK=33554432 cargo test -p doslet --release --locked --bin doslet quake_aot_matches -- --ignored --nocapture --test-threads 1
```

Logs: `target/quake-x87-boundary-tests.log`,
`target/quake-pc24-compiler-tests.log`, `target/quake-pc24-aot-tests.log` and
`target/quake-pc24-pc-tests.log`, plus
`target/quake-pc24-nonflat-tests.log` and
`target/quake-pc24-tiered-replay.log`.

#### Live opcode inventory after the first integrated replay

The separate two-pass inventory records original live instruction bytes and
checks ordinary execution against the observed pass, including portable state
and audio. Its window starts after 10M warmup steps and covers 400M scheduling
steps; it is not the saved-state window used for the coverage comparison above.
Both passes completed with identical execution and no overflow of the bounded
8,192-entry x87 encoding table (`target/quake-x87-live-inventory.log`).

The inventory records 1,880,531 FNSTSW AX executions, 1,679,381 FCOM/FCOMP
real32 or register executions, and 1,155,318 FSTP ST(i) executions. These are
counts of individual instructions, not the surrounding work a larger compiled
region would capture. The hottest live x87 sequence begins at linear address
`0x1310fb`; each of its first seven instructions executes 235,453 times.
Another long sequence begins at `0x130f4a` with 192,040 executions per site.

Inspection of the original executable identifies two separate compiler limits:
the span loop around `0x130ce3` crosses FCOM, FNSTSW AX and a discard pop;
the loop beginning at `0x1310fb` has an internal branch whose paths reach a
join with different stack depths. Supporting the missing opcodes alone does
not make the latter graph admissible. Keeping a valid long prefix with an
exact interpreter exit is a separate candidate for increasing coverage.

The comparison slice uses finite `f64` ordering, including signed-zero
equality, to update C0/C2/C3 and clear C1 while retaining sticky exception
bits. All operands are checked before a compare or pop. FNSTSW AX preserves
the upper half of EAX, integer flags and the previous FPU instruction/operand
pointers. Register stores copy retained values and tags without rounding;
their destination may be empty. The boundary wire version is now 2 and allows
the three finite comparison outcomes or unchanged initial condition bits.
The native compiler cache revision advances to `coff-pc24-boundary-v6`.

The comparison extension passes 12 focused compiler tests (including six new
tests), all 244 AOT tests, 96 PC integration tests and 67 x87 tests; one x87
timing test is ignored. The same 400M saved-state replay also passes all four
full-state/audio/tier checkpoints. However, coverage **regresses to 19.3243%**:
77,297,041 translated instructions in 945,048 calls across 772 entries.
Entries containing x87 account for 24,864,204 mixed scalar/x87 instructions;
that is not a count of x87 instructions alone.

Previously hot entry EIP `0x10ce3` disappears from this plan. Admitting more
instructions also joins candidates into larger graphs that can fail existing
CFG/stack-topology limits; the earlier interpreter boundary had made a useful
smaller region available. This result motivates preserving valid fragments
when a larger candidate fails. The new opcodes alone are not an accepted
performance improvement. Log: `target/quake-pc24-compare-tiered-replay.log`.

The first recovery pass retains a straight prefix from a rejected PC24
candidate, including node-bound failures and unsupported CFG/TOP joins. It
requires at least 32 instructions, stops before the first control transfer,
and caps work at the ordinary grant. The next original instruction remains
source-guarded but is represented as a zero-work interpreter exit. Recovery
reuses bounded planning storage and consumes the existing scan budget.
Legacy recipes and already accepted candidates keep their behavior.

All 250 AOT tests and five nonflat application tests pass after this change.
The latter include actual execution of a recovered mixed prefix, checking
exact EIP, 32 charged instructions and retained FPU state. Seven manifest
tests and the native owner's allocation-free boundary test also pass. The
400M replay remains equivalent at all four checkpoints, but gains only one
entry and 51,917 translated instructions: **19.3372%** coverage, 77,348,958
instructions in 945,999 calls. This simple prefix policy does not recover
the branch-containing entry lost above. Logs:
`target/quake-pc24-prefix-aot-tests.log`,
`target/quake-pc24-prefix-nonflat-tests.log` and
`target/quake-pc24-prefix-tiered-replay.log`.

The second recovery pass keeps exact FNSTSW AX continuations as candidates.
If the whole candidate fails, a continuation gets one additional attempt
with interpreter exits at targets below its entry address. Full graph/CALL
roots and successful unfloored candidates retain their existing policy.
This attempt precedes straight-prefix recovery and shares the same scan
budget; it can preserve an inner loop without accepting its outer graph.

All 254 AOT tests pass, including four new continuation-policy tests. On the
same 400M replay, **coverage reaches 23.0246%**: 92,098,252 instructions in
1,132,542 calls, across 866 installed entries. All four state, PCM/MIDI and
tier/time checkpoints match; source refusals remain zero. The recovered
`0x10ce3` entry contributes 14,535,239 instructions in 179,813 calls.
Entries containing x87 contribute 39,585,862 mixed instructions, with the
same mixed-region counting caveat as above. This result improves on the
first integrated recipe's 22.4666% and the scalar recipe's 14.3142%, but is
still below the 30% timedemo threshold. Log:
`target/quake-pc24-status-tiered-replay.log`.

A separate bounded CFG probe explains why the hottest observed draw code
does not benefit yet. With no extra trusted entries, the original executable
and cached live text both produce a 56,431-node graph that excludes EIPs
`0x10f4a` and `0x110fb` (linear `0x130f4a` and `0x1310fb`). They are absent
from discovery, rather than rejected by x87 lowering. Diagnostic explicit
seeds make the first entry admissible with 40 operations. The second reaches
the instruction-node limit; its first branch follows only 20 operations,
so the 32-instruction straight-prefix policy cannot retain it. The production
planner contains no game-specific entry list. The next work is bounded
discovery from observed instruction entries with source-identity validation,
followed by preserving supported fragments containing branches.
The optional `QUAKE_COMPARE_DUMP_TEXT=1` replay diagnostic confirms that all
368,984 bytes of its actual compiled text equal the cached probe input, so
these discovery findings apply to the current replay. Evidence:
`target/quake-cfg-presence-fresh.log`.

The workspace Clippy gate passes with warnings denied. Native and browser
manifest tests share the same permissions schema; the browser app and worker
also pass the `wasm32-unknown-unknown` check. These build results do not replace
browser execution or frame-rate measurements. The explicit recipe remains
disabled in automatic desktop selection.

#### Short host-time check

A 40M-step replay (0.60 guest seconds) tests the same PC24 recipe with and
without diagnostic entry EIPs `0x10f4a,0x110fb`. Correctness replay coverage
rises from 29.5189% to 31.6781% in this shorter window. Both variants preserve
full state, drained audio and tier counters at all four checkpoints.

Host timing uses five runs per variant, alternating interpreter/AOT order,
with installation excluded, entry admission sampling enabled, and audio
drained. Every timed run also matches the reference final portable state.

| Recipe | Interpreter median | AOT median | Additional host time |
| --- | ---: | ---: | ---: |
| Existing PC24 entries | 0.417815 s | 0.566194 s | 35.51% |
| Additional diagnostic entries | 0.409200 s | 0.568895 s | 39.03% |

AOT ranges are 0.554226–0.568185 s and 0.561708–0.579854 s, respectively.
The 0.48% difference between AOT medians is smaller than the observed spread;
the additional entries do not demonstrate a host-speed improvement. Both
AOT variants take substantially longer than their interpreter controls.
This is a native headless replay measurement, not a displayed-FPS or browser
measurement. The next performance investigation must separate the costs of
installed AOT storage/dispatch, entry/exit handling and compiled execution.

Logs: `target/quake-short-timing-baseline.log` and
`target/quake-short-timing-seeded.log`. Reproduce with `QUAKE_COMPARE_STEPS=40000000`,
`QUAKE_COMPARE_PC24=1`, `QUAKE_COMPARE_ENTRY_FILTER=1`, `QUAKE_COMPARE_TIERS=1`,
`QUAKE_COMPARE_TIMING=1` and `QUAKE_COMPARE_TIMING_PAIR=1` on the existing
ignored replay test. The second run also sets the test-only
`DOSLET_AOT_TEST_ENTRY_EIPS=0x10f4a,0x110fb`; normal builds do not read it.

#### Procedure analysis before AOT

The memory-procedure pipeline now has an explicit preparation stage:

```text
bound CFG → supported procedure IR → analysis and selection → WASM → native AOT
```

`ProcedureGraphProgram::prepare` validates the IR and constructs private
lowering proofs without emitting WASM. It exposes each entry's execution
class, work reservation, declared interpreter exits and returns. The classes
are `acyclic`, `bounded-loop`, `finite`, `guarded-finite`, `copy` and `rle`.
The prepared object borrows immutable IR; `compile` consumes its proofs.
The shared application builder uses preparation to select individual entries,
then prepares and emits the selected module. It no longer emits a temporary
WASM module for each candidate during selection.

The independent x87 classification is:

| Class | Meaning |
| --- | --- |
| `none` | No x87 instructions in the region. |
| `metadata-only` | Reads FPU metadata without reading value slots or changing FPU state. |
| `initial-values` | Needs at least one value present at entry. |
| `independent-balanced` | No initial values needed; every declared exit restores entry stack depth. |
| `independent-unbalanced` | No initial values needed; declared exits do not establish balanced stack depth. |
| `unknown` | Signed stack topology could not be established. |

Effects include read/write masks, status/control/pointer requirements, signed
stack extrema, and a stack delta for each declared exit. Stack analysis uses
signed depth rather than TOP modulo eight: a cycle pushing eight values is
unknown, not balanced. All admitted PC24 entries still depend on the existing
control-word guard. Balanced exits do not imply unchanged status, retained
physical register values or instruction/operand pointers. Intermediate grant,
memory and numeric refusals can expose a partially occupied stack; these
summaries do not authorize skipping the existing FPU boundary or IRQ checks.

Bundles embed `analysis_version: 1` and per-function `analysis` diagnostics.
Runtime owners continue to use their existing validated bindings; diagnostic
metadata grants no extra privileges. Native cache revision
`coff-procedure-colors-v7` causes old modules to be regenerated with analysis.
Desktop PC24 remains opt-in.

The Quake replay reports calls, translated instructions and side exits by
execution/x87 class. These instruction counts include the whole region, not
only x87 operations. Set `QUAKE_COMPARE_DUMP_COLORS=1` to save per-entry
analysis and execution counts to `target/quake-procedure-colors.json`.
This report can guide a smaller FPU boundary or whole-procedure admission;
the classification change alone does not establish a gameplay speedup.

On the same 40M-step PC24 replay, 866 entries are installed, including 220
containing x87. The four full-state, PCM/MIDI and tier checkpoints match the
interpreter. Translated work remains 11,807,554 instructions in 162,181 calls
(29.5189% coverage), identical to the earlier short baseline.

| x87 class | Translated instructions | Share of all AOT work |
| --- | ---: | ---: |
| `none` | 6,936,043 | 58.7424% |
| `initial-values` | 3,369,791 | 28.5393% |
| `independent-unbalanced` | 1,450,514 | 12.2846% |
| `independent-balanced` | 48,060 | 0.4070% |
| `metadata-only` | 3,146 | 0.0266% |

Balanced independent regions account for 0.9866% of work in regions containing
x87. These are selected AOT regions, including partial functions; the result
does not establish the corresponding proportions for complete Quake functions
or untranslated code. No finite or guarded-finite entry is called in this
short window. This points toward investigating region boundaries and incoming
value transfer as well as balanced-procedure specialization.

Evidence: `target/quake-procedure-colors.log` and
`target/quake-procedure-colors.json`. Reproduce with:

```sh
QUAKE_COMPARE_STEPS=40000000 QUAKE_COMPARE_ENTRY_FILTER=1 \
QUAKE_COMPARE_TIERS=1 QUAKE_COMPARE_PC24=1 QUAKE_COMPARE_DUMP_COLORS=1 \
RUST_MIN_STACK=33554432 cargo test -p doslet --release --locked --bin doslet \
  quake_aot_matches -- --ignored --nocapture --test-threads 1
```

Validation also passes 297 `doslet-x86-wasm` tests and nine application nonflat
layout tests. The new cases cover deferred emission, invalid inputs, execution
classes, signed stack cycles, divergent exits and PC24 preparation through
successful WASM generation. Scoped Clippy checks for `doslet`, `doslet-web`
and `doslet-x86-wasm` pass with warnings denied. `doslet` and
`doslet-web-worker` also pass the `wasm32-unknown-unknown` check; browser
execution and FPS are not measured here.

#### Color-based admission timing

The ignored replay accepts `QUAKE_COMPARE_COLOR_TIMING=1` to compare five
entry-selection policies on the same state: interpreter, all AOT entries,
entries without x87, entries without x87 plus metadata-only and independent
balanced x87, and entries containing x87. Selection occurs before the timer.
Each policy first passes an untimed full-state and drained PCM/MIDI comparison,
then runs five timed replays in rotating order. Every measured replay checks
its final portable state. The normal per-entry trial filter runs inside the
timer and intersects the color policy in one update; excluded entries must
have zero calls. Module installation and validation hashing are not timed.
This tests selection by color, not a smaller FPU ABI or different numerical
lowering. Production entry selection remains unchanged.

A separate four-policy control, with the existing 128-instruction ordinary
limit, isolates installation/storage from active execution:

| Policy | Median host time | Range |
| --- | ---: | ---: |
| Interpreter | 0.407740 s | 0.402273–0.412947 s |
| Active AOT | 0.539202 s | 0.530750–0.545600 s |
| AOT installed but suspended | 0.408286 s | 0.403368–0.414023 s |
| AOT installed, then removed | 0.403489 s | 0.400789–0.429262 s |

All runs execute 40M scheduling steps, about 0.60 guest seconds. Active AOT
adds 32.24% host time in this control. The suspended result is within the
interpreter's observed range; this run does not show a large shared-storage
penalty. It does not separate admission, host/WASM transitions, FPU conversion
and generated-code costs. Evidence: `target/quake-colors-storage-baseline.log`.

With color selection enabled, all five policies pass the untimed state/audio
check and every timed final-state check. The same 40M-step replay gives:

| Entry selection | Median host time | Range | Extra time versus interpreter |
| --- | ---: | ---: | ---: |
| Interpreter | 0.411155 s | 0.395785–0.422273 s | — |
| All AOT colors | 0.543268 s | 0.537390–0.561546 s | 32.13% |
| No x87 | 0.506802 s | 0.494678–0.513341 s | 23.26% |
| No x87 + metadata-only + independent-balanced | 0.498733 s | 0.495262–0.518537 s | 21.30% |
| x87 regions only | 0.483506 s | 0.476427–0.519500 s | 17.60% |

The lowest AOT median is 11.00% below all-colors AOT, but remains slower than
the interpreter. The no-x87 and no-x87-plus-balanced ranges overlap, so these
runs do not establish a benefit from adding balanced regions. Color-based
selection alone does not provide an emulator speedup in this window. These
are native headless CPU replay timings, not displayed FPS or browser results.

Evidence: `target/quake-colors-ab-timing.log`. Reproduce with:

```sh
QUAKE_COMPARE_STEPS=40000000 QUAKE_COMPARE_ENTRY_FILTER=1 \
QUAKE_COMPARE_TIERS=1 QUAKE_COMPARE_PC24=1 QUAKE_COMPARE_COLOR_TIMING=1 \
RUST_MIN_STACK=33554432 cargo test -p doslet --release --locked --bin doslet \
  quake_aot_matches -- --ignored --nocapture --test-threads 1
```

A follow-up pair raises `QUAKE_COMPARE_WORK_LIMIT` to 1024, retaining all
colors and the same hardware admission. Five rotating runs give interpreter
median 0.407888 s (0.402872–0.418817) and AOT median 0.537176 s
(0.531992–0.545237), still 31.70% additional host time. The four correctness
checkpoints and every timed final state match. In the correctness replay,
calls fall from 162,181 to 113,876, but translated work also falls from
11,807,554 to 11,213,303 instructions, and budget refusals rise from 16,029
to 112,341. Larger reservations therefore reduce call count while making
entry admission harder. This experiment does not demonstrate a timing gain
over the 128-step control; their observed AOT time ranges overlap.

Evidence: `target/quake-colors-work1024-timing.log`. Use the ordinary timing
mode with `QUAKE_COMPARE_TIMING=1`, `QUAKE_COMPARE_TIMING_PAIR=1` and
`QUAKE_COMPARE_WORK_LIMIT=1024` instead of `QUAKE_COMPARE_COLOR_TIMING=1`.
The tested selectors and work-limit override remain local replay diagnostics;
these measurements do not justify a production default change.

#### Archived instruction-metering experiment

A local native experiment compared the current graph emitter with budget checks
and instruction accounting, without budget checks, and without either. It used
naturally terminating register, RAM read/add/store and PC24 x87 loops. Two runs
each used seven rotated timing rounds after warmup. Timings included a 664-byte
frame/descriptor reset and the Rust-to-WASM call. Memory protection, page
generations and memory-effect counters remained enabled.

For 256 iterations per call, removing both mechanisms gave these ratios across
the two runs:

| Loop | Counted / unmetered time |
| --- | ---: |
| Registers | 1.0064–1.0088x |
| RAM read/add/store | 0.9978–1.0042x |
| PC24 x87 | 1.0161–1.0302x |

Sample ranges overlapped. Full WASM memory comparisons passed, excluding only
the deliberately unchanged charged-work field in the unmetered variant.
Natural exits, expected outputs and work totals were also checked. These
synthetic results do not establish a Quake FPS gain. Removing charged work
from the whole game would additionally require replacement guest-time accounting.

The temporary test, WASM rewriter, added development dependencies and raw logs
were removed after the experiment. Production AOT behavior was unchanged.
