# Static EXE-to-WASM target and experimental profile backend

The required backend analyzes an executable offline and emits one large WASM
code module. The execution contract below records the target design. The
existing profile-only translator is an earlier experimental prototype; its
implementation and measurements are documented separately below.

## Required static AOT execution contract (planned)

The converter takes the EXE as its code source. It reconstructs executable
objects, pages, entry points and relocations, then builds control flow for
statically recoverable code, including paths absent from runtime profiles.
Speed-tiers JSON profiles guide optimization, region layout and hot-path choices.
The output is one module containing the translated program, with internal direct
control flow and calls into the WASM runtime. Operand widths 8, 16 and 32 bits
belong to the translation scope. Dynamically loaded, generated or modified code
and unresolved control transfers retain interpreter fallback.

Runtime execution does not discover or compile hot blocks, rebuild a binding
table on each entry, or manage a translated-code cache. Static entry tables,
relocation bindings and invalidation information describe the compiled image;
they are separate from a runtime translation cache. The runtime remains the
exclusive owner of guest state, RAM, devices and services. The generated module
imports that runtime's memory and functions, with explicit access boundaries.

The host scheduling budget is **soft**. Generated execution may finish the
current block or bounded group of loop iterations after exhausting it:

- With accounting enabled, account completed guest work at block ends, rather than checking/updating
  counters after every instruction. Successful block paths have a known cost.
- Use signed or sufficiently wide accumulators so budget debt cannot wrap into
  a new positive allowance. Return actual completed work, including overshoot.
- For a narrow loop with a proven finite trip count, hoist the budget check and
  aggregate accounting to its entry when safe. An early exit or fault must still
  reconcile the count with work actually completed; architectural state and
  device effects are not advanced merely because work was reserved.
- Loops with unknown duration use infrequent backedge checkpoints over bounded
  groups of iterations. Loops awaiting guest I/O or another externally changing
  condition also need runtime service points. One entry check must not allow
  such a loop to monopolize the owner indefinitely.
- Preserve exact register, flag and partial-fault effects. Host budget overshoot
  does not authorize counting unexecuted work, dropping interrupts or moving I/O
  effects arbitrarily. Device timing remains the runtime's responsibility at
  explicit synchronization points, independently of UI refresh.

An optional **uncounted HOT** code-generation policy omits instruction/statistics
counter updates and instruction-budget charges entirely inside HOT regions.
COLD, GREY and other accounted regions retain the block-end policy above. The
compiler emits a counter-free HOT path; selecting this policy or a precompiled
entry variant occurs outside that path. Exact-accounting instrumentation remains
available as a separate option for profiling and differential validation.

Uncounted HOT execution still needs bounded runtime service points for I/O,
interrupts, input, stopping and long-running loops. Those scheduling mechanisms
are independent of counting HOT guest instructions. A region without such a
bounded return/service path must be split or use a different execution variant.
Runtime timing policy must provide device-time progression for this mode without
reintroducing a hidden per-instruction HOT counter. Reports identify charged work
separately and mark the total executed instruction count unavailable; an omitted
HOT count must not be reported as zero executed work. Benchmarks of this variant
use equivalent guest checkpoints/workloads rather than a claimed exact total
step count. Profile regions shared by work/wait or IRQ/main paths must retain
their context distinctions instead of permanently inheriting a caller's tier.

The scheduler continues to select adaptive and speed-tier policies independently
of translation. With accounting enabled, measurements compare actual completed
work; a reference replay must not assume requested and completed budgets are
identical. Offline image reconstruction and CFG discovery are implemented;
module generation, live binding and this soft-budget ABI are separate gates.
The ABI-v1 prototype below retains its existing strict grant contract and tests.

## Procedure-level AOT proposal and known interrupt limitation

This planned alternative selects complete computational procedures and emits
one WASM function per selected procedure within the static module. The
interpreter enters it with register/stack inputs and commits returned register,
flag and stack state. The procedure accesses borrowed guest memory directly.
The intended admission criterion is finite computation, including conditional
and nested loops. Each reachable cycle must make progress within a bound
established statically or checked from the live arguments. Waiting for an
external writer or device event is not computation admission. A memory read
alone does not identify a wait: scanning a bounded input buffer also reads
values that the procedure never writes. Progress and dependence on an external
event must be analyzed together.

Entry checks may establish buffer bounds and disjoint control state where
aliasing could invalidate a loop's progress. The resulting bound must exclude
counter/address wraparound and account for nested paths. Work is accumulated
inside WASM and returned at the procedure boundary, without interior IRQ polls.
The current linear-loop restriction and 4096-instruction grant are implementation
limits; neither proves that a rejected procedure waits or fails to terminate.
General finite-loop admission remains planned. The computation-kernel path
below now admits a complete branchy RLE decoder with a live termination proof.
Under the whole-computation policy, a proved finite call may finish
past the caller's soft batch budget; a separate per-call host-work bound keeps
execution responsive. This does not require IRQ polling inside the function.

Watcom register/stack conventions
guide discovery; observed instructions determine the actual contract and stack
cleanup. Code-page invalidation disables the affected procedure; procedures that
may modify their own executable bytes are excluded from the initial path. The
procedure execution backend is not implemented by the region compiler documented
below. `doslet-aot::procedures::ProcedurePlan` implements the first offline
analysis slice: trusted entries and resolved direct-call targets, contiguous
code32 integer leaves ending in near RET/RET imm16, and at most 128 instructions
per procedure. It reports read-before-write GPR byte masks, architectural EFLAGS
inputs/effects, conditional entry-ESP stack-read offsets and return cleanup.
Watcom argument order is a hint over observed register inputs, never an inferred
C signature. Nineteen focused tests cover effects, aliases, relocation windows,
exclusions and resource bounds; the complete AOT suite passes 199 tests.

On the local bound ROTT CFG (49348 instructions), this deliberately narrow slice
finds 16 leaves comprising 91 instructions and rejects 728 roots. First rejection
reasons include 218 direct calls and 337 conditional branches. These are analysis
results, not total procedure counts, executed coverage or a speed measurement.
Evidence: `target/static-rott-procedure-discovery.log`. Transitive callers and
internal control flow require separate analysis stages.

The opt-in `CallProcedurePlan` now adds resolved direct calls to eligible
straight-line bodies. A bounded iterative SCC pass rejects recursive call
cycles, then composes callees before callers, distinguishing possible from
definite GPR/flag writes. Repeated calls contribute repeated work; depth,
expanded work and allocations have independent limits. The complete AOT suite
passes 215 tests. The same ROTT CFG admits 19 bodies, including three callers,
107 local source instructions, depth at most two and expanded work at most 30
instructions per accepted entry. Evidence:
`target/static-rott-procedure-closure.log`. These are structural call-continuation
summaries: live source validity, actual return targets and guest stack effects
still require execution guards. They do not authorize native calls or stack
elision on their own. Internal conditional branches remain excluded.

Live code validation belongs to the interpreter's decoded-entry binding. When
decoding a CALL or recognizing a procedure entry, it can bind an eligible
unchanged procedure to an AOT function identifier. A valid cached binding then
dispatches directly to WASM; generated procedure functions need not repeat
source-byte or page-generation checks. A code write or mapping/context change
invalidates the binding, so subsequent decoding returns to ordinary execution
unless the live bytes again match the offline function. This reuses decoded
instruction caching, not a runtime translation cache.

The binding must cover the complete procedure and every statically included
callee. The current single-instruction `CodeCheck` covers the decoded
instruction's bytes, so a cached CALL's existing check alone cannot validate
its target body. Extend the binding's invalidation dependencies rather than
adding per-instruction checks inside WASM. Its validity ends if any dependency
changes. Runtime scheduling and guest stack obligations still apply at the
outer interpreter/procedure boundary.

For the first controlled ROTT procedure experiment, live invalidation need not
block testing. The eight source mismatches recorded in the earlier 2M-step
inventory are in object 3 (`0x696`, `0x69c`, `0x6cd`, `0x6d3`, `0x727`, `0x72d`,
`0x769`, `0x76f`); all 19 currently accepted procedure bodies are in object 1.
A new 2M-step native replay from the natural-wait checkpoint verifies that every
accepted body's initial loaded bytes match the bound offline image and that
every recorded source-page generation remains valid at the end. Page generations
also detect intervening writes followed by byte restoration. Validation occurs
only at replay boundaries, adding no per-instruction checks. Evidence:
`target/static-rott-procedure-stability.log`.

This supports omitting live invalidation from the measured procedure path in
that controlled experiment, while checking source stability outside timing and
discarding a run if it changes. It establishes code stability for this recorded
window, not procedure-WASM execution or all future game paths.

### First executable register-procedure slice

The optional CPU feature `procedure-calls` supplies the generic interpreter
entry adapter. A separate preparation pass,
`wasm::procedures::PreparedRegisterProcedures::from_bound_leaves`, decodes bound
leaf bodies into procedure IR and an immutable table of entry addresses,
function IDs, explicit `RegisterLeafNear32` ABI and exact instruction costs.
It rejects memory, explicit SP/ESP and other return ABIs with diagnostics.
This pass does not change the ordinary x86 decoder.

The CPU's borrowed `ProcedureRegistry` and generic `ProcedureExecutor` contain
no WASM/Wasmtime dependency. `Cpu::try_call_procedure` attempts a registered
leaf at its current entry, after the ordinary entering CALL has written its
real return address. It validates mode, actual SS/CS return bounds, ordinary
RAM, execution hazards and the owner-granted budget before invoking the
backend. On success it commits register/flag results and performs the actual
outer RET32. Backend errors or invalid results do not commit CPU state.
An ordinary decline leaves normal interpretation available. Active execution
labels currently decline; per-invocation tier accounting remains future work.
`Cpu::step` retains exactly-one-instruction behavior.

Registry construction explicitly requires `FrozenCode::AssumeImmutable` in
this experiment. The owner must freeze code/mappings, handle pending IRQs and
provide a grant bounded before the next device event. There are no interior
IRQ checks or source invalidation hooks. The feature stays disabled by default;
normal Session/game execution does not automatically install this registry.

An integrated native Wasmtime fixture now passes 1024 inputs through real
interpreted CALL, AOT leaf execution and architectural RET. Complete CPU
snapshots, all 64KiB of fixture RAM and page generations match ordinary x86,
including the real return-address store and unchanged caller stack pointer.
There is no ESP normalization in this test. Evidence:
`target/static-procedure-integrated-wasmtime.log`. CPU validation passes 95
all-feature tests and 78 feature-disabled tests; the bound second pass adds
three focused tests, bringing the complete offline AOT suite to 218.

```sh
cargo run -p doslet-wasm-aot --features procedure-calls --example check-procedure --release --locked --offline -- --integrated
```

`doslet-x86-wasm::static_program::procedure` compiles explicitly supplied
register-only procedure IR into one module with direct WASM calls. Inputs are
the eight GPR values and EFLAGS; results are those values and one bulk work
count. Internal calls carry registers/flags only. Each exported entry appends
its statically expanded instruction count on return, including repeated calls
and guest CALL/RET costs. The compiler reuses the region emitter's arithmetic
semantics, rejects explicit SP/ESP access, memory/stack operations and guest
branches/loops, and checks the complete call DAG before emission. It has no
imports, guest-memory interface, callbacks or interior interrupt checks.

The `doslet-wasm-aot::procedure` adapter compiles this module with Cranelift Speed
before execution and loads only its own native artifact. The executable
`check-procedure` fixture compares an independently encoded x86 A/B/C call chain
against that native artifact for 1024 deterministic register/flag inputs. A calls
B twice; B calls C. ADD/ADC, XOR AH,CL, INC AX and ROR AL exercise byte, word and
dword values, flags and partial aliases. All GPR/flag results and the bulk count
of 19 instructions agree. The module is 1253 bytes. Local evidence:
`target/static-procedure-first-native-test.log`.

```sh
cargo run -p doslet-wasm-aot --example check-procedure --release --locked --offline
```

This is a CPU procedure-IR execution test. It does not yet translate EXE procedure
plans, validate live source bytes, access guest RAM, integrate interpreter entry
or establish full guest-memory equivalence. The pure-register ABI preserves ESP;
the reference executes the outer RET, so the fixture explicitly normalizes its
four-byte stack pop before comparing registers. Guest stack bytes and return
targets remain obligations for the later adapter. This test supplies no game
speed, browser speed or interactive gameplay evidence.

The optional `--bench` mode measures the warmed CPU slice against native Rust
interpretation, with decode caching enabled and disabled. Both execute 262144
state-dependent invocations of A. A second grouping adds a procedure containing
64 direct CALL A instructions and a RET: 1281 expanded guest instructions per
entry, compared with 19 for an individual A entry. For each grouping, native
interpretation and WASM execute matching scalar computations and structural
instruction counts. Each variant receives a complete warmup and seven rotated
measurements. Compilation, loading, reset and final-state validation are excluded.
The native fixture bus implements ordinary-RAM reads and per-page code-generation
checks so cache-enabled interpretation can retain valid decoded instructions
across stack writes.

The first release measurement, under Wasmtime/Cranelift Speed, gave these medians
for the complete 262144-A workload:

| Entry grouping | Native interpreter, cached | Procedure AOT | Cached-native / AOT |
| --- | ---: | ---: | ---: |
| One A per host entry | 87.924 ms | 9.284 ms | 9.470x |
| 64 A invocations per host entry | 91.608 ms | 7.329 ms | 12.499x |

Uncached native medians were 140.716 ms and 151.041 ms respectively. All runs
matched GPR/flag results and their expected work counts. Grouping reduces host
entries but also changes generated function structure; the timing difference
does not isolate an exact host-call cost. Source:
`target/static-procedure-benchmark.log`.

```sh
cargo run -p doslet-wasm-aot --example check-procedure --release --locked --offline -- --bench
```

These speed ratios cover register-only synthetic execution. Generated code
performs no guest stack stores, while the reference executes real CALL/RET;
full guest-memory equivalence is not asserted. Interpreter-side binding
validation, stack
materialization, interpreter integration, device time, memory-heavy workloads,
real ROTT procedures and browser execution remain outside this measurement.

### Direct-memory procedures with internal branches

The current generator writes one `procedures.wasm` file. Its
`doslet.procedures` custom section contains version 2 procedure metadata,
including source bytes, bindings and the SHA-256 of the module with that whole
custom section omitted. The native loader validates the section, hash and
memory ABI before installing any procedures. Calls still use the same shared
RAM, VRAM and page-generation backing; embedding metadata does not change the
execution ABI. Existing version 1 `procedures.json` plus sibling WASM bundles
remain loadable. Use a fresh output directory when generating a single-file
bundle so an old sidecar is not mistaken for current output.

The native desktop now has an opt-in `wasm-procedures` test path. It installs
the prepared module into the actual Session and replaces its RAM backing with
one fixed Wasmtime memory shared by interpretation, devices and AOT. Existing
RAM, page generations and physical VRAM migrate once at installation; their old
allocations are released. No RAM, VRAM or generation table is copied on a
procedure call. Runtime bindings are not serialized into
portable states, so the desktop reinstalls the selected bundle on snapshot
restore and restart. The first native test explicitly disables speed-tier
labels; their per-invocation accounting remains a separate integration step.
Ordinary interpreter batches borrow RAM, page generations and physical VRAM
once, together with the VGA controller. They access RAM directly and preserve
normal VGA byte operations without stopping the batch at each pixel. A20
wrapping, instrumented accesses and boundary-crossing operands use the existing
PC bus. No borrow survives an AOT call.

Prepare the local ROTT module from the matching flat32 binding declarations:

```sh
mkdir -p target/rott-aot
cargo run -p doslet-headless --features wasm-static-test --example prepare_memory_procedures --release --locked --offline -- games/ROTT.toml target/static-rott-layout/current-bindings.json target/rott-aot-embedded-before-irq --computation-kernels --kernel-interrupts before-irq
cp target/static-rott-natural-wait/initial.state target/rott-aot/start.state
cargo build -p doslet --features wasm-procedures --release --locked --offline
sh tools/run-rott-aot.sh
```

These historical bindings were captured without a DPMI host (object 1 at
0x18e000), so `tools/run-rott-aot.sh` launches with `--experimental-dpmi off`.
`capture_procedure_layout` now enables DPMI Direct by default, as the desktop
does, where object 1 starts at 0x1e4000; pass `--dpmi off` or `--dpmi reflect`
for other launches. The capture records the mode in `bindings.json`; the
generator copies it into the module metadata, and loaders reject a module
recorded for another mode. Bindings without the field are treated as `off`.
States cannot yet hold the DPMI host, so such captures write no `start.state`.

Without `OUTPUT_DIR`, the generator writes `procedures.wasm` to the user cache
under `aot/<executable-sha256>/<layout>/`, where `<layout>` is the key of the
bindings' layout (`le_layout::Layout::key`: object bases and a digest of their
selectors). The desktop finds the live layout itself: `apps/common/le_layout.rs`
probes the running session every million steps of flat 32-bit code, searches
page-aligned candidates for 32-byte relocation-free anchors of the image
(every offset on one probe in 32), and binds every object as the offline
capture does. It then installs the cached module of that layout, or compiles
one on a background thread with the
`--computation-kernels --kernel-interrupts before-irq` recipe shared through
`apps/common/procedure_cache.rs`, and installs it at the next batch boundary
without recorded RAM. The first 10,000 calls decide: an average below 32
instructions suspends the procedures and leaves `unavailable.txt` beside the
module. `--no-aot` disables this. For ROTT the live module's code equals the
offline module's byte for byte; it compiled in 0.1 s and averaged 702
instructions per call. HEROES.EXE first calls procedures after about 90
million steps and averages 7 instructions per call; its module ran 20% slower
than the interpreter over 40 guest seconds (7.0 s against 5.9 s), so it is
suspended. Under DPMI Direct from boot,
the regenerated 202-entry ROTT module ran steps 60–100 million in 287 ms against
353 ms interpreted (1.23×). AOT executed 15% of those instructions in 8,992
calls, and both runs ended at the same PC and guest time. The desktop stores the module's native
compilation in the user cache, so later launches load it instead of compiling;
see [host directories](../configuration.md#host-directories).

The bundled checkpoint is local test input at the known loaded addresses,
not a new game save. The existing window, input, audio and game UI are used.
Additional native flags can be appended, for example `--midi-output PORT`.
The terminal reports module installation and the first actual AOT execution;
loading a module alone does not establish executed coverage. The original
launch command without `--wasm-procedures` retains the normal RAM owner.
Do not redistribute the local game bytes, checkpoint or generated code.

Native presentation uses the selected RAM/VRAM owner for both scanout and
video-mode metadata. A stale `Session::video_mode_label` call to the VGA chip's
old, released backing caused a startup panic after migration even though the
headless execution comparison passed. Runtime presentation tests now exercise
text, mode 13h and VBE 101h after installing a real module; a native regression
also calls the actual `Doslet::snapshot` path. Both reproduced the original
zero-length VRAM assertion before the fix and pass with the borrowed backing.

The same native Session path has a bounded differential replay tool:

```sh
cargo run -p doslet-headless --features wasm-procedures --example check_native_procedures --release --locked --offline -- games/ROTT.toml target/rott-aot/start.state target/rott-aot-embedded-before-irq/procedures.wasm 2000000
```

It requires actual translated work and compares complete portable state,
including RAM/VRAM, CPU/device state and guest clocks, plus drained PCM and
MIDI. Its single-pass timing is diagnostic, not a warmed speed comparison.
Append `--bench` after a step count for two preliminary and seven measured
replays with alternating order. Each replay restores identical guest state;
module preparation/compilation, save-state encoding and audio hashing are
outside the timed `Session::advance` calls. Every replay must pass equality.
This measures native emulation execution, excluding the window/audio host UI.
VGA wait acceleration is enabled in both paths by default, matching normal
native execution. Use `--no-vga-waits` to reproduce the raw instruction timings
recorded below. Use `--reference-no-vga-waits` to compare accelerated waiting
against actual polling in the reference; this checks the combined wait/AOT
path and must not be reported as an AOT-only speedup. Every run prints both
wait policies. The asymmetric state comparison normalizes the saved policy
after execution; all guest/device state and drained audio remain compared.

The native owner retains the encoded memory descriptor between calls. If its
validated inputs are unchanged, entry writes the CPU frame and resets only the
effect counters and work grant. Changes to segment permissions, bounds, VGA
banks or controls still replace and validate the descriptor. The PC caches
segment windows using the live selectors, hidden caches and materialized PE
mode; reloading a descriptor with the same selector invalidates that snapshot.
Export IDs resolve to checked instance indices during installation. No native
RAM borrow or pointer survives entry into Wasmtime. These changes retain memory
ABI version 2, so existing compiler output remains usable.

The isolated shared-owner call benchmark excludes compilation, allocation and
the PC's source checks, hardware scheduler and call-frame construction:

```sh
cargo run -p doslet-wasm-aot --example bench-shared-call --release --locked --offline -- --indexed
```

It performs two warmups and seven measured samples of 200,000 calls for each
body, checking registers, return state, work and effects on every invocation.
Use the native Session replay above to measure integrated game execution;
call-boundary timings do not establish a game speedup.

### Semantic procedure kernels

`--copy-kernels` enables a separate whole-procedure lowering pass. The first
recognized idiom copies contiguous source bytes to a strided planar VGA
destination. Recognition checks the complete IR and branch topology; it uses
neither executable names nor game addresses. `--kernels-only` omits ordinary
instruction-by-instruction procedures from the generated bundle.

```sh
cargo run -p doslet-headless --features wasm-static-test --example prepare_memory_procedures --release --locked --offline -- games/ROTT.toml target/static-rott-layout/current-bindings.json target/rott-aot-kernels --max-work-per-call 4096 --hot-profile "$HOME/.cache/doslet/speed-tiers/profiles/RiseoftheTriad-speed-tiers.json.learned.json" --profile-bindings target/static-rott-layout/historical-bindings.json --copy-kernels --kernels-only
```

The local image produces one 2177-byte module for the complete column copy at
object3+0xc83. Its effective body is `dst[stride * i] = src[i]`. The emitted
loop keeps source, destination, count and change totals in WASM locals. Full
RAM/segment, strided VGA and return-stack checks run before any effect. This
initial specialization accepts unchained mode-0 replacement writes with any
plane mask; unsupported mappings and zero counts decline before copying.
The source assembly copies one byte for count zero, so treating it as an empty
copy would be incorrect.

For count 200 the owner admits one complete 710-instruction computation. Work
is derived from the live count and includes executed padding and RET. The
copy metadata retains its own 4096-instruction bound. The complete computation
may finish past a smaller caller batch budget. There are no internal CPU checkpoints or flag updates per
pixel; final registers and arithmetic flags are computed once at the boundary.
The five direct callers in this image overwrite the kernel's clobbered values
before use; the first kernel nevertheless preserves its cheap final register
results. This is an explicit assembly contract, not an assumed standard C ABI.

IRQ delivery may be delayed until return. After the call, the owner advances
devices through every elapsed sample/timer deadline rather than dropping audio
events in one oversized time step. This does not prove that delaying an IRQ
is harmless for every procedure or game. Interrupt-dependent RAM polling is
not an eligible computation. Source checks and page invalidation remain active.
The earlier precise per-instruction compiler remains available independently.

`--kernel-interrupts before-irq` optionally adds one admission check at the
procedure boundary: the complete live cost must fit before the next known IRQ.
Audio sample events do not split the computation. `--kernel-interrupts defer`
is the default and preserves the explicit delayed-IRQ experiment. Neither mode
polls IRQs inside the WASM loop.

The native `--interpret-procedures` flag uses the same bundle to mark bounded
interpreter invocations. It executes the original x86 through the cached
interpreter, retains the original RAM/VRAM owners and performs no WASM
compilation. IRQ delivery waits until the admitted invocation returns. Both
engines use the same live work count, source-byte guards, context/memory checks
and whole-computation policy. Mixed bundles mark only their semantic kernels
in this reference mode. Unadmitted calls, ordinary code and observed stepping keep
normal instruction-level IRQ behavior. Only finite semantic kernels are
eligible for this reference mode; arbitrary polling procedures are excluded.
`interpreted_calls` and `interpreted_instructions` remain separate from
translated work. The boundary registry is reinstalled from launch settings
after a native snapshot restore or restart, like the AOT registry.

To run the deferred-IRQ bundle in the normal window with either engine:

```sh
DOSLET_ROTT_PROCEDURES=target/rott-aot-kernels/procedures.json sh tools/run-rott-aot.sh
DOSLET_ROTT_PROCEDURES=target/rott-aot-kernels/procedures.json sh tools/run-rott-aot.sh --interpret-procedures
```

The original launcher default still selects the `before-irq` bundle.

Native PC tests compare complete calls for counts 1, 2, 3, 199 and 200 against
the actual interpreter, including RAM, physical VRAM, final GPRs/flags, RET,
device counts and clocks. Other tests cover current-code guards, completing
past a soft budget, count zero, allocation-free first entry and DMA/sample progression.

### Generic finite computations

`--computation-kernels` also selects a general control-flow strategy. It builds
a forest of natural loops, proves progress of their register counters, and
emits structured WASM loops and branches. It does not use game names, procedure
addresses, or a guest-PC dispatch loop. Proven loops contain no instruction
fuel, host callbacks or IRQ checks. Executed instruction counts remain exact.

The proof accepts independent byte/word wrap-to-zero counters and initialized
32-bit countdowns. It tracks register bit ranges: CH and CL can be independent,
while a write to CX overlaps both. Nested loops may temporarily use known-zero
upper bits of an initialized countdown when their normal exits restore those
bits to zero. Irreducible cycles, unproved counter changes, and loops waiting
only for an externally written variable decline this strategy.

One 32-bit induction loop per function can instead take its initial count from
live registers. A symbolic whole-function work bound derives its accepted trip
range, including branches and fixed enclosing loops. A single check on each
dynamic entry to that loop excludes zero/full-wrap and counts exceeding the
representable atomic-work ceiling. Refusal resumes at that header with the
completed prefix intact; accepted iterations contain no further count check
beyond the original guest loop condition. This ceiling protects work-count
representation and is independent of the host batch budget.

Single-element LODS/STOS lowering handles byte/word/dword operands, both address
widths and both DF directions. A failed complete memory-access proof exits
before its effects. REP retains its separate interpreter execution policy.

Discovery also considers ordinary continuations after unsupported setup
instructions and calls. A finite cyclic region can finish immediately before
an unsupported instruction and resume interpretation there. Loop-header
candidates can leave an enclosing loop to the interpreter. These entries are
reported as procedure/region entries; their count is not a count of recovered
source-language functions. Short acyclic prefixes ending at unsupported
instructions are excluded. Complete existing procedures remain eligible.

`finite_computation` metadata lists exact interpreter-exit/RET addresses and
live induction guards for the optional original-x86 boundary reference. The
owner validates them against source declarations. This metadata changes no
guest ABI and adds no per-call RAM copy. The existing soft host budget can be exceeded by a proved
computation; the owner services elapsed hardware events afterward.

The current private PRINCE conversion contains 387 procedure/region entries,
up from the retained 137-entry RLE-stage bundle; all prior entries remain.
Five use generic finite-loop lowering (three have a live dword guard), one
uses the RLE kernel, and the rest use ordinary lowering. The palette nearest-color
inner loop is now discovered automatically and runs 255 times in the saved
20-million-step window. It adds roughly 1.57 million translated instructions;
total AOT work is 6,141,595, or 30.71% of that window. The final release comparison
gave 105.547 ms native versus 71.136 ms AOT (1.484x), with two warmups and seven
measured replays, alternating order and all nine full portable-state/PCM/MIDI
comparisons passing against ordinary interpretation. Preparation, compilation
and state/audio hashing are excluded. Evidence:
`target/prince-finite-prepare.log` and `target/prince-finite-benchmark.log`.

Prepare the current generalized PRINCE module and verify the native checkpoint
with:

```sh
cargo run -p doslet-headless --features wasm-static-test --example prepare_memory_procedures --release --locked --offline -- games/PRINCE.toml target/prince-aot/bindings.json target/prince-aot-embedded --computation-kernels
cargo run -p doslet-headless --features wasm-procedures --example check_native_procedures --release --locked --offline -- games/PRINCE.toml target/prince-aot-vga-waits/start.state target/prince-aot-embedded/procedures.wasm 20000000 --bench
```

For ROTT, `target/rott-aot-finite` uses the deferred-boundary experiment;
`target/rott-aot-finite-before-irq` keeps whole calls before the next IRQ.
Generate the latter using the same computation flag plus
`--kernel-interrupts before-irq`, with `games/ROTT.toml` and
`target/static-rott-layout/current-bindings.json` as inputs. Deferred IRQ entry
still changes ordinary RAM compared with the unmodified interpreter in the
10-million-step ROTT window; comparison using matching interpreted procedure
boundaries passes. The before-IRQ variant passes against ordinary interpretation.
Unknown live dword loops currently reserve a conservative static maximum, so
this strict IRQ policy can refuse them even when their particular live count
would be small. Neither policy inserts checks inside accepted loop iterations.

The generic pass discovers palette-indexed strided-column rendering loops in
ROTT; its current module has 202 entries, three using generic guarded loops.
Its main fixed-point/transparent renderers still contain runtime-patched
immediates and remain outside immutable-source lowering. Discovery does not
establish that a particular checkpoint exercises every generated entry.
The strict before-IRQ 10-million-step replay gave 105.836 ms native versus
83.465 ms AOT (1.268x), with all nine full-state/audio checks passing. Its
3,627,127 translated instructions are still dominated by the existing copy
kernel; this measurement does not establish a gain from the newly discovered
palette-column loops. Evidence: `target/rott-finite-before-irq-benchmark.log`.
The native launch helpers now select the current PRINCE finite bundle and
the current ROTT before-IRQ bundle respectively. These are checkpoint/native
measurements; interactive gameplay and browser throughput were not tested.

### Whole RLE computations

`--computation-kernels` also recognizes a complete byte RLE procedure by its
operations, register/stack contract and full branch graph. It does not match
an executable name or a fixed address. Recognition permits grouped/split and
relocated IR while rejecting altered operations, extra instructions and
changed edges. The existing copy-kernel and ordinary lowering APIs remain
available independently.

Before entry the native owner proves a finite input prefix, checked segment
and ordinary-RAM spans, disjoint input/output/control/stack storage, protected
code pages and generation capacity. Zero-length run tokens are supported:
their consumed input advances the proof even without output progress. The
owner calculates exact original work in that pass. The generated loop uses
WASM locals and direct memory operations, without callbacks, IRQ polls or
instruction-budget tests. Page generations and original write accounting are
reconciled once after return, including unaligned DWORDs crossing pages.
Guest register results, arithmetic flags, stack shadow bytes and RET8 match
the original procedure. Entry proof and execution allocate no memory.

The initial contract accepts positive signed output counts, a 32-bit stack and
ordinary disjoint RAM. Calls failing that proof use ordinary interpretation.
The ordinary `--max-work-per-call` option does not split this kernel. Both
native and interpreted whole computations can finish past a soft batch budget;
hardware service follows the selected procedure-boundary policy.

Before generic finite-CFG lowering was added, the private PRINCE checkpoint
produced 137 procedures, including one RLE kernel, in a 1,842,289-byte module.
That historical bundle remains at `target/prince-aot-computations`; the current
generator additionally enables generic computations with the same flag.
Replay the retained RLE-stage artifact with:

```sh
cargo run -p doslet-headless --features wasm-procedures --example check_native_procedures --release --locked --offline -- games/PRINCE.toml target/prince-aot-vga-waits/start.state target/prince-aot-computations/procedures.json 20000000 --bench
```

The release replay performs 480 whole RLE calls replacing 4,565,682 original
instructions. Total AOT coverage is 4,569,882 instructions, or 22.85% of this
20-million-step window. With VGA wait acceleration enabled in both engines,
two warmups and seven measured replays gave native 111.637 ms versus AOT
82.198 ms (1.358x). Every replay compared complete portable CPU/RAM/VRAM/device
state, clocks, PCM and MIDI against the ordinary native interpreter. A separate
run with `--procedure-irqs` also verified the matching whole-call reference
policy. Compilation, preparation and state/audio hashing are outside timing.
Evidence: `target/prince-rle-original-irqs-benchmark.log` and
`target/prince-rle-native-benchmark.log`. These are checkpoint execution
measurements, not browser throughput or interactive gameplay validation.

Run the local checkpoint with the latest generalized computation bundle in the
native window with:

```sh
cargo build -p doslet --features wasm-procedures --release --locked --offline
sh tools/run-prince-aot.sh
```

The script leaves game data untouched and disables speed-tier labels and the
dynamic core for this experiment. `DOSLET_PRINCE_PROCEDURES` and
`DOSLET_PRINCE_STATE` can select another local bundle/checkpoint; extra CLI
arguments pass through. Do not redistribute these private artifacts.

### Earlier native copy measurements

The initial 20-million-step semantic-kernel run exposed a native integration
cost: with almost no AOT work, shared memory took 718.967 ms versus 218.643 ms
for the ordinary interpreter. Its borrowed RAM bus rejected VGA operands,
whereas the normal PC bus batches them. Each uncompiled pixel therefore
repeated admission, deadline calculation and scalar stepping. Borrowing VRAM
and VGA alongside RAM fixes that asymmetry. The repeated owner-only experiment
then measured 218.391 ms versus 225.737 ms reference, with all nine complete
states and audio hashes equal. These are separate runs, not a subtraction of
isolated costs. Evidence: `target/rott-semantic-owner-baseline-20m.log` and
`target/rott-semantic-vga-batched-owner-20m.log`.

With IRQ deferral, the semantic kernel runs 8938 complete calls covering
6345980 instructions. The final CPU registers/flags, visible pixels and
PCM/MIDI agree, but 67 ordinary RAM bytes differ and the full portable states
do not match. Both executions enter 266 IRQs. Delayed entry can change saved
interrupt context; these observations do not establish that every differing
byte is harmless. This run must not be described as exact game compatibility.

Using the same deferred procedure boundaries in the interpreter removes this
policy difference from the comparison. The 20-million-instruction replay then
passes full portable state and PCM/MIDI equality in all nine runs. Both engines
execute 8938 marked calls, covering 6345980 original instructions, with zero
side exits, source refusals or IRQ-horizon refusals. Two preliminary and seven
measured replays give medians of **224.090 ms for native interpretation** and
**162.787 ms for AOT**, or 1.377x throughput (27.4% less elapsed execution time).
This establishes agreement under the shared procedure-boundary policy, not
instruction-level IRQ timing equivalence with the unmarked interpreter or
interactive game compatibility. Evidence:
`target/rott-deferred-irq-interpreter-benchmark-20m.log`.

```sh
cargo run -p doslet-headless --features wasm-procedures --example check_native_procedures --release --locked --offline -- games/ROTT.toml target/rott-aot/start.state target/rott-aot-kernels/procedures.json 20000000 --bench --procedure-irqs
```

The variant with `--kernel-interrupts before-irq` passes the complete native
20-million-step comparison in all nine replays. Medians after two preliminary
runs are **225.304 ms for the cached native interpreter** and **173.490 ms with
the semantic kernel**: 1.299x throughput, or 23.0% less elapsed execution time.
The kernel performs 8913 complete calls covering 6328230 original instructions,
1782600 VGA byte writes and 983432 changed physical video bytes. There are no
side exits or source refusals; 25 calls decline at IRQ admission and 1859 at
the caller's remaining budget. Every full portable state and PCM/MIDI hash
matches. This is the native Session workload with compilation/setup excluded,
not a browser benchmark or interactive gameplay validation. The native launcher
selects this bundle. Evidence: `target/rott-kernel-before-irq-benchmark-20m.log`.

```sh
cargo run -p doslet-headless --features wasm-procedures --example check_native_procedures --release --locked --offline -- games/ROTT.toml target/rott-aot/start.state target/rott-aot-kernels-before-irq/procedures.json 20000000 --bench
```

Before direct VGA stores and loop support, the native ROTT replay passed at
both 2 million and 20 million instructions
with complete state and audio equality. The longer window reaches 65 AOT calls
covering only 195 translated instructions, with no side exits or source/budget
refusals. Its diagnostic times are 215.136 ms for ordinary interpretation and
682.152 ms with this experimental integration. This does not demonstrate a
whole-game gain: almost all work still runs in the interpreter, and the new
entry checks and backing path add overhead. The native window is built for
manual testing; this replay does not validate interactive gameplay. Evidence:
`target/rott-aot-session-replay-final.log` and
`target/rott-aot-session-replay-20m.log`.

The coverage diagnostic can additionally join the learned HOT profile to a
single-step replay. Supply the profile and its original address layout after
the step count:

```sh
cargo run -p doslet-headless --features wasm-static-test --example inspect_static_coverage --release --locked --offline -- games/ROTT.toml target/rott-aot/start.state target/static-rott-layout/current-bindings.json 10000000 "$HOME/.cache/doslet/speed-tiers/profiles/RiseoftheTriad-speed-tiers.json.learned.json" target/static-rott-layout/historical-bindings.json
```

Historical instruction bytes and executable relocations are checked before
carrying labels to current object addresses. The profile supplies labels, not
frequencies; counts come from this replay. Root body walks do not follow calls,
are capped at 4096 nodes and can overlap. They are candidate inventories, not
procedure safety proofs. Separately listed observed HOT signatures outside the
CFG or with runtime-patched source must not be treated as frozen code.

The earlier 10-million-step local checkpoint replay identifies these bodies;
the final column reflects the current backend:

| Object-relative candidate | Observed instructions | Share of requested steps | Current obstacle |
| --- | ---: | ---: | --- |
| object3+0xc83, column-copy body | 3637413 | 36.37% | Implemented semantic kernel, selected in the current kernel bundle |
| object3+0xc40..0xc6d, sampled-pixel loop | 1940094 | 19.40% | Missing CFG reachability and patched immediates |
| object1+0x1080, record-search body | 238678 | 2.39% | Direct calls, branchy search and IDIV |
| object1+0x617b7, RAM-fill body | 91499 | 0.91% | Multiple loops and conditional exits |

These counts cover the indicated bodies or loop instructions, excluding called
bodies. They are guest instruction counts rather than host-time shares. Names
describe the observed assembly; source symbols have not been established.
The copy body has 5124 observed entries; the search body has 33, and RAM fill
has 9. The RAM-fill entry sample has EDX=0 and EAX=0x237020. The column-copy
loop reads a word from ordinary RAM and stores its two bytes in VGA. Its first
sample has ESI=0x303ddc, EDI=0xa9e00, plane mask=1, sequencer memory mode=6 and
graphics mode=0x40: unchained addressing with write mode 0. The renderer loop
also writes VGA, and its two ADD immediates at object3+0xc42/+0xc4f differ from
the original executable while matching the learned profile.

The direct procedure path now translates VGA stores to shared physical VRAM
using the active VGA write pipeline. For unchained simple-replacement writes, each enabled plane
maps aperture offset `o` to physical byte `4*o + plane`. Other write controls,
latches and change revisions must retain the existing device semantics.
Controls are captured once per bounded procedure entry; admitted kernels
perform no port I/O. Loop work has a hardware-bounded grant and a precise
resumption point. The coverage diagnostic itself only discovers candidates.
Full discovery evidence and register samples are in
`target/rott-hot-candidates-10m.log`.

The prepared native HOT bundle selects 34 procedures from this image, including
the column-copy entry and its loop header. Selection requires verified HOT
labels on at least half of non-NOP source nodes; it does not assign a tier to
the other nodes. All emitted sources retain ordinary runtime code checks.
The first two-million-step replay executes 217602 translated instructions in
945 calls, preserving full portable state and audio. Prefix exits account for
630 calls, and 1099 insufficient grants return to interpretation. These exits
are expected bounded continuation, not replay of earlier stores. Evidence:
`target/rott-aot-vga-replay-2m.log`.

The complete native Session benchmark uses the same 20-million-step checkpoint
workload, two preliminary replays and seven measured replays, with alternating
execution order. The final medians are **215.034 ms for the cached native
interpreter** and **385.871 ms with AOT**, or 0.557x interpreter/AOT throughput.
Thus this remains about 79% slower overall. AOT executes 7342319 instructions
(36.71%), 2065421 VGA writes and 1142177 changed physical video bytes, with zero
source refusals. All nine full portable states and PCM/MIDI hashes match.
VGA control/backing/alias proofs are computed once per generated call; invalid
capabilities still refuse at the actual store after committing the preceding
prefix. The earlier per-store proof version measured 429.286 ms versus
226.328 ms reference in its own repeated run. These results do not isolate
the remaining costs between native interpretation over shared memory,
entry scheduling and generated execution. They establish working native VGA
and loop integration, not a whole-game performance gain. Evidence:
`target/rott-aot-vga-hoisted-benchmark-20m.log`, with the earlier version in
`target/rott-aot-vga-benchmark-20m.log`.

```sh
cargo run -p doslet-headless --features wasm-procedures --example check_native_procedures --release --locked --offline -- games/ROTT.toml target/rott-aot/start.state target/rott-aot-vga/procedures.json 20000000 --bench
```

The manifest identifies memory ABI version 2 explicitly. Regenerate older
local bundles when using the new compiler/runtime pair.

The separate opt-in `static_program::procedure_memory` compiler now imports
only `doslet_static.memory`. It uses the existing 72-byte frame and 384-byte
memory descriptor, keeps GPRs and EFLAGS in WASM locals across the entire
procedure, and flushes at one common exit. Its `memory_procedure_<id>(frame)`
export returns `COMPLETE` or `SIDE_EXIT`. Supported operations include the
existing scalar arithmetic, byte/word/dword ordinary-RAM loads and stores,
memory arithmetic, extensions, multiplication, LEA, PUSH/POP and real guest
RET/RET imm16. The descriptor's SS.B controls stack addressing independently
of the operand width. Ordinary writable RAM starts at linear address `0x500`
or above; low-memory and protected source-page accesses decline to the
owner. Scalar stores additionally accept VGA with explicit read/write mapping
descriptors, including chain-4, unchained and banked linear addressing, all
four write modes, masks, rotation, logic operations and latch merging. Actual
device accesses and changed physical bytes are returned for device/revision
accounting. Procedure source loads and read-modify-write instructions remain
ordinary-RAM-only. Memory accesses use inline segment, span and generation checks.

A refused access preserves all earlier register and RAM effects, identifies
the exact unexecuted guest instruction and charges only the completed prefix.
The owner resumes interpretation there; replaying the entry would repeat
stores. Source-page stores and exhausted generations decline before changing
memory. The owner must mark every compiled source page, provide a frozen-code
proof, keep backing and control tables disjoint and reserve the maximum work
before entry. Generated code does not repeat source validation, reserve grants
or poll IRQs. Actual completed work is returned once for owner accounting.

`ProcedureGraphProgram` additionally accepts finite, acyclic graphs with
conditional Jcc, direct JMP, joins and multiple returns. The compiler orders
blocks topologically and emits structured WASM branches, retaining local
registers between blocks. It has no internal runtime dispatcher or callbacks.
Jcc/JMP/RET each charge one actual guest instruction; synthetic subdivisions
charge none. `maximum_work` reserves the longest path while the frame records
the path actually taken. The default compilation declines cycles. The optional
`compile_with_work_limit` accepts one single-entry natural loop with a linear
body and JMP/Jcc latch, plus acyclic entry/exit paths. It emits a structured
WASM loop without a guest-PC dispatcher, checking the grant once per whole
iteration and precisely within the final partial iteration. The limit is
1..4096 actual instructions; the test bundle uses 256. Unresolved edges,
overlaps, unreachable blocks, oversized graphs and unsupported loop shapes
decline compilation. Internal guest CALLs remain outside this slice; the separate
register compiler retains its direct call-DAG support.

`wasm::memory_procedures::PreparedMemoryProcedures::from_bound_graph` supplies
an offline second pass from original, relocated x86 bytes to this IR. Roots
come from trusted CFG entries and resolved direct CALL targets. Bounded mode
also considers backward branch targets for resuming loop execution. The pass
rejects calls, default-mode cycles, unsupported loop shapes, overlapping decode
streams and unsupported instructions. Its first implementation uses one block
per original instruction and bounds a leaf to 256 source nodes. Input records,
root occurrences, scan work, output nodes and cumulative allocations have
separate limits. It emits source identities for later owner validation, not
live bindings or permanent speed-tier labels.

The native `doslet-wasm-aot::procedure_memory` adapter owns one Wasmtime linear
memory. Native reference execution borrows that same kind of backing, releasing
the borrow before any WASM call. No RAM copy occurs at a procedure boundary.
The adapter prevents retries after a trap because earlier stores may already
have committed. Installing it into a game Session requires the explicit native
bundle option above; it is separate from the register-only `procedure-calls`
hook.

The same optional CPU feature also exposes the separate generic
`procedure_calls::memory` registry and `Cpu::try_call_memory_procedure`.
A `MemoryProcedureBus` owns the shared memory and receives an immutable CPU
view to validate live segment windows and prepare the boundary frame. The hook
checks context, transient hazards and the maximum-work grant before entering.
It commits returned GPRs, flags and EIP, without pre-reading the return address
or performing another RET. Complete and partial results carry actual work;
partial results resume at the precise unexecuted instruction. Backend failures
and malformed results can follow guest stores and are fatal for that execution;
they are never permission to replay the entry. Active speed-tier labels still
decline until owner accounting is integrated. Ordinary `Cpu::step` and the
register-only ABI retain their existing behavior.

The `check-memory-procedure` executable compares independently encoded original
x86 against Cranelift Speed AOT. It exercises stack prologues/epilogues, actual
RET, mixed 8/16/32-bit operations and state-dependent RAM updates. Branch cases
take unequal-length paths through TEST/JZ, XOR/JMP or NEG before joining.
Validation compares every GPR, EFLAGS, EIP, all 64 KiB of guest RAM, every page
generation, store counts and actual completed work. Timings include frame
marshalling and the native-to-WASM entry/return. Compilation, instantiation,
reset and full-state checks are outside the measured interval. Each sample
warms 32 entries before timing; a complete warmup run precedes seven
measurements in alternating native/AOT order. Fixtures fit within the native
decode cache's instruction-count capacity; ordinary direct-map conflicts can
still occur. These are CPU procedure workloads, not whole-game or browser
measurements.

```sh
cargo run -p doslet-wasm-aot --example check-memory-procedure --release --locked --offline -- --bench
cargo run -p doslet-wasm-aot --features procedure-calls --example check-memory-procedure --release --locked --offline -- --integrated --bench
```

The integrated variant executes the real entering CALL in both implementations
and dispatches AOT through the generic CPU memory hook. It includes registry
lookup, context/window checks and CPU commit in the measured interval. It
compares complete CPU snapshots as well as full RAM, page generations, writes
and work; the entering CALL is counted separately from the procedure's actual
work. This remains an interpreter integration fixture rather than installation
of the registry into normal ROTT Session execution.

The final local release series used Wasmtime 45.0.3 with the existing Cranelift
Speed, backtracking register allocation and inlining settings. Each row times
32768 entries; values are medians of seven alternating measurements. The native
reference borrows its RAM slice once per batch, avoiding a Wasmtime memory
lookup on every interpreted memory access. The integrated AOT owner includes
its real descriptor-window checks, registry/context checks, frame exchange and
CPU commit.

| Workload | Maximum body work | Native cached | Integrated AOT | Speedup |
| --- | ---: | ---: | ---: | ---: |
| Memory, 1 cell | 12 | 7.887 ms | 4.717 ms | 1.672x |
| Memory, 8 cells | 54 | 38.731 ms | 21.667 ms | 1.788x |
| Memory, 32 cells | 198 | 141.153 ms | 76.981 ms | 1.834x |
| Memory and branches, 1 cell | 16 | 9.552 ms | 5.016 ms | 1.904x |
| Memory and branches, 8 cells | 86 | 53.299 ms | 15.736 ms | 3.387x |
| Memory and branches, 32 cells | 326 | 201.120 ms | 51.952 ms | 3.871x |

Maximum body work excludes the real entering CALL. Actual branch-path counts
and complete CPU/RAM state agree after every sample. Direct-entry measurements
of the same bodies, without the interpreter hook or entering CALL, give
1.942–3.256x for memory and 4.003–4.739x for memory with branches. Compilation is
excluded: the integrated series recorded roughly 5–404 ms of AOT compilation
and 0.05–0.34 ms of loading per fixture. This is separate setup cost, not part
of the warmed throughput. Evidence:
`target/static-procedure-memory-integrated-benchmark.log` and
`target/static-procedure-memory-branch-benchmark.log`.

The bound ROTT image now prepares 45 acyclic memory/branch leaves with 1089
source-node occurrences, compared with 16 straight-line leaves and 91 nodes.
They compile into one 511073-byte WASM module. All included instruction bytes
match the loaded checkpoint, and their code-page generations remain unchanged
over the subsequent two-million-step native replay. The remaining roots report
435 call rejections, 56 cycles, one instruction bound and 207 unsupported
instructions. These are eligibility and source-stability observations, not
procedure executions or a ROTT speed result. Evidence:
`target/static-rott-memory-branch-procedures.log`.

Safety analysis must extend transitively through resolved direct calls: start
with eligible leaves, then admit callers whose own operations and every callee
meet the same execution contract. Emit direct WASM calls within the admitted
group, passing proven inputs and results as WASM values without a runtime
register flush between calls. Removing guest call frames additionally requires
proof that return addresses and stack storage are not observed or exposed
through pointers; otherwise preserve the required guest stack effects. Unknown
targets, invalidated callees and unbounded recursive call cycles prevent atomic
admission. Bounds and external-wait exclusions apply to the complete call tree,
not just the number of instructions in each individual procedure.

The procedure returns completed work in bulk. The runtime charges that work to
the current invocation's speed-tier and actual interrupt context; the same
procedure can serve different callers without acquiring one permanent tier.
Straight-line costs can be constants and bounded loops can aggregate actual
iterations. Instructions already executed by the interpreter, such as the
entering CALL, must not be counted again.

**Known scheduling problem:** the proposed small computational pieces do not
poll for interrupts internally. The runtime advances device time and handles
pending interrupts after a piece returns. This may delay an interrupt that
would have occurred inside the original procedure. Record this timing
limitation for the initial experiment; precise interior interrupt delivery is
deferred. The existing ABI-v1 region backend keeps its event-free grant checks.
Small code size alone does not establish short execution: loops require
separate treatment.

The critical analysis requirement is to recognize **waiting for externally
changed memory** before treating a procedure as one uninterrupted computation:

```asm
wait:
    mov eax, [timer_ticks]
    cmp eax, edx
    je wait
```

The continuation predicate depends on a repeated memory read. If the loop and
its callees cannot write the observed bytes, progress requires an external
writer, commonly an interrupt handler. Comparing two variables must qualify;
the predicate need not compare memory with an immediate constant. Writes to
unrelated locals or repeated same-value temporary stores do not themselves
disqualify a wait. Analysis must account for complete access widths and possible
aliases; unresolved writes/calls leave the classification uncertain.

Such a loop must yield to the runtime while its waiting predicate holds,
allowing device time and interrupt handling to progress. It must not remain in
an atomic procedure until return, which could prevent its own wakeup forever.
Absence of a local writer is a wait candidate, not proof that a particular IRQ
will change the value or permission to skip useful work. Existing bounded
runtime recurrence/event checks remain the authority for actual idle skipping;
unknown writers or unavailable interrupts retain bounded execution/fallback.

Acceptance needs a real IRQ-updated wait that wakes correctly, variable-to-
variable comparisons, unrelated local stores, overlapping or unresolved writes,
and a useful computational loop that must not be classified as waiting. The
current wait-candidate implementation covers these predicate/alias distinctions
and has a synthetic BIOS IRQ0 wakeup test with full-state comparison. Integrating
those exclusions into complete procedure selection remains a separate gate.

## Implementation stages and review gates

Work is delegated one small task at a time. Each task receives its immediate
input contract and relevant files rather than the whole conversation. A reviewed
handoff is the input to the next task; implementation and independent review are
separate steps. Existing game files and unrelated worktree changes stay intact.

| Stage | Deliverable | Acceptance gate | Status |
| --- | --- | --- | --- |
| 1a | Borrowed LE discovery, object/page metadata and raw fixup views | Checked ranges, explicit format limits, malformed/ambiguous image tests, existing MZ behavior preserved | Reviewed |
| 1b | Typed fixup decoding | Validated record lengths, cross-page sources and explicit unsupported records | Reviewed |
| 2a | Symbolic executable image | Object-relative identities, explicit BSS, bounded allocation and canonical relocation sites | Reviewed |
| 2b | Offline relocation binding | Explicit load/selector bindings, width/overflow checks and application tests | Reviewed |
| 3a | Static control-flow analysis | Entry/call/branch traversal independent of profiles; unresolved targets, overlap and limits reported | Reviewed |
| 3b | Additional static targets and profile guidance | Proven relocation-backed transfers; profile hints never replace executable bytes or establish reachability | Partial: bound existing targets and verified tier guidance |
| 4 | One program WASM module | 8/16/32-bit semantics, direct region flow, soft budgets and optional uncounted HOT code | In progress |
| 5 | Shared-memory WASM runtime integration | One owner, direct runtime calls, dynamic-code fallback and bounded service points | Experimental counted register/MOV slice with direct RAM and VGA reads |
| 6 | Real-game validation and measurements | Recorded ROTT execution, appropriate state/audio checks and repeated target-specific measurements | Bounded startup windows; gameplay pending |

Each stage must preserve the no-Node, safe `no_std` library and static bus
boundaries. Parser/image code must reject malformed or unsupported input without
unchecked arithmetic, partial publication or silently invented semantics.
Synthetic tests, executable inspection, generated-module validation and game
execution are distinct evidence. Passing a foundation stage does not establish
the later compiler's instruction coverage or game compatibility.

`doslet-executable::LeImage` implements stage 1a without allocating or copying
the input file. Discovery resolves nested MZ origins, validates the supported LE
tables and rejects ambiguous candidates or exhausted inspection limits. LX,
non-little-endian headers and non-enumerated page encodings are explicitly
unsupported. An object's implicit BSS tail is retained in its virtual-size
metadata. Names and exports remain opaque.

`LeFixups` implements stage 1b as an allocation-free fallible iterator. Each
direct internal record is checked completely before publication, including
source lists and signed cross-page offsets. Errors identify the record and
field offsets; a shared decode budget can continue across pages. Imports,
entry-table targets, chaining and additive fixups remain explicitly unsupported.
The decoder preserves target offsets without treating them as evidence of a
valid memory or code target. It does not apply relocations or choose selectors.

Local read-only inspection covers three user-supplied executables:

| Executable | MZ origin | LE header | Data pages | Objects | Enumerated pages | Raw fixup bytes |
| --- | --- | --- | --- | --- | --- | --- |
| ROTT.EXE | `0x26654` | `0x290a4` | `0x5f254` | 4 | 168 | 219597 |
| HEROES.EXE | `0x352a4` | `0x37cf4` | `0x636a4` | 2 | 200 | 176739 |
| PRINCE.EXE | `0` | `0x2a50` | `0x28400` | 3 | 188 | 152182 |

All three complete record streams decode within the supported subset:

| Executable | Records/sources | Offset32 | SelfRelative32 | Selector16 | Negative sources |
| --- | --- | --- | --- | --- | --- |
| ROTT.EXE | 25219 | 25192 | 27 | 0 | 17 |
| HEROES.EXE | 20405 | 20405 | 0 | 0 | 5 |
| PRINCE.EXE | 18380 | 18365 | 0 | 15 | 12 |

These fixtures contain no source lists or alias records; those forms have
synthetic decoder coverage. Run the inspector on a local executable with:

```sh
cargo run -p doslet-executable --example inspect-le --locked --offline -- games/HoMnM/HEROES.EXE
```

The parser and decoder have 47 synthetic tests, including reordered LE page maps
whose fixup-table association uses the mapped physical page number. The existing
seven DOS MZ loading tests also pass. This evidence covers file inspection and
MZ compatibility, not relocation application or translated execution. The
executable bytes remain local; no game fixture is distributed with the tests.

`doslet-aot::SymbolicImage` implements stage 2a as an offline `no_std + alloc`
preparation layer. It reconstructs immutable objects, records each file-backed
span and zeroes implicit BSS. It retains symbolic relocation targets and source
provenance without choosing runtime addresses or selectors. Sources must fit
their object at the full write width. Equivalent cross-page records share one
canonical site; conflicting or overlapping writes fail explicitly. Requested
storage, source occurrences and complete-image fixup decoding have separate
limits, with fallible reservations before publication.

| Executable | Virtual bytes | File-backed bytes | Canonical relocation sites | Duplicate source occurrences |
| --- | --- | --- | --- | --- |
| ROTT.EXE | 1124358 | 680195 | 25202 | 17 |
| HEROES.EXE | 1123511 | 813006 | 20400 | 5 |
| PRINCE.EXE | 807963 | 765303 | 18368 | 12 |

The image layer has 21 synthetic tests. All three local executables reconstruct
successfully with default limits:

```sh
cargo run -p doslet-aot --example inspect-image --locked --offline -- games/PRINCE/PRINCE.EXE
```

These results validate reconstruction and canonicalization. Offline binding,
instruction discovery, generated WASM and game execution have separate gates.

`doslet-aot::binding::DosFlatBindingPlan` implements stage 2b for explicitly
declared load bases and selector views. It validates nonwrapping, disjoint object
extents, selector coverage and consistency, then preencodes every patch before
publishing a plan. The final linear byte `0xffffffff` is supported. A caller can
apply a complete object's immutable patches to an exact-size offline buffer;
invalid arguments leave that buffer unchanged. This neither chooses selectors
nor writes guest memory or validates live descriptor caches.

Supported fields are `Offset32`, `Selector16` and default-code32
`SelfRelative32`. Other field kinds and code16 self-relative fields fail
explicitly. Narrow and alias views currently require the entire object to fit
64 KiB. Seventeen binding tests cover encoding, limits, selector identities,
modular target addends and atomic output checks. Under explicitly declared
synthetic layouts, all three fixtures produce complete plans:

| Executable | Patches | Offset32 | SelfRelative32 | Selector16 |
| --- | --- | --- | --- | --- |
| ROTT.EXE | 25202 | 25175 | 27 | 0 |
| HEROES.EXE | 20400 | 20400 | 0 | 0 |
| PRINCE.EXE | 18368 | 18353 | 0 | 15 |

```sh
cargo run -p doslet-aot --example inspect-bindings --locked --offline -- games/PRINCE/PRINCE.EXE
```

These are offline declaration/encoding checks, not proof of a real loader's
layout or game execution.

`doslet-aot::cfg::StaticCfg` implements stage 3a using a private offline x86
decoder. It starts at the LE entry and any explicitly supplied trusted entries,
then traverses direct calls, branches and possible continuations without using
profiles. It decodes only executable file-backed bytes, including instructions
crossing adjacent backed spans. It does not sweep unknown bytes or treat pointers
as proof of code. Indirect/far transfers, runtime services and unbound 16-bit near
targets have explicit unresolved edges. A relocation that can change instruction
encoding stops that path; a relocated branch target remains unresolved.

Different reachable instruction starts may share source bytes. The graph retains
each start independently and records validated overlap pairs; consumers must
generate code from node identities and edges. Overlap detection scans at most
29 start positions per instruction and has its own count/storage limits. An
explicit strict policy remains available for consumers that cannot model overlap.
PRINCE exercises this through Watcom's `memchr` compiler intrinsic, whose
[instruction template](https://github.com/open-watcom/open-watcom-v2/blob/master/bld/cc/a/code386.asm#L921-L933)
uses a `TEST` immediate to consume the bytes of an alternative `MOV` path.

Read-only release inspection from the LE entry alone reports:

| Executable | Instructions | Instruction bytes, including shared bytes per instruction | Edges | Unresolved edges | Stopped paths | Overlap pairs |
| --- | --- | --- | --- | --- | --- | --- |
| ROTT.EXE | 48507 | 155506 | 56056 | 1066 | 1 | 0 |
| HEROES.EXE | 42163 | 138648 | 48896 | 714 | 0 | 0 |
| PRINCE.EXE | 87351 | 281909 | 100693 | 1391 | 0 | 2 |

ROTT's stopped path is `1+0x62cac` (`RelocatedEncoding`). PRINCE's overlap pairs
start at `1+0x9ad88` / `1+0x9ad8a` and `1+0x9adbd` / `1+0x9adbf`.
These discovered nodes all have default code32 mode; operand widths are separate.
The image/CFG layers have 55 synthetic tests. Counts describe conservative static
discovery, not actual execution frequency, complete program coverage or gameplay.

```sh
cargo run -p doslet-aot --example inspect-cfg --release --locked --offline -- games/PRINCE/PRINCE.EXE
```

`doslet-aot::regions::RegionPlan` partitions discovered instruction identities
into bounded straight-line regions. Only a unique resolved sequential edge can
stay inside a region. Joins, branches, calls, services and explicitly supplied
boundaries split regions; boundaries cannot add undiscovered instructions.
Overlapping byte streams remain independent. Seven tests cover grouping,
cycles, shared tails, the Watcom pattern, stopped paths and resource limits.
PRINCE produces 20455 regions at the default 64-instruction bound. This is a
partition of conservative discovery, before unsupported-operation and profile
boundaries; it is not a generated-code or performance result.

### Static program module

`doslet-x86-wasm::static_program` provides a separate generic CPU IR and ABI;
it does not depend on LE, DOS or a machine. `doslet-aot`'s `wasm` feature lowers
discovered instruction identities and explicitly bound relocation operands to
that IR. A patched 15-byte stack window supplies decoding; neither object/RAM
copies nor profile windows supply the executed code. Bound near16 and relocated
transfers can target existing discovered starts or explicit fallback stubs.
Unsupported instructions stop before their effects. This slice supports register
MOV, arithmetic/logical operations, unary operations, XCHG, NOP, direct JMP and
all Jcc conditions with byte/word/dword aliases. LEA computes 16/32-bit effective
offsets with word/dword destinations, preserves flags and ignores the segment;
it requires no direct-memory option or descriptor. Register shifts and rotates
support one, immediate and CL counts; SHLD/SHRD support word/dword operands.
MOVSX/MOVZX support byte/word register or guarded RAM/VGA sources and word/dword
destinations, preserving flags. Memory access bounds, latches and read counters
use source width; byte/word extension never reads a dword merely because its
destination is 32-bit. CBW/CWDE reuse pure register sign extension.
Two- and three-operand IMUL support word/dword registers and guarded RAM/VGA
sources. Operands are captured before writing aliases; only CF/OF change, based
on whether the signed product fits the destination width.
One-operand MUL/IMUL support byte/word/dword register and guarded RAM/VGA
sources, retaining the complete product in AX, DX:AX or EDX:EAX. Input aliases
are captured before either output is written; only CF/OF change.
Word SHLD/SHRD with CL remains unsupported because the static region cannot
prove its count is supported before execution. Immediate masked counts above 16
also remain unsupported, matching the reference CPU's explicit execution limit.
Optional direct-memory lowering
handles MOV loads, register-destination ALU and memory CMP/TEST from RAM/VGA.
Ordinary RAM also supports MOV stores, ALU read-modify-write and INC/DEC/NEG/NOT,
including address-size and segment overrides. Register/immediate PUSH and
register POP support word/dword operands and independent live SS.B addressing.
Bound fullflat32 rel32 CALL lowers to one guarded PUSH of the actual linear
return EIP and an internal tail transfer, charging one instruction. Its
continuation remains a distinct entry. Memory shifts, VGA stores by default, other stack
operations, indirect/far or unresolved rel16 calls, far returns and strings
currently use the interpreter boundary.

With `CompileOptions::memory_write_helper`, isolated MOV stores retain the
direct RAM fast path and may call `doslet_static.memory_write(frame, linear,
width_bytes, value) -> i32` after successful inline admission. The runtime
exports `static_memory_write` directly to that import: 1 means accepted; 0
means no effects. The helper uses ordinary bus stores for complete RAM/VGA
spans without interpreting an instruction, synchronizing GPRs or advancing
guest time. It commits prior direct effects before the bus access and refreshes
the memory descriptor afterward. Pending reservations accumulate until the
normal boundary; decline rolls back only the current reservation. Mixed
RAM/VGA spans remain interpreter boundaries. Preparation keeps this experiment
off by default; `compare_static_game --memory-write-helper` enables it and
records the setting and successful `memory_write_helpers` separately from
ordinary callbacks.

Bound near RET/RET imm16 supports word/dword EIPs and independent SS.B. It
validates the actual stack read before effects, then pops the EIP and adjusts
SP/ESP by operand bytes plus the unsigned imm16. The additional adjustment does
not access that discarded memory. Immutable linear aliases dispatch to existing
compiled entries with the original target guards; unknown destinations delegate
after RET completes. Soft budgets and structural service quotas still apply.
The application requires its declared flat32 source CS to match the live
protected, unpaged CS before generating linear aliases.

Large dispatch tables are partitioned into internal functions of at most 8192
entries, selected by a balanced range tree within the same module. Existing
region function IDs, imports and target admission remain unchanged; small modules
retain their previous bytecode. A regression with 40000 regions and 40000 aliases
including RET validates and executes a 29383416-byte module, with ten helpers
and a largest function body of 1261593 bytes. `dispatcher_bytes()` includes all
these helper bodies. This avoids the engine's per-function parsing limit without
raising the 64 MiB module limit.

The game adapter uses binding-aware traversal for full flat32 entries. Reached
relocated CALL/JMP/Jcc targets and sequential continuations may cross object
boundaries, including file-backed data32 objects reached under the source CS.
Relocations or pointer tables alone do not establish code roots. Other CS
contexts stop explicitly in this mode; the existing unbound code16 API remains
available. Lowering uses the proven bound edges and requires the exact plan
borrowed by the graph. Historical profile layouts remain guidance inputs only.

The generated module imports memory and one synchronous runtime function.
Known region edges use direct WASM tail calls, retaining the eight GPRs and
flags as parameters. Only initial entry and callback return use a static address
dispatcher. This API requires the WASM tail-call extension. Its 72-byte frame is
documented in `static_program::abi`; ABI-v1 remains unchanged. Counted regions
charge completed work once and saturate the soft budget at zero after bounded
overshoot. HOT regions can omit those charges and mark total work unavailable.
An independent region quota forces runtime service even in HOT/zero-work loops.
Optional runtime admission adds a separate direct WASM import before nonempty
regions. It reserves a whole region's hard event-free grant before effects;
refusal enters the interpreter. The live frame may be stale during admission,
so guards inspect immutable source/context proofs and reservation state only.
The compiler rejects admitted uncounted HOT: its independent device-time policy
remains incomplete. Finite-loop accounting hoisting remains future work.

The diagnostic game adapter enables direct memory and inline admission together.
A 384-byte descriptor follows the unchanged CPU frame, pointing into the actual
runtime-owned RAM, generation tables and VGA backing storage. There is no runtime
call per load/store or admitted region: a shared helper inside the program module
checks source generations and reserves a device grant. Direct-memory regions contain
one instruction so refused segment, backing or permission checks precede every
effect and reservation. RAM stores refuse compiled source pages and immediately
increment each distinct touched page generation. VGA reads use actual banked or
planar bytes, implement chain4/plane selection and read-compare, and retain latch
effects. Callbacks and final returns commit aggregated reservations, RAM-write
counts and VGA latch/access effects before ordinary runtime execution. Direct
views require A20 enabled and no active RAM journal/write log; other cases fall
back before effects. No framebuffer conversion or RAM/VRAM copy supplies these
accesses. Descriptor offset 44 declares stack capability as disabled, 16-bit or
32-bit; its default disabled value refuses stack execution before effects.
PUSH/POP use ordinary RAM only, preserve flags, and apply SS.B independently of
code, operand and address-size prefixes. The compiler's old option-off path
remains available.

When an inline source guard becomes stale, the owner can revalidate its exact
source bytes and retry the same static entry without interpreting its first
instruction. Both previous and current device grants must cover that region.
Proof renewal permits only one retry: another unchanged memory refusal follows
ordinary interpretation. Changed source bytes, unsupported entries and IRQ
boundaries retain their ordinary runtime paths.

Optional offline boundaries isolate immediate operands and likely mutable
instructions. Reachable absolute MOV stores and mismatched profile observations
can identify partition boundaries; neither supplies replacement executed bytes
or additional code roots. Every resulting region retains exact relocated source
validation. Partitioning can increase module size and does not imply a speedup.

A self-contained guest fixture exercises the shared runtime without game assets:

```sh
cargo build -p doslet-static-game-probe --target wasm32-unknown-unknown --release --locked --offline
cargo run -p doslet-headless --example check_static_memory --features wasm-static-test --release --locked --offline
```

The fixture sets mode 13h, seeds VGA storage, enables A20 and enters protected
mode through guest instructions. It checks chain4, selected-plane and read-compare
reads together with byte/word/dword RAM stores, page crossings and register aliases.
Each VGA case runs with both 16-bit and 32-bit SS descriptors selected by guest
instructions. The 49-instruction body includes word/dword PUSH/POP, signed
immediates, SP/ESP aliases, a direct near CALL, and a complete CALL/RET round trip
through a static linear alias, LEA, MOVSX/MOVZX, CBW/CWDE and register/RAM/VGA
IMUL, MUL AH, IMUL DX and implicit dword RAM MUL. Each run records twelve VGA byte reads and eighteen
RAM page writes, including seven stack-page updates.
It requires every measured instruction to execute in generated WASM with zero
callbacks, checks staged effects and live page generations, and compares complete
saved state with both WASM and native interpreters after reconciliation.
Another 24 cases enable memory-write helpers across the same VGA/SS combinations
and write modes 0 through 3, with set/reset, logical XOR and partial bit/plane
masks. Each executes 54 translated instructions and five memory helpers, with
zero ordinary callbacks or interpreted steps. The suffix covers VGA stores of
all three widths, a low-RAM store and a same-value compiled-source-page store.
Every case compares the complete saved state with both interpreters.

CPU compiler tests execute actual WASM and compare flags/register aliases,
boundary/random arithmetic and mixed region sequences against the interpreter.
Adapter tests cover original EXE decoding, relocated operands, code16
binding, direct loops and fallback before memory effects. The earlier
register-only conversion under explicitly synthetic layouts produced one
validated module per local fixture (these counts exclude direct-memory lowering):

| Executable | Register operations | Direct transfers | Interpreter boundaries | Regions | WASM bytes |
| --- | --- | --- | --- | --- | --- |
| ROTT.EXE | 17159 | 5377 | 25971 | 39433 | 17359988 |
| HEROES.EXE | 12456 | 5346 | 24361 | 35686 | 15489001 |
| PRINCE.EXE | 30079 | 10970 | 46302 | 70957 | 31290942 |

These are static node counts, not execution frequencies or runtime hit rates.
The example validates with Wasmi and optionally writes a new local artifact:

```sh
cargo run -p doslet-aot --features wasm --example translate-exe --release --locked --offline -- --synthetic-layout games/PRINCE/PRINCE.EXE
```

The native test adapter `doslet-wasm-aot::static_program` runs this ABI through
Wasmtime. `shared_runtime` additionally links a WASM runtime's exported memory
and function directly into the generated module, avoiding the prototype's Rust
trampoline and scratch copies. Six new native tests cover soft budgets, HOT,
callback mutation, instance isolation, direct shared-memory WASM callbacks and
stopping after an error. Additional admission tests cover hard grants and typed
imports. The `doslet-static-game-probe` acceptance app also compiles the complete
Rust Session/interpreter/devices to WASM. `apps/common/static_session.rs` owns
that Session exclusively, with bounded immutable code metadata and version guards.
Callbacks reconcile completed register work before ordinary device/IRQ/fallback
execution; every generated return requires final reconciliation, including a
direct soft-budget exit. Stale or modified source bytes refuse the affected
region. Immutable version proofs do not cache translated code.

A local ROTT module precompilation with Wasmtime/Cranelift Speed, Backtracking
allocation and Inlining=Yes produced a 75264424-byte native artifact from
17359988 WASM bytes: 5.835643 seconds compilation and 0.021463 seconds loading.
This measures offline preparation, not warmed execution or game speed:

```sh
cargo run -p doslet-wasm-aot --example precompile-wasm --release --locked --offline -- target/aot-static-rott-v1.wasm
```

### Profile guidance and warmed CPU acceptance

`doslet-aot::profile` parses native standalone/learned speed-tier JSON under
input, node, identity and owned-storage bounds without building a JSON value
tree. Native `fast`/`slow` and semantic `hot`/`cold` names retain their tiers.
Binding requires an existing CFG start, matching code mode, explicit loaded
object base and exact instruction bytes including relocation values. Duplicate
conflicting bytes/tiers stay GREY; IRQ observations remain distinct and counted.
Identity strings are surfaced; callers can require an exact expected identity.
Neither labels nor weights add code or override executable bytes.

The guided lowerer preserves per-instruction tiers/weights and splits tier/context
transitions. Only verified HOT becomes uncounted, and only after explicit opt-in.
The diagnostic accepts `--profile JSON`, `--expected-identity ID` and
`--uncounted-hot`. A real 1526-node ROTT profile parses successfully; its PCs
do not match the example's synthetic layout, so it correctly admits no labels.
Runtime-observed load bases must be supplied before those labels can bind.

The user's separate learned ROTT profile contains 21110 observations. Its source
layout differs from the saved-state layout. Explicit historical load declarations
bind 9360 existing instructions with zero byte mismatches: 8786 HOT, 493 COLD and
81 IRQ. The remaining observations do not establish new CFG roots. A guidance
rebind can carry those verified symbolic labels to another explicit binding of
the same immutable image; it cannot rebind a different image. The learned
identity is checked against SHA-256 of the base JSON. Offline selector
declarations still do not prove the presence or rights of live descriptors.

The Rust `doslet-static-probe` provides a separate synthetic mixed8/16/32-bit
register loop with the actual CPU interpreter compiled to WASM. Both reference
and generated code run under Wasmtime/Cranelift Speed, Backtracking and inlining,
with direct WASM runtime callbacks and shared memory. Setup, compilation/loading,
reset and verification are outside timed execution. A local five-repeat run,
after warming each case, measured 500000 iterations / 2500000 guest instructions:

| Path | Median | Range |
| --- | --- | --- |
| Interpreter WASM, cache off | 82.841 ms | 81.707–83.872 ms |
| Interpreter WASM, cache on | 24.411 ms | 24.102–24.761 ms |
| Static counted | 5.663 ms | 5.638–5.995 ms |
| Static HOT, uncounted | 5.593 ms | 5.572–5.733 ms |

Counted/HOT execution was 4.31x/4.36x faster than the cached interpreter in this
fixture. Every run matched all GPRs, EFLAGS, EIP and the complete 64-KiB guest-RAM
hash. Counted work was 2500000; HOT reported exact total unavailable. Both static
variants used 7813 direct callbacks (7812 services, one endpoint). Boundary cases
at 1/2/63/64/65 iterations and an independent register/flags oracle also pass.
These results do not establish game speed, device correctness or browser speed.

```sh
cargo build -p doslet-static-probe --target wasm32-unknown-unknown --release --locked --offline
cargo run -p doslet-wasm-aot --example compare-static-cpu --release --locked --offline -- target/wasm32-unknown-unknown/release/doslet_static_probe.wasm 500000 5
```

### Wait candidates and actual runtime acceptance

`doslet-aot::waits::WaitCandidates` finds conservative short loops independently
of profiles. Each candidate has one contiguous path ending in a conditional
backedge, a memory read and CMP/TEST supplying the branch flags. Discovery rejects
internal branches/joins, alternative entries, overlaps/stopped streams, calls,
port access, privileged instructions and unknown effects. Stores are allowed:
only runtime recurrence can establish that they leave RAM unchanged. Limits
bound input records, scan work, candidate storage and each body (at most 32
instructions/128 bytes). Twenty-four synthetic tests cover shapes, predicate
provenance, alias distinctions and limit failures.

Evidence distinguishes `ExternalMemory`, `LocalProgress` and `Uncertain`.
Bounded byte-level register provenance and local-store forwarding trace the
branch predicate. Disjoint temporary stores can preserve external-memory
evidence; induction variables, changing addresses and writes to observed bytes
show local progress. Unresolved pointer writes or segment aliases remain
uncertain. These labels do not identify an IRQ and never authorize skipping by
themselves.

The diagnostic `compare_static_game` accepts
`--event-waits off|static|external|cold` (default `off`). The `external` policy
tries both external-memory and uncertain candidates, excluding established
local progress and requiring no profile label. Runtime proof handles unresolved
aliases. The JSON report counts all three evidence categories before selection.
The `cold` policy requires verified COLD guidance for every
instruction in the candidate. A selected existing head becomes a zero-work
runtime stub. After flushing prior generated work, the actual interpreter starts
the existing CPU/RAM recurrence proof immediately, rather than waiting for its
periodic search. A slice accounts at most 65536 logical steps; only proven whole
passes are omitted, strictly before hardware events. No proof survives the return
to generated code. Counters distinguish actual interpreted work from proven
skipped steps. Neither selection nor proof changes the selected CPU MHz.

ROTT's released `CalcTics_` symbol and the
[original source](https://github.com/dos-games/vanilla-rott/blob/master/rott/rt_draw.c#L1506)
identify a five-instruction wait at LE1+`0x4101` through the JE at `0x4112`.
The complete function matches the local EXE uniquely after masking relocation
placeholders. The loaded body reads `ticcount`, stores/reloads a stack local and
compares `oldtime`. The historical profile marks all five instructions COLD and
the function entry HOT. The new static+COLD selector chooses exactly this head,
with a ten-step initial proof bound. These observations identify a candidate;
they do not establish idle execution.

Native runtime tests prove same-value stores in a BIOS-timer wait, preserve the
timer wakeup and compare complete saved state to ordinary execution. A changing
RAM loop cannot skip. An application-adapter test additionally verifies one wait
callback's frame/budget reconciliation and separate logical/actual counters.
Another adapter test derives the hint from the exact synthetic code that the
session executes: the loop stores/reloads a temporary, waits for BIOS IRQ0 to
change the tick count, and reaches its exit. Three 65536-step slices match the
ordinary interpreter's complete saved state while omitting proven idle passes.

On the local ROTT natural-wait checkpoint, the evidence pass finds 61 candidates:
four external-memory, 56 local-progress and one uncertain. Selecting only the
four proven predicates misses the reached wait; admitting the uncertain candidate
lets runtime recurrence resolve its aliases. A 2M-step correctness run with
`--event-waits external --memory-write-helper` selects five candidates, makes
43 wait callbacks and skips 422400 proven steps. Complete saved state and
PCM/MIDI hashes match the native oracle and WASM interpreter variants. Evidence:
`target/static-rott-external-waits-with-uncertain/report.json`. This one-repeat
diagnostic is not a new performance measurement, and the candidate counts are
not counts of computational procedures.

Actual ROTT startup windows at steps 20M–25M and 40M–45M match normalized saved
state, CPU/RAM/VRAM/scanout/time and output hashes with idle off/on, but record no
successful proofs or skips. Traces show active startup/drawing work and no entry
to this wait. A separate three-repeat 2M-step window under Wasmtime, starting at
20M, matches full saved state against a native oracle and both WASM interpreter
variants. It executes 32471 translated steps and 1967529 interpreter callbacks;
the selected wait is not reached. Digital PCM/MIDI are empty in these windows.

For this bounded startup window the static register slice remains slower than
the cached interpreter: median 357.262 ms versus 29.980 ms (uncached interpreter
163.582 ms). This exposes missing active-code coverage and the one-step fallback
cost; it does not establish a general game-speed gain. Local evidence is in
`target/static-rott-cold-waits/report.json`. Generation/AOT/reset, output draining,
hashing and serialization are outside timing; each measured path has one full
warmup. Production browser and interactive gameplay measurements remain pending.

The comparison requires explicit current binding JSON and, for historical
guidance, source binding JSON plus base/learned profiles. Each binding row declares
`object_number`, `linear_base`, `selector` and optional `alias_selector`; defaults
are not inferred from preferred bases. The local ROTT declaration uses an
unverified offline selector view for its unobserved 16-bit object. No translated
code16 region or selector fixup in this fixture depends on that view; live code32
admission remains independently checked. These fixture declarations are not a
production loader implementation.

A bounded natural-entry search subsequently reached the selected `CalcTics`
head after 29155616 additional interpreted instructions, at session step
49155616, without modifying guest registers or memory. In the following 100000
logical steps, ten callbacks executed 50 real interpreter instructions and proved
99950 skipped steps. Every full saved state matched native and WASM reference
execution. Three warmed repeats measured a median 0.033588 ms for this wait path
versus 1.440727 ms for the cached WASM interpreter. This is savings from proven
waiting, not evidence of faster translation of ordinary game code.

A longer 2M-step window crosses the wait's event boundary and continues into
other code. It matches complete saved state and output hashes while reporting
11980 translated steps, 1565620 actual interpreted steps and 422400 proven
skipped steps. The static path makes 1558135 callbacks. A direct native comparison
uses the actual Rust interpreter, Cargo release with thin LTO and one codegen
unit, identical checkpoints/drain boundaries and one full warmup per measured
pass. Three-repeat medians are:

| Path | Time for 2M logical steps |
| --- | --- |
| Native interpreter, cache off | 105.703 ms |
| Native interpreter, cache on | 17.653 ms |
| Native interpreter, cache on and ordinary idle proofs | 14.687 ms |
| WASM interpreter, cache on, under Wasmtime | 34.131 ms |
| Static WASM AOT with COLD wait hints | 270.322 ms |

Thus this AOT slice is about 15.3x slower than the cached native interpreter,
or 18.4x slower with ordinary native idle skipping. Only 0.599% of this window's
logical work executes translated operations. The one-instruction fallback and
missing active-code coverage dominate. Native idle also saves waiting work;
the wait-loop gain cannot be attributed to WASM AOT itself. AOT compilation,
checkpoint preparation, reset, drains and verification are excluded from these
execution times. Evidence: `target/static-rott-native-wake/report.json`.

After enabling direct memory and inline admission, the same executable lowers
6265 memory loads and 4296 stores, reducing static interpreter-boundary nodes
from 25971 to 15410. The complete module is 57214507 bytes. Replaying that same
2M-step checkpoint still spends most execution outside recovered static paths:
22285 translated steps, 1555315 interpreted steps, 422400 proven skipped steps
and 1547841 callbacks. Full saved states and audio hashes match in every repeat.
Three warmed repeats measure a median 438.921 ms for static WASM, 35.179 ms for
cached WASM interpretation, 18.929 ms for cached native interpretation and
16.271 ms for native cached interpretation with ordinary idle proofs. This
implementation adds correct direct accesses but does not improve this workload;
its repeated runtime transitions and descriptor preparation remain expensive.
In this earlier unbound traversal, the most active startup helpers in object 3
were outside the recovered CFG. Binding-aware traversal now reaches these paths.
Evidence: `target/static-rott-direct-memory-wake-final/report.json`. These are
Wasmtime measurements; browser and interactive gameplay performance is unmeasured.

Binding-aware traversal, shifts/rotates, direct memory ALU, PUSH/POP, near CALL/RET
and stale source-proof retry increase translated work in the same checkpoint to
1374387 steps, with 203217 interpreted and 422400 proven skipped steps. Callback
count falls to 235889. All three warmed repeats match complete saved state and
PCM/MIDI hashes. The 60354633-byte module has 44110 regions and 49348 discovered
instruction identities; 2210 identities remain interpreter boundaries.
The following medians exclude setup/AOT and have fallback diagnostics disabled:

| Engine | Median for 2000004 logical steps |
| --- | ---: |
| Static WASM AOT with COLD waits | 138.011 ms |
| Cached WASM interpreter | 34.728 ms |
| Cached native interpreter | 21.045 ms |
| Cached native interpreter with ordinary idle proofs | 14.868 ms |

The static path remains about 4.0x slower than cached WASM interpretation and
6.6x slower than cached native interpretation in this window. Native cached
samples range from 18.309 to 28.050 ms; static samples range from 134.908 to
139.418 ms. Program AOT compilation takes 4.372 seconds outside these timings.
Evidence: `target/static-rott-call-return/report.json`. An earlier instrumented replay
classifies 109212 callbacks as changed source, 82851 as residual memory/guard
refusals and 37621 as stale proofs; the latter can now retry the static entry.
These diagnostic counts are callback causes, not completed instruction counts.
Evidence: `target/static-rott-stale-retry-diagnostics/report.json`.
Pass `--diagnose-fallbacks` to `compare_static_game` to collect the seven
exclusive causes and separate service callbacks. This option defaults off,
survives replay resets, and marks instrumented timings in the report. It does
not renew proofs or alter execution policy while classifying a callback.

A subsequent LEA, extension and truncated IMUL run reduces static unsupported
identities to 1107, but translates only ten additional instructions in this
checkpoint: 1374397 translated, 203207 interpreted, 422400 skipped and 235881
callbacks. All repeats preserve full state and audio. Static timings are
192.042, 204.314 and 319.760 ms; cached WASM timings are 44.698, 50.777 and
74.668 ms. The wide variation in both paths does not establish a further speed
gain. Broader instruction coverage alone has little effect in this window.
Evidence: `target/static-rott-lea-extensions-imul/report.json`.

A controlled memory-write-helper experiment runs off/on/off, each with three
warmed repeats, the same runtime WASM binary, checkpoint and profiles. The two
off medians are 136.333 and 135.748 ms; enabling the helper measures 100.266 ms
(99.128–100.936 ms), about 26% less elapsed time. All runs preserve complete
saved state and PCM/MIDI hashes; the off modules also have identical hashes.
The helper executes 82851 stores without interpreting an instruction. Ordinary
callback count falls from 235879 to 161650; interpreted steps fall from 203205
to 120357, with 1457247 translated and 422400 skipped steps. Helper calls remain
WASM-to-WASM calls and are counted separately; their cost is lower because
they neither synchronize live GPRs nor run an interpreter/clock-accounting step.
Cached WASM interpretation in the on series measures 34.084 ms, so AOT remains
about 2.94x slower. These are Wasmtime measurements, not browser results.
Evidence: `target/static-rott-memory-helper-off/report.json`,
`target/static-rott-memory-helper-on/report.json` and
`target/static-rott-memory-helper-off-confirm/report.json`.

The read-only instruction inventory finds 94.130% of executed CPU instructions
already lowerable with matching source bytes, 5.461% with changed/unavailable
source and 0.171% known but unsupported. This is potential instruction coverage,
independent of live admission, device memory support or wait skipping; it is
not a translated-execution percentage. Evidence:
`target/static-rott-coverage-current.log`. This inventory precedes PUSH/POP lowering.

The same comparison on the isolated 100000-step wait measures native cached
execution at 0.708462 ms, native cached execution with ordinary idle proofs at
0.006313 ms, and static WASM AOT wait handling at 0.034626 ms (three warmed
repeats). Native idle proves/skips 99995 steps, versus the static wrapper's 99950:
the wrapper deliberately drops its temporary proof at each generated-code
boundary. Against native idle, AOT is about 5.5x slower even in this wait-only
window. Evidence: `target/static-rott-native-wait-only/report.json`.

```sh
cargo build -p doslet-static-game-probe --target wasm32-unknown-unknown --release --locked --offline
cargo run -p doslet-headless --example compare_static_game --features wasm-static-test --release --locked --offline -- --config games/ROTT.toml --state target/rott-wasm-fixture/initial.state --bindings target/static-rott-layout/current-bindings.json --profile-bindings target/static-rott-layout/historical-bindings.json --profile "$HOME/.cache/doslet/speed-tiers/profiles/RiseoftheTriad-speed-tiers.json.learned.json" --base-profile "$HOME/.cache/doslet/speed-tiers/profiles/RiseoftheTriad-speed-tiers.json" --event-waits cold --out target/static-rott-cold-waits --steps 2000000 --repeats 3
```

`--seek-wait N` optionally executes at most 100M additional instructions (and
60 host seconds) to capture a naturally reached selected head before comparison.
It injects no guest registers or memory. That search and the resulting initial
checkpoint are separate from the timed workload. Probe unit tests use
`RUST_MIN_STACK=8388608`; they include bounded metadata decoding with DSM1 input
compatibility and DSM2 wait hints.

## Existing profile-only prototype

`doslet-x86-wasm` translates observed x86 code into one WebAssembly module before
execution. It accepts the existing speed-tiers JSON profiles and, additionally,
native `--core-profile` files in DSJ1 format. The module dispatches between
translated blocks internally and calls the Rust host for interpreter fallback
and execution-budget boundaries.

This is an experimental compiler and host interface, with native and browser CPU
acceptance tests. Desktop launch options, the browser game Worker and
`Session::advance` do not select this core yet. The interpreter, native Cranelift
core, adaptive idle and speed-tier strategies retain their existing behavior.
The `compare_wasm_game` example supplies a diagnostic Session adapter with actual
device deadlines, IRQ/service fallback and complete-state comparisons.
An execution backend and a policy for controlling guest throughput are separate
choices. Profile tier names do not authorize skipping instructions or changing
time in this backend.

## Convert an existing profile

Run the converter from the repository root:

```sh
cargo run -p doslet-x86-wasm --example translate_profile --locked --offline -- path/to/speed-tiers.json target/game-core.wasm
```

The same command accepts a DSJ1 file instead of JSON. It reports compiled entries,
16-bit entries, conflicts, unsupported entries and entries omitted at capacity.
The output includes the source profile in a custom section. Keep artifacts local:
profiles and generated modules can contain proprietary guest instruction bytes.
The converter does not read or modify the original game directory.

JSON input follows the current standalone or learned profile layout: each `nodes`
entry supplies a numeric `pc`, `bits` and a 15-byte `bytes` signature. Optional
`version` and `learning_version` must equal 1. At most 100,000 input nodes and
64 MiB of JSON are accepted. Only observed instruction starts become entries;
the rest of each 15-byte lookahead window is not automatically treated as code.
Adjacent observed instructions can form a block. Duplicate conflicting signatures,
`reason: "code-conflict"` and `changed: true` remain interpreter work. The importer
accepts all tier labels without inferring runtime IRQ context from them.

At most 4096 entries are emitted, with at most 16 instructions per block.
`high_count`, or `count` when available, prioritizes admission. Profiles without
counters use address order. The report makes omitted coverage explicit.

## Implemented instruction scope

The compiler uses the existing execution decoder through the portable
`doslet_x86::translation` interface. It translates dword register MOV,
ADD/OR/ADC/SBB/AND/SUB/XOR/CMP/TEST, INC/DEC, XCHG, NEG/NOT and relative Jcc/JMP.
It preserves the interpreter's arithmetic-flag behavior, including incoming carry
for ADC/SBB and preserved carry for INC/DEC.

The current entry context requires protected default-32-bit execution with a
full flat executable CS, no paging, TF/RF/VM, interrupt shadow or active execution
labels. Real mode, 16-bit instructions, RAM operands, I/O, stack operations,
CALL/RET, REP, x87 and changes to segments or control state use the interpreter.
An unsupported instruction can return to translated execution afterward.

Compilation does not discover every code path or apply executable relocations.
Profiles identify observed loaded code. Blocks start at leaders: instructions
not reached by fallthrough from another supported instruction of the same tier,
plus local branch targets. Exact current instruction bytes must match before a
binding is enabled, and changed code falls back safely.

Addresses in a profile come from one layout. DOS4GW places ROTT's code object at
0x18e000 without a DPMI host and at 0x1e4000 under the built-in direct DPMI
host; the LE preferred base 0x10000 is used in neither. Static profiles declare
each code object as `relocatable: [{base, size}]` at its preferred base;
observed profiles without declarations form one implicit span over their hints.
The host resolves one live delta per span from code executing in it: it searches
the span's longest block around EIP and accepts a delta when at least half of
its 64 longest blocks match live bytes. Page-aligned deltas win over unaligned
copies of the image, such as a file buffer. Failed attempts back off. A delta
only chooses where blocks are looked for; bytes still guard every binding.

## Host contract and module ABI

`host::Runtime` keeps ownership of the CPU, RAM, devices, clocks and services.
The generated module imports the runtime's memory and the fixed address of a
65,600-byte scratch area holding register state and a bounded dispatch table.
It touches nothing else in that memory, and polls copy nothing.

`Host::poll(completed, remaining, memory)` first commits completed translated CPU
state, then calls `Runtime::account_translated`. It checks the next execution
context and asks `translated_budget` for a grant capped at 4096 steps. A
machine/session adapter must end that grant before an eligible interrupt,
hardware deadline, service interception, debugger boundary or external
code/mapping change. It must execute all of those effects through its normal
runtime path. Returning zero keeps execution on the interpreter. The PC session
also returns zero while a policy single-steps or acts at the current boundary:
an idle-loop proof, a known idle head or proven pass, an analyzer window or probe,
a synchronization burst or a parked menu. Between those, a grant is equivalent to
an ordinary batch, so the same wait loops are detected and skipped with AOT.

The dispatch table persists between polls. Each entry records its block, length
and live tier. The host tracks write generations per 4-KiB code page; before a
grant it revalidates only changed pages on EIP's page and its two neighbours, and
the module chains only into entries in that window. Self-modifying code therefore
costs one page check, not a table rebuild. Inside a grant, translated code only
changes registers, arithmetic flags and EIP. A whole block must fit the remaining
grant, and chained entries must have the grant's tier. Missing targets and
exhausted grants return to `poll`. Dispatch collisions that exhaust the
16-probe limit use fallback.

Fallback is batched. Block entries are registered as prepared CPU entries, and
`Runtime::interpret(budget)` runs ordinary batches until execution arrives at one.
An entry stops batches only while its grants pay for the stop. After 32 attempts,
an entry yielding fewer than 32 translated steps per attempt becomes cold; refused
grants count as zero. Cold entries are retried after 2^28 guest steps.

The module exports `run(budget: u32) -> u32`. It imports `doslet.memory`,
the immutable `doslet.base: i32` scratch address and
`doslet.poll(completed: u32, remaining: u32) -> i32`. Before returning, every
poll stores its interpreted steps at byte 44. Results are a nonnegative initial
block index, `-1` for another poll, or `-2` to stop. A final poll commits state
even when the run budget reaches zero. Only compiler-produced modules and their
matching host metadata are supported; the `doslet.abi` section is version 2.

The first eight scratch words contain EAX, ECX, EDX, EBX, ESP, EBP, ESI and EDI;
EFLAGS is at byte 32, EIP at 36, the grant remainder at 40, interpreted steps at
44, the grant tier at 48 (0xff without labels) and the validated window start and
length at 52 and 56. The dispatch table starts at 64. The browser worker keeps one
scratch allocation for its lifetime, so the base survives machine replacement.
`web/browser/wasm-core.js` validates ABI version 2 and rejects execution after a
trap. Bundles generated for ABI version 1 must be rebuilt.

`host::CpuRuntime` is a CPU/bus test adapter. It supplies no PC device clocks,
IRQ delivery or DOS service interception. Do not use it as a game session adapter.
The optional `native::Runner` uses Wasmi to execute the same generated bytes on
the host CPU. Wasmi is a testing interpreter, not a native JIT performance result.
The default compiler and host remain `no_std` with `alloc` and forbid unsafe Rust.

The separate native `doslet-wasm-aot` adapter uses Wasmtime 45.0.3 to precompile
the module before execution. It selects Cranelift `OptLevel::Speed`, backtracking
register allocation and explicit compiler inlining. `Speed` is the compiler's
highest speed optimization preset; the adapter preserves normal WASM checks.
One compiled artifact serves multiple instances. Two private unsafe loaders
accept native code: unchanged bytes just produced by the same engine's
`precompile_module`, and bytes returned by a host `CompiledCache`. The adapter
opens no files; the cache contract requires the host to return only bytes the
adapter stored for the same WASM source and engine fingerprint (Wasmtime version,
configuration and target). Wasmtime rejects incompatible code, which is then
compiled again. The desktop cache lives in the per-user cache directory and
checks a SHA-256 of each file; this detects corruption, not a local user able to
write that directory. Headless runners compile every launch. The diagnostic
writes `game.cwasm` for local inspection. Its contents depend on the native target
and engine configuration and are not a browser-loadable WASM module.

## Validate

```sh
cargo test -p doslet-x86-wasm --locked --offline
cargo test -p doslet-wasm-aot --locked --offline
cargo clippy -p doslet-wasm-aot --all-targets --locked --offline -- -D warnings
cargo clippy -p doslet-x86-wasm -p doslet-wasm-probe --all-targets --features doslet-x86-wasm/native-test --locked --offline -- -D warnings
cargo check -p doslet-x86-wasm --target wasm32-unknown-unknown --locked --offline
python3 tests/wasm_backend.py
```

The browser test requires Firefox, the pinned `wasm-bindgen` CLI and the WASM Rust
target. It builds `doslet-wasm-probe` with Cargo, generates bindings, and launches
an isolated headless Firefox against a temporary loopback server. Generated files,
logs and `result.json` stay in `target/`. No Node tooling is used.

Native differential tests compare full CPU snapshots and guest RAM against the
existing interpreter. They cover arithmetic boundaries and sampled values, all
Jcc conditions, cross-block loops, limited budgets, relocation, self-modification,
I/O/memory fallback, malformed input and profile capacity. The synthetic
two-block loop executes 4000 instructions with one grant and two host polls.
That callback count does not establish a speedup.
After warming the native WASM engine, an allocation regression also measures
repeated translated and mixed interpreter execution and requires zero allocations.
A synthetic deadline test commits elapsed steps and rejects code changed by the
host at that boundary; it does not establish PC device timing correctness.

The browser acceptance compares register state, flags, EIP, memory hashes and
step counts with the Rust interpreter. These are synthetic CPU tests. The ROTT
test below adds a bounded real-game comparison through Wasmi and native Wasmtime AOT.
Adaptive scheduling integration, gameplay coverage and browser performance
measurements remain separate work.

## ROTT real-run comparison

Run the local ZIP launch profile without publishing guest saves or changing
existing learned profiles:

```sh
cargo run -p doslet-headless --example compare_wasm_game --release --features wasm-core-test --locked --offline -- --config games/ROTT.toml --warmup 20000000 --train-steps 2000000 --steps 2000000 --repeats 3 --out target/rott-wasm-comparison
```

The example reads the shared TOML codec and ZIP adapter, uses a private write
overlay, disables competing accelerations for both owners, and captures a common
initial state. It records actual post-service CPU entries, excluding IRQ-only
steps, into the existing speed-tiers JSON layout. If an executable-hash cache or
explicit `--profile FILE` exists, it uses that input instead. Otherwise it uses
the newly observed JSON. Each comparison replays the observed window from its
initial snapshot; `--following-window` instead measures the subsequent window.
No source ZIP or cache is modified. Full CPU/clock, RAM/VRAM, PCM and MIDI checks
run between batches, and complete serialized states must match at the end.
Each instance first executes the entire window as a warmup. The example then
restores the guest snapshot while retaining that same engine instance, resets
host counters, and times a second pass. Both passes must match the reference.
Measurement excludes profile construction, module preparation and comparisons;
AOT generation and loading have separate timing fields. Repetitions alternate
which engine runs first in each batch.

Use the AOT compiler with the same launch profile and observed JSON:

```sh
cargo run -p doslet-headless --example compare_wasm_game --release --features wasm-aot-test --locked --offline -- --config games/ROTT.toml --engine wasmtime --warmup 20000000 --train-steps 2000000 --steps 2000000 --repeats 3 --profile target/rott-wasm-comparison/observed-speed-tiers.json --out target/rott-wasm-aot-comparison
```

For the same warmed comparison with Wasmi, use `--engine wasmi` and a different
output directory. `--warmup` controls guest initialization before observation;
the additional full execution warmup occurs once per instance for either engine.

The recorded local ROTT.EXE SHA-256 is
`2deedf54c45178792483114c017223407e3044abcc0863526aa477654a78569a`.
No matching cached profile was present, so this run generated its profile. After
20 million warmup steps, the measured two-million-step window displayed the
Earth intro. It exercised 1,994,496 default-32-bit and 5,496 default-16-bit CPU
instruction entries or REP iterations; eight remaining steps had no CPU entry.
This establishes intro execution, not interactive gameplay compatibility.

The shared decoder classified nonbranch operation widths as follows:

| Decoder width | Executed entries | Share of classified nonbranch entries |
| --- | ---: | ---: |
| 8 bits | 337,739 | 21.8% |
| 16 bits | 66,354 | 4.3% |
| 32 bits | 1,146,958 | 73.9% |

These are dynamic counts, not static code sizes or CPU-time attribution. They
exclude branches and 1,064 entries outside the shared cache decoder. Mixed-width
instructions such as MOVZX/MOVSX retain the decoder's primary width; the table
is not a count of every individual source and destination operand.

All three repetitions produced identical complete states and PCM/MIDI output.
The current backend translated 954,700 steps (47.735%) and interpreted 1,045,300,
with 420,202 grants and 1,465,702 host polls. A grant averaged only 2.27 translated
steps, unlike the synthetic register loop. The profile yielded 315 compiled
entries and excluded 23 conflicting signatures and 463 unsupported 32-bit sites.

A subsequent release comparison reused that exact JSON profile with one complete
warmup and one timed pass per instance, three repetitions per WASM engine. Both
warmup and measured passes produced identical complete states and PCM/MIDI.
Median execution times for the two-million-step window were:

| Execution engine | Median seconds |
| --- | ---: |
| Reference x86 interpreter, paired with AOT | 0.10965 |
| Wasmi, warmed WASM interpreter | 7.84299 |
| Wasmtime AOT, Speed + backtracking + inlining | 7.78953 |

Generating WASM took 0.324 ms; one AOT compilation took 18.124 ms and loading its
artifact took 0.175 ms, all outside execution measurements. AOT was still about
71 times slower than the reference. The difference between the WASM engines is
small compared with variation across repetitions; AOT alone does not establish
a useful improvement for this adapter. These are native results, not browser
JIT measurements.

Both engines still made 1,465,702 host polls and 420,202 grants. On each eligible
poll, the current host clears the 64-KiB dispatch table and scans the 315 compiled
entries to rebuild validated bindings, even when the next instruction eventually
uses fallback. This substantial repeated work remains outside the generated
WASM. Frequent transitions and validation need separate optimization; this
comparison does not isolate their individual timing contributions.

A separate three-repeat replay of the first million startup steps produced
999,947 default-16-bit entries and only 15 default-32-bit entries. No translated
steps ran there; complete states and audio matched. Both datasets show why
8/16-bit operands, 16-bit execution contexts and memory operations matter for
this backend. The existing scope remains unchanged by the diagnostic.

Local artifacts are `target/rott-wasm-comparison/report.json`, `measurements.json`,
`observed-speed-tiers.json`, `game.wasm`, `reference.png` and `wasm.png`. The initial
startup replay is under `target/rott-wasm-startup-comparison/`. Profiles, modules
and screenshots remain local and may contain proprietary game material.
The warmed Wasmi comparison is in `target/rott-wasm-warmed-wasmi/`; the AOT
comparison, including `game.cwasm`, is in `target/rott-wasm-aot-comparison/`.

## Complete interpreter under Wasmtime

`doslet-wasm-game-probe` compiles the complete Session, interpreter, PC devices
and diagnostic WASM host to `wasm32-unknown-unknown`. It has no JS or WASI imports.
The native driver loads a common saved state and immutable ZIP resources, then
compares the WASM interpreter with and without the decode cache and the translated
backend whose fallback and validation also execute in WASM. Both modules use
the same Wasmtime engine and AOT optimization settings described above.

Prepare a local fixture without running the earlier native-host measurements:

```sh
cargo run -p doslet-headless --example compare_wasm_game --release --features wasm-aot-test --locked --offline -- --config games/ROTT.toml --warmup 20000000 --train-steps 2000000 --out target/rott-wasm-fixture --prepare-only
cargo build -p doslet-wasm-game-probe --target wasm32-unknown-unknown --profile web --locked --offline
cargo run -p doslet-headless --example compare_wasmtime_game --release --features wasm-aot-test --locked --offline -- --config games/ROTT.toml --fixture target/rott-wasm-fixture --steps 2000000 --repeats 3 --out target/rott-wasmtime-interpreter
```

The fixture contains `initial.state`, `observed-speed-tiers.json` and `game.wasm`.
Every instance executes a full warmup before restoring its guest snapshot for
the timed pass. Variant order reverses on alternate repetitions. All passes must
produce byte-identical final saved states and identical PCM/MIDI SHA-256 digests
against a native interpreter replay of that same initial state. Setup, snapshot
restoration, audio draining/hashing and final state serialization are outside the
execution timer. The driver writes measurements and compiler timings to
`report.json`; the native oracle timing is a single validation run, not a median.

The two-module variant uses a native reentrant poll trampoline. The generated
module imports the probe's memory and scratch base, as in the browser, so
callbacks copy no scratch; guest RAM/VRAM remain owned by the runtime instance.
These results measure Wasmtime, not a browser engine. The probe's narrow unsafe
FFI boundary transfers owned input buffers and opaque owner handles only between
this diagnostic module and its trusted synchronous driver. Emulation libraries
and the shared diagnostic Session adapter retain their existing safe interfaces.

The recorded ROTT replay used the same two-million-step intro window and exact
profile/module hashes as the earlier AOT test. Three repetitions per variant,
each with a full warmup, gave these medians:

| Wasmtime AOT execution | Median seconds |
| --- | ---: |
| Complete interpreter/runtime in WASM, decode cache disabled | 0.16174 |
| Complete interpreter/runtime in WASM, decode cache enabled | 0.03036 |
| Translated WASM with runtime/fallback in WASM, decode cache disabled | 17.53171 |

All 18 warmup/measured passes matched the native oracle's complete final saved
state and PCM/MIDI digests. The saved-state SHA-256 was
`b2b3b761b8dda7ee7f7962840366a865c3fdbf6897618a62f4a0a25f2a75e37f`.
The backend again translated 954,700 steps with 1,465,702 polls and 420,202 grants.
It was about 108 times slower than the uncached WASM interpreter in this adapter.
The cached interpreter was about 5.3 times faster than the uncached interpreter.
These are measurements of this prototype and bounded intro, not of a future
backend with broader instruction coverage or reduced validation/transport work.

AOT compilation of the full runtime took 297.4 ms, with 4.48 ms to load its native
artifact. The translated module took 7.83 ms to compile and 0.14 ms to load, all
outside execution measurements. Results are in
`target/rott-wasmtime-interpreter/report.json` and `measurements.json`.

Validate the standalone diagnostic and shared host adapter with:

```sh
cargo test -p doslet-wasm-game-probe -p doslet-wasm-aot --locked --offline
cargo clippy -p doslet-wasm-game-probe --target wasm32-unknown-unknown --locked --offline -- -D warnings
cargo clippy -p doslet-headless --example compare_wasmtime_game --example compare_wasm_game --features wasm-aot-test --locked --offline -- -D warnings
```
