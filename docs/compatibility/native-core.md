# Native dynamic core: performance and warmup

The native core now supports the frequent missing LEA, byte operations,
memory-left comparisons, stack accesses, near calls/returns and memory XCHG. Four balanced
one-billion-step PRINCE comparisons measured **2.56x execution throughput**
(9.558 s versus 24.502 s median), or **2.19x including observed preparation**.
The 300-million-step uncached-reference comparison also passed. These are
Linux x86-64 headless startup results, not interactive FPS or another target's
performance. Earlier implementation measurements are retained below.

The native feature remains enabled by default; `--no-dynamic-core` retains the
cached interpreter with ordinary scheduling. Scope and fallback are described in the
[architecture](../architecture.md#native-dynamic-core).

Measurements below use the native x86-64 target before integration with the
configurable-machine changes on `main`, using the Cargo `release` profile with `profiling` enabled and
Cranelift 0.132.3. They are headless measurements, not desktop FPS, AArch64 or
browser performance results. Both paths retain the existing decode cache and
VGA wait acceleration. The reference disables only the dynamic core.

## Extended instruction scope

The extension implements LEA, dword memory-left CMP/TEST, byte MOV/CMP/TEST
(including AL/AH aliasing and byte stores), PUSH/POP32, direct/indirect near CALL,
RET/RET imm16 and dword memory XCHG. Direct calls can reach prepared targets; indirect calls and
returns compare their actual target with instruction starts in the current graph.
An internal target retains SSA state and the remaining budget. External targets
return to Rust with the completed instruction and correct guest stack state.
This is not yet direct linking of separately compiled blocks.

Stack operations require full flat SS with a 32-bit stack; CALL/RET additionally
require full flat executable CS. Other segment layouts retain interpretation.
PUSH ESP uses the old pointer, POP ESP uses the popped value, and indirect CALL
reads its source before pushing the return address. Existing RAM bounds,
active-code protection and page-generation updates apply to all stack stores.
Memory XCHG captures its old address and register value before exchanging either
operand; failed guards leave both unchanged, including ESP/address aliases.

The measured byte TEST and memory-left CMP sites access globals near 142F0h,
which the old B0000h-based native view excluded. The PC now supplies backing RAM
from 500h to the configured RAM end (16 MiB in these measurements), with one
excluded VGA interval A0000h..B0000h. Every complete
byte/dword access rejects any intersection with that interval before touching
backing bytes. IVT/BDA stay below the view, preserving BDA read accounting.
A20 must be enabled. This keeps device behavior while allowing ordinary low-RAM
globals and stack buffers. Old public compiler execution methods retain their
arguments and default to no excluded interval; PC execution supplies the new
interval explicitly.

| Current mode, one billion steps | Round 1 | Round 2 | Round 3 | Round 4 | Median execution |
| --- | ---: | ---: | ---: | ---: | ---: |
| Cached interpreter | 24.919 s | 24.539 s | 24.464 s | 24.273 s | 24.502 s |
| JIT with historical profile | 9.786 s | 9.591 s | 9.523 s | 9.526 s | 9.558 s |

This is **60.99% less execution time**, or 2.563x throughput. Median observed
preparation is 1.612 s, and median total is 11.183 s versus 24.514 s for the
interpreter: **54.38% less total time**, or 2.192x throughput including that
preparation. Initial game/profile loading remains outside measured windows.
The four JIT sessions each executed 873,152,196 native steps, including
873,013,021 RAM/CFG steps over 115,151,224 entries (7.58 steps per entry).
The compilation count was 1,316 prepared representations, with no failures.

All eight sessions matched CPU/control state, guest time, RAM/VRAM, scanout,
execution counters and MIDI/PCM hashes at every 100-million-step checkpoint.
[Raw timing observations](native-core-expanded-profile.csv) retain all windows.
The intermediate run before memory XCHG measured 10.010 s versus 24.182 s,
with total 11.647 s; [its raw data](native-core-opcodes-profile.csv) is retained
separately. These sequential series do not isolate XCHG's contribution.
The earlier implementation used its own contemporaneous interpreter comparison;
absolute cross-series differences do not isolate the contribution of each change.

The separate uncached-reference test matched through 300 million steps across
136,359 varied batches with 207,244,607 native steps, versus 97,669,822 in the
preceding run. It checked full final RAM/VRAM, scanout/palette, MIDI/PCM and guest
CPU/time state. Native coverage is evidence of wider execution scope, not a
substitute for the timing result above. These remain startup checks rather than
full interactive gameplay acceptance.

At that measurement snapshot, all 84 PC tests, 18 adapter tests and 18 runtime allocation tests passed, along with workspace
tests, strict Clippy, WASM checks and native checks without default features.
Adapter tests cover all 65,536 byte CMP/TEST input pairs, high-byte aliases,
stack ordering, internal direct/indirect calls and returns, RET cleanup and
excluded-interval boundaries. Machine tests add partial budgets, changed CS/SS,
active-code aliases, low-RAM globals, both VGA edges and BDA read callbacks.
A mixed call/byte/LEA/stack loop executes natively without allocations after
preparation and matches the uncached interpreter.

## Integration with configurable machines

Integration on top of `main` at `f46beac` retains configurable RAM sizes,
AT-keyboard deadlines, storage ownership and profile saving before session
replacement or native window close. Native RAM borrows cover the actual
configured allocation. Tests exercise 4/8/16/32 MiB, the last valid dword and
out-of-range accesses, plus native execution before an AT typematic IRQ.

The integration differential exposed incorrect carry generation for trace
INC/DEC at wraparound. The generator now preserves incoming CF. A regression
checks INC FFFFFFFF and DEC 0 with both initial CF values in ordinary and
instrumented traces.

After that correction, the 300-million-step uncached-reference PRINCE comparison
passed across 136,359 batches with 207,244,448 native steps. Full final RAM/VRAM,
CPU/time, scanout/palette, counters and MIDI/PCM matched. All 93 PC tests and 19
adapter tests passed with workspace tests and strict Clippy; native builds
without default features and WASM checks also passed. The earlier timing series
has not been repeated after integration and the carry correction.

## Exit profile after the instruction extension

A separate cold one-billion-step diagnostic of the final code matched the cached
interpreter at every 100-million-step checkpoint. It retained 3,831 event keys
without drops and compiled 1,280 representations without failures. Actual native
calls completed 873,151,841 steps; there were 115,188,171 successful calls and
743 zero-step load guards, for **115,188,914 actual exits**. The 252,115
segment-proof refusals are separate and excluded from this denominator.

| Final actual native exit | Count | Share |
| --- | ---: | ---: |
| Supported successor outside the prepared graph | 109,857,980 | 95.3720% |
| Exhausted execution budget | 3,489,279 | 3.0292% |
| Unsupported successor instruction | 1,802,052 | 1.5644% |
| End of register block/prefix | 38,860 | 0.0337% |
| Load outside the RAM contract | 743 | 0.0006% |
| Store bounds, active-code protection or generation guards | 0 | 0% |

Unsupported exits fell from 71,197,595 to 1,802,052. Total exits increased because
much more guest work now executes natively; neither raw exit count nor coverage
alone measures speed. The timing above establishes the resulting throughput.
The dominant remaining boundary is supported code outside the current graph,
including far-away call targets/return sites and the unchanged 16-node/240-byte
window. Linking separate blocks and expanding discovery are distinct remaining
work; implemented CALL/RET only stay in one native function when targets belong
to its prepared graph. Event frequencies are not shares of host CPU time.

[Final raw exit observations](native-core-expanded-exits.csv) retain all phases.
The intermediate profile before memory XCHG had 9,576,351 exits at the newly
exposed `001F8ACE` exchange site, 8.317% of its 115,140,619 actual exits; see
[intermediate observations](native-core-before-xchg-exits.csv). Implementing that
measured form removed it from the unsupported category. No further opcode scope
was added after that final bounded extension.

## Measured native exit reasons

The following baseline predates the instruction extension above. The opt-in
`profile_dynamic_exits` diagnostic ran the local PRINCE workload cold
through **one billion logical steps**, preparing at most two blocks between
10,000-step batches. It compared CPU/control state, guest time, full exposed
RAM/VRAM, scanout/palette, execution counters and MIDI/PCM hashes with a cached
interpreter at every 100-million-step checkpoint. All ten checkpoints matched;
606 prepared representations compiled without errors. No timing conclusion is
drawn from this instrumented run.

There were **86,834,342 actual native exits**, including 743 zero-step load-guard
exits. Their completed steps total 406,506,464, exactly matching native-step
statistics. Successful native calls numbered 86,833,599; adding those 743
zero-step calls accounts for every actual exit. A separate 1,418 segment-guard
refusals did not invoke native code and are excluded from the percentages.
The 8,192-slot setup-allocated table retained 1,185 unique event keys without
dropping observations. [Raw window/entry/exit/reason counts](native-core-exits.csv)
contain addresses and metadata, not guest code bytes.

| Actual native exit | Count | Share of actual exits |
| --- | ---: | ---: |
| Unsupported successor instruction | 71,197,595 | 81.9924% |
| Supported successor outside the prepared graph | 9,498,130 | 10.9382% |
| End of a register block/prefix | 5,480,439 | 6.3114% |
| Exhausted execution budget | 657,435 | 0.7571% |
| Load outside the native RAM contract | 743 | 0.0009% |
| Store bounds, active-code protection or generation guards | 0 | 0% |

Unsupported instructions account for 81.19% of exits in each final 100-million
step window; graph boundaries account for 11.59% and budget exits about 0.77%.
These are event frequencies, not fractions of host CPU time. Rare budget exits
do not establish that checking a counter before every instruction is cheap.

Inspection of the largest exit sites found:

| Exit CS:EIP | Successor operation | Count | Share of all actual exits |
| --- | --- | ---: | ---: |
| 0160:001F8AF1 | RET | 9,549,705 | 11.00% |
| 0160:001E0C89 | TEST byte memory, immediate | 4,775,145 | 5.50% |
| 0160:001E0BD9 | Direct CALL | 4,774,959 | 5.50% |
| 0160:001E0BE8 | CMP dword memory, immediate | 4,774,804 | 5.50% |
| 0160:001E0C64 | Direct CALL | 4,761,869 | 5.48% |
| 0160:001D7E2D | POP EDX | 4,747,707 | 5.47% |
| 0160:001F6065 | Supported MOV EAX,[EAX], outside graph | 4,695,039 | 5.41% |
| 0160:001D7E1A | Indirect CALL | 4,694,369 | 5.41% |
| 0160:001DB0AF | LEA | 3,110,751 | 3.58% |

Five inspected CALL sites alone account for 23.61% of actual exits, four POP
sites for 21.53%, and the RET site for 11.00%. The 24 largest unsupported sites
cover 98.77% of unsupported-instruction exits. This is direct evidence for
extending stack/call/return support, byte TEST, memory-left CMP and LEA before
assuming that less frequent budget checks will remove most returns to Rust.
Eliminating an exit may expose another unsupported successor; these percentages
are not predicted speedups.

Two graph edges dominate that category. `001F602C -> 001F6065` occurs 4,628,011
times: the supported successor lies within the byte window but beyond the
16-node discovery budget. `001D7E20 -> 001D7E17` occurs 4,511,436 times: the target
precedes the entry, outside the current forward discovery window. Together they
account for 96.22% of graph-boundary exits. Larger graphs and a discovery region
that can include preceding backedge targets have concrete workloads to test;
direct block linking can also avoid returning to Rust for supported successors.

The diagnostic mode must be enabled before any native compilation. Profiled
traces return an exact reason with their completed-step count; ordinary native
functions retain their existing ABI and contain no exit instrumentation.
Register-block reasons are classified from their known terminal form and returned
IP/budget. The host aggregation table is bounded, does not allocate during
execution, and separates refusals from native calls. Adapter tests exercise every
native reason; CPU tests compare profiled and ordinary execution, including
zero-step exits. Allocation tests pass with profiling both enabled and disabled.
Workspace tests, strict Clippy, WASM checks and native checks without default
features passed.

Reproduce the diagnostic independently of timing measurements:

```sh
cargo run -p doslet-headless --release --features profiling --locked --example profile_dynamic_exits -- /path/to/PRINCE.EXE 1000000000
```

## Earlier guarded stores and cached segment safety

Dword MOV stores from registers or immediates now continue inside a native
trace. The PC supplies an exclusive ordinary-RAM view only with A20 enabled
and an available code space. Every complete access must fit the view, and store
segments must be flat, readable and writable over the full 32-bit range.
Positive segment validation is retained per execution-cache entry and compared
against every used selector and complete hidden descriptor. Changing the
prepared graph, CS/IP, privilege, mode or a used segment invalidates that proof.
The ordinary per-entry RAM mapping and code-generation checks remain required.

Each successful native store increments the generation of each touched physical
4-KiB page once, including unaligned cross-page dwords. Stores to either active
trace page (including gaps and neighboring bytes) exit before changing memory.
The interpreter performs those writes and code invalidation. Stores to another
compiled block may continue, but its next lookup observes the changed generation.
Exhausted generation counters and unavailable mappings also exit before effects.
The immutable compiler entry point rejects writable traces; the immutable CPU
entry point may execute a register prefix but never stores. Mutable native
execution requires an exclusive bus/RAM borrow and never retains pointers.

The new synthetic linked-list update executes dword reads, arithmetic, stores
and an internal backedge. Three rotated five-million-step rounds measured
cached-interpreter times of 118.394, 118.160 and 117.812 ms, versus native times
of 6.405, 6.214 and 6.506 ms (median ratio **18.45x**). It averaged about
150 native instructions per entry and matched registers, flags, guest time and
complete exposed RAM/VRAM. This is synthetic evidence, not a game speedup.
When a larger RAM graph cannot run because of its segment or bus requirements,
a separately compiled register prefix remains available. This preserves existing
native work on machines with A20 disabled or segments outside the trace contract.
The fallback runs only after zero trace progress and within the same resolved
hardware budget. Its host function counts toward the lifetime compilation cap.

Tests also exercise a finite read/update/copy loop: **224 instructions in one
native entry**, with partial budgets and cross-page stores matching the reference.
Self-modification exits before the store; another cached block is invalidated;
MMIO, address overflow, nonflat/read-only/limited segments and A20 changes retain
fallback behavior. Segment tests first populate the positive cache before
changing descriptors or selectors, detecting stale safety assumptions.

The 300-million-step PRINCE differential test passed again across 136,359
bounded batches, with full final RAM/VRAM, VGA/palette, MIDI, PCM, CPU state and
guest time matching the uncached reference. Native work increased from the
earlier 85,019,541 steps to 97,669,822, including 94,863,817 in RAM traces,
with 527 prepared representations and no compiler failures. Workspace tests,
strict Clippy, WASM and native builds without default features passed. Both
register-prefix and writable-trace allocation regressions report zero runtime
allocations after preparation. These are startup execution checks, not full
interactive gameplay acceptance.

The paired game timing at that stage used four balanced rounds with interpreter and
historical-profile JIT sessions, each running ten successive 100-million-step
windows. Every window compared CPU/control state, exposed RAM/VRAM, scanout,
execution counters and MIDI/PCM hashes. All eight sessions matched through one
billion steps, with identical native counts across the four JIT sessions.

| Current mode | Round 1 | Round 2 | Round 3 | Round 4 | Median execution |
| --- | ---: | ---: | ---: | ---: | ---: |
| Cached interpreter | 25.506 s | 25.163 s | 25.132 s | 24.915 s | 25.147 s |
| JIT with profile | 19.967 s | 19.727 s | 19.656 s | 19.511 s | 19.692 s |

Median preparation was 0.501 s; observed totals were 20.207 s for JIT and
25.159 s for interpretation. The JIT used **21.69% less execution time**
(1.277x throughput), or **19.69% less observed total time**. Initial game/profile
loading is outside the measured windows. This measures the combined writable
traces, cached segment validation and current code generator; it does not isolate
the gain from stores alone or compare directly against a simultaneous old JIT.
Raw observations: [native-core-stores-profile.csv](native-core-stores-profile.csv).

Each JIT session executed 406,539,103 native steps, of which 395,246,882 were
RAM-trace steps over 81,354,830 RAM-trace entries (4.86 instructions per entry).
The trace length remains much shorter than the cyclic synthetic scan, despite
higher coverage. Further work on operands, linking, flags and budget checks
therefore remains relevant.

## Earlier read-only RAM and control-flow traces

The preceding extension added an independent trace compiler without changing the
register-only compiler's calling convention. A trace contains up to 16 reachable
instructions within a 240-byte code span, including internal relative branches
and exits before unsupported operations. Dword MOV/ALU memory sources use
base/index/displacement addressing through a borrowed, bounds-checked RAM view.
All used segments must be flat and fully readable. The PC view requires A20
enabled and exposes RAM above the VGA aperture, excluding BDA read accounting.
At that stage, stores, stack operations, calls and device accesses remained
interpreter work.

Each instruction checks the remaining hardware-bounded budget. Failed RAM guards
return before the load, with the correct prior registers, full flags, EIP and
completed-step count. Generated code holds state across internal branches and
backedges; it does not call Rust for each RAM read. Trace discovery allocates
nothing, and every entry retains code-generation, owner, mapping and CS checks.

The first four-mode experiment after adding RAM traces ran four rounds of ten
successive 100-million-step windows. All sixteen sessions matched the existing
window-by-window guest-state checks through one billion steps:

| Mode before profile-binding fix | Run 1 | Run 2 | Run 3 | Run 4 | Median execution |
| --- | ---: | ---: | ---: | ---: | ---: |
| Cached interpreter | 25.684 s | 25.642 s | 25.428 s | 25.433 s | 25.537 s |
| Batching without compilation | 35.073 s | 34.897 s | 34.766 s | 34.703 s | 34.832 s |
| JIT without profile | 22.164 s | 22.156 s | 21.893 s | 21.979 s | 22.067 s |
| JIT with historical profile | 25.966 s | 25.936 s | 25.737 s | 25.739 s | 25.837 s |

Cold JIT used **13.59% less execution time** than the cached interpreter. Its
median preparation time was 0.353 s and median observed total was 22.433 s.
It executed 355,930,732 native steps, including 302,221,334 in RAM traces.
The batching-only diagnostic still performs candidate discovery for every
instruction: broader discovery increased that cost. Its result must not be
treated as the cost of a separately optimized block interpreter.

The historical profile exposed a separate regression. Runtime binding scanned
its template list on cache misses and could choose a short old register prefix
before discovering the current larger RAM graph. The fix decodes the complete
current representation and uses the prepared pool's fingerprint plus full
byte/length comparison. Imported templates already enter that pool during
preparation, so no separate linear runtime search is needed. A regression test
covers both migration from a short prefix and immediate reuse of a current
complete profile. Raw observations before the fix are in
[native-core-ram-before-profile-fix.csv](native-core-ram-before-profile-fix.csv).

Three rotated rounds of five million logical steps, after warmup, measured:

| Workload | Interpreter median | Batching-only median | JIT median | Interpreter/JIT throughput |
| --- | ---: | ---: | ---: | ---: |
| 16-INC straight blocks | 103.329 ms | 115.049 ms | 14.800 ms | 6.98× |
| Dense ADC/DEC/JNZ loop | 108.702 ms | 119.125 ms | 9.075 ms | 11.98× |
| RAM linked-list scan, three loads and internal exit | 101.658 ms | 137.413 ms | 5.869 ms | 17.32× |

The RAM interpreter/JIT pairs were 101.505/5.869, 101.658/5.392 and
101.866/5.933 ms. Preparing its five blocks took 3.045 ms. Native entries averaged
125.2–125.3 steps; registers, flags, control state, guest time and complete exposed
RAM/VRAM matched after each round. The original diagnostic list is cyclic during
timing and contains no PRINCE code.

All 60 PC tests passed, including internal exits and partial budgets, full-span
load guards after pointer changes, data-segment/A20 changes, code mutation and
bus replacement, PIT interrupts, timestamped DMA PCM and profile migration. Adapter tests, workspace
tests including allocation checks, Clippy, WASM and native builds without default
features passed. The local PRINCE test matched the uncached reference through
300 million steps across 136,359 varied batches, with 387 compiled blocks and
zero compiler failures. Of its 85,019,541 native steps, 73,162,958 used RAM traces;
the previous register-only run executed 32,420,955 native steps. Matching native
coverage does not replace execution-time measurements.

## Earlier generated host code inspection

The diagnostic example compiles the original linked-list benchmark's eight
instructions (18 guest bytes), using the same trace emitter as the game. It does
not dump PRINCE machine code. Before the exit cleanup, actual x86-64 output was
3,048 bytes, including cold exits and constants, with a 128-byte stack frame and
13 return epilogues. Only one epilogue executes per call.

The guest `CMP EAX,EDX; JE exit` expanded into 40 host instructions on the hot
path. The emitter computes full ZF/SF/PF/CF/AF/OF, including parity with `popcnt`,
then tests the packed ZF. An abbreviated actual sequence was:

```asm
sub    ebx,r13d
sete   al
; pack zero/sign flags, then calculate parity
popcnt eax,eax
; pack parity/carry/auxiliary/overflow flags
test   r8d,0x40
jne    0xb60
```

Exact budget exits can expose flags immediately after CMP, so removing their
calculation requires lazy flag materialization or a fused path with correct
fallback exits. Simply omitting the unused branch flags is incorrect.

A conservative union of registers written anywhere in the trace limited
register stores, and all exits shared one epilogue. At that stage the same diagnostic produced
**1,816 bytes, a 96-byte stack frame and one return**, a 40.4% code-size reduction.
This is not a measured 40.4% execution-time improvement. A follow-up synthetic
run measured 6.137 ms median for the RAM scan versus the earlier 5.869 ms;
these separate runs do not demonstrate a speedup from the cleanup. No new game
speedup is claimed for this change. All 60 dynamic-core PC tests and six adapter
tests passed. Adapter tests cover a
register written only on an untaken branch and exact zero/partial budgets.

These assembly artifacts predate the later opcode and excluded-interval extension;
the command below compiles the current emitter and can produce a different size.
The complete actual assembly from that stage is available
[before](native-core-codegen-before-x86_64.asm) and
[after](native-core-codegen-after-x86_64.asm). Generate the diagnostic outside
execution timing with:

```sh
cargo run -p doslet-x86-jit --release --locked --example inspect_ram_trace -- /tmp/doslet-ram-trace
objdump -D -b binary -m i386:x86-64 -M intel /tmp/doslet-ram-trace.bin
```

The profile-binding fix was measured separately, before this codegen cleanup:
four balanced rounds with only interpreter/profile sessions, each through one
billion steps, gave median execution times of 24.620 s and 21.474 s respectively
(**12.78% less time**). Including observed preparation gave 24.632 s versus
21.843 s (**11.32% less time**). All eight sessions matched at every 100-million
step checkpoint. [Raw observations](native-core-ram-profile.csv) retain all
windows; the warmup example's optional fourth argument `profile-pair` selects
this comparison. Absolute timings should not be compared directly with the
four-resident-mode experiment above.

Those game traces average only **4.54 RAM instructions per native entry**,
compared with approximately 125 in the cyclic synthetic scan. Entry validation,
state transfer, per-instruction budget tests and full flag construction therefore
amortize over much less useful work. These are observed code-shape and coverage
facts; their individual shares of host execution time have not been profiled.

## Earlier register-loop extension

The native-loop extension executes a register body and its terminal backedge
inside one generated function, checking the remaining slice budget between
iterations. Code validation and architectural state stores occur at the call
boundary. The dense `ADC; DEC; JNZ` synthetic workload averaged approximately
248 native steps per entry, or 82.7 whole iterations, rather than returning to
Rust after each short register block.

After warmup, three alternating pairs of five million logical steps produced:

| Workload | Interpreter median | Native median | Throughput ratio | Native steps per entry |
| --- | ---: | ---: | ---: | ---: |
| Existing 16-INC straight blocks with interpreted backedge | 97.022 ms | 15.210 ms | 6.38× | 15.58 |
| Dense ADC/DEC/JNZ loop with native backedge | 111.610 ms | 9.190 ms | 12.14× | 248.00 |

The dense interpreter/native pairs were 112.291/9.777, 111.426/8.805 and
111.610/9.190 ms, with another 0.398 ms for preparing its one compiled block.
The straight-block pairs were 97.022/15.210, 96.770/14.965 and 97.452/16.444 ms,
with 2.436 ms of preparation. Registers, flags, control state, guest time and
step counts matched after every pair. These are two different synthetic
workloads; the throughput ratios do not measure the improvement over an older
JIT on the same dense-loop workload, nor do they establish a PRINCE speedup.

All 54 PC tests passed, including all 16 short/near Jcc conditions, budget
remainders, finite/infinite loops, carry between ADC/SBB iterations, unsupported
operand-prefix fallback, PIT interrupt timing and timestamped DMA PCM. Workspace
tests (including allocation checks), strict Clippy, WASM and native builds with
default features disabled passed. The local uncached-reference differential
test again reached 300 million PRINCE steps with matching observations and zero
compiler failures.

The full four-mode experiment was then repeated with native loops enabled:

| Mode | Run 1 | Run 2 | Run 3 | Run 4 | Median execution |
| --- | ---: | ---: | ---: | ---: | ---: |
| Cached interpreter | 24.849 s | 24.765 s | 24.566 s | 24.371 s | 24.665 s |
| Batching without compilation | 27.684 s | 27.571 s | 27.435 s | 27.184 s | 27.503 s |
| Batching and JIT, without profile | 24.861 s | 24.768 s | 24.628 s | 24.412 s | 24.698 s |
| Batching and JIT, with profile | 24.970 s | 25.068 s | 24.864 s | 24.668 s | 24.917 s |

Cold JIT was 10.20% faster than batching without compilation and effectively
tied with the ordinary interpreter (0.13% more execution time). Its median
preparation was 0.123 s and observed total was 24.835 s. All sixteen sessions
reached one billion steps with the same window-by-window comparison checks as
the experiment below. Native-step, native-entry and slice counts stayed
unchanged from the earlier run: this observed PRINCE path does not use the new
register-only native backedge. Its identified loop still needs memory
operations and internal conditional exits. Raw observations are in
[native-core-loops.csv](native-core-loops.csv).

## Separating native execution from batching

A follow-up experiment added `batching`: the same dynamic scheduler, operand
checks and block discovery, with profile loading and compilation omitted. It
asserted zero compiled blocks and zero native steps after every window. Four
rounds balanced the order of all four modes, each running ten successive
100-million-step windows. Results below precede the native-loop extension.

| Mode | Run 1 | Run 2 | Run 3 | Run 4 | Median execution |
| --- | ---: | ---: | ---: | ---: | ---: |
| Cached interpreter | 24.873 s | 26.478 s | 24.785 s | 24.748 s | 24.829 s |
| Batching without compilation | 27.437 s | 27.790 s | 27.247 s | 27.293 s | 27.365 s |
| Batching and JIT, without profile | 24.607 s | 24.740 s | 24.336 s | 24.347 s | 24.477 s |
| Batching and JIT, with profile | 24.898 s | 25.559 s | 24.753 s | 24.714 s | 24.825 s |

Enabling JIT reduced median execution time by **10.55% relative to batching
without compilation**, with paired reductions of 10.31%, 10.98%, 10.68% and
10.79%. The median difference against the ordinary cached interpreter was only
1.42%, with visible run variation. JIT helps within the current dynamic
scheduler, but much of that benefit offsets the scheduler's own validation and
discovery cost. The `batching` mode is not an optimized standalone block
interpreter: it still discovers native candidates, including instructions that
a compiled block can execute without separate lookups.

Medians were calculated after summing windows within each round, retaining all
four observations. The cold-JIT median preparation time was 0.119 s and median
observed total was 24.610 s. Initial profile loading/preparation remains outside
these window timings. Native availability also changes decode/discovery work
and cache behavior, so this comparison does not isolate the cost of generated
ALU instructions alone.

All sixteen sessions reached one billion steps. Each window matched the
registers, flags, control state, guest time, exposed ordinary RAM, VRAM,
scanout/palette, execution counters and MIDI/PCM stream hashes against the
cached interpreter. These are the exposed checks listed in the diagnostic,
not equality checks for every internal device state or stream timestamp.
Raw numeric observations are in
[native-core-batching-ablation.csv](native-core-batching-ablation.csv).

## Warmup and scheduling: one billion steps

The earlier version of `bench_dynamic_warmup` compared three persistent sessions:
cached interpreter, JIT starting without a profile, and JIT with an existing
local profile. Each ran
ten successive 100-million-step windows without resetting its cache. Three
experiments start fresh sessions and rotate execution order across modes and
windows. Timings cover only `Session::advance`; compilation/maintenance and
stream draining are reported separately. This diagnostic uses fixed 10,000-step
host batches, without CPU sampling or RGBA conversion.

| Implementation / mode | Run 1 | Run 2 | Run 3 | Median |
| --- | ---: | ---: | ---: | ---: |
| Before: interpreter | 25.074 s | 24.872 s | 24.749 s | 24.872 s |
| Before: JIT without profile | 32.676 s | 32.485 s | 32.345 s | 32.485 s |
| Before: JIT with profile | 33.684 s | 33.583 s | 33.314 s | 33.583 s |
| After: interpreter | 25.994 s | 25.118 s | 24.899 s | 25.118 s |
| After: JIT without profile | 26.766 s | 24.783 s | 24.681 s | 24.783 s |
| After: JIT with profile | 28.259 s | 24.943 s | 24.803 s | 24.943 s |

The initial regression was **not primarily a warmup delay**. Between steps
200–400 million, the original JIT compiled no new blocks yet remained slower.
The final five windows also retained the regression. Loading a profile did not
remove it. In the first window, preparation including compilation and maintenance
took less than 24 ms, compared with roughly half a second of execution overhead.

The optimized version eliminates overwritten arithmetic-flag calculations,
stores only changed registers and borrows CPU state directly. Its scheduler
combines native blocks with validated decoded RAM/near-branch instructions in
slices of at most 256 steps. IRQ checks, hardware advancement and execution
counters occur once per slice. Device I/O, MMIO, mode/IF changes, faults and
PIT/DSP deadlines remain boundaries; code writes still invalidate the next lookup.

In the new test, 138,901,632 steps used native machine code and 726,321,986 used
decoded interpretation inside slices, together accounting for 24,913,472 slices
(34.7 steps per slice on average). Native coverage remains limited; much of the
improvement comes from scheduling interpreter work more efficiently. In the
first 300 million steps, JIT without a profile took 6.760 s versus 6.707 s for the
interpreter. In the final 500 million steps it took 13.123 s versus 13.411 s.
These small differences do not support a general warmup or game-speedup claim.

Every window compared registers, flags, control state, guest time, complete
exposed ordinary RAM, VRAM, scanout/palette, execution counters and MIDI/PCM stream
hashes. All three experiments reached one billion steps in each mode without
mismatches or compiler failures. Raw observations are retained as
[before](native-core-warmup-before.csv) and [after](native-core-warmup-after.csv)
CSV files. Game bytes and the private profile are excluded.

The synthetic loop also improved after these changes: three alternating pairs
measured interpreter/JIT execution at 100.160/14.484, 103.731/15.225 and
105.132/14.718 ms. The medians are 103.731 and 14.718 ms (7.05 times the
throughput), with another 2.520 ms for preparation. This remains a register-loop
result; the game measurements above are the relevant end-to-end evidence.

## Initial implementation: 300 million logical steps

After an initial warmup pair, three pairs ran sequentially with alternating
interpreter/JIT order. JIT runs loaded the local observed-block profile. The
original game installation was read-only; the profile stayed under ignored
`target/`. Each run reached the 300-million-step limit while still running,
which intentionally produces the runner's budget-exhausted exit status.

| Pair | Interpreter execution | JIT execution | Interpreter total | JIT total |
| --- | ---: | ---: | ---: | ---: |
| 1 | 6.952 s | 9.187 s | 7.013 s | 9.322 s |
| 2 | 6.710 s | 9.029 s | 6.768 s | 9.164 s |
| 3 | 6.693 s | 8.788 s | 6.751 s | 8.920 s |
| Median | **6.710 s** | **9.029 s** | **6.768 s** | **9.164 s** |

JIT takes **34.6% more execution time**, or about 25.7% less execution throughput.
The observed loop total increases by 35.4%. Execution timing covers
`Session::advance`; the total also includes between-batch compilation, sampling,
frame conversion and output draining. Both exclude initial game/profile loading
and profile preparation before the timed loop. These instrumented timings should
not be substituted for an uninstrumented desktop benchmark.

Every measured JIT run compiled 88 blocks without compiler failures and executed
32,377,087 steps through native code: 10.8% of the logical budget. There were
14,964,988 native block entries, averaging only 2.16 steps per entry. That
implementation excluded memory operands and branches. These observations identify
limited coverage and short blocks; they do not isolate the host-time cost of
discovery, validation, dispatch or each fallback operation.

## Initial implementation: synthetic register loop

`bench_dynamic` warms both paths, then measures three alternating pairs of five
million steps. It checks registers, flags and guest time after every pair.

| Pair | Cached interpreter | JIT |
| --- | ---: | ---: |
| 1 | 95.478 ms | 20.935 ms |
| 2 | 93.065 ms | 21.150 ms |
| 3 | 90.012 ms | 20.025 ms |
| Median | **93.065 ms** | **20.935 ms** |

The median execution throughput is **4.45 times higher** with JIT. Preparing the
15 blocks took another 9.311 ms, outside the timed execution. This workload has
long sequences of register increments and no guest memory operands; it is not
representative of the PRINCE result above.

## Correctness and validation

The separate local PRINCE differential test compared JIT with the uncached
interpreter for 300 million steps across 136,359 bounded batches. It checked
registers, EFLAGS, control state, guest time, MIDI, PCM and execution counters at
batch boundaries, periodically compared RAM/VRAM and scanout/palette, and
compared complete exposed ordinary RAM and physical VRAM at the end. It passed
with 32,361,401 native steps and zero compiler failures. After the scheduling
changes it passed again with 32,420,955 native steps and 175,387,888 decoded steps
inside 7,062,645 slices, with zero compiler failures. Different batch sizes
explain why this native-step count differs from the timing run.

Workspace tests, strict Clippy, WASM checks and native builds with default
features disabled passed. Focused tests cover ALU flags, code writes across page
boundaries, bus ownership, A20 mapping, CS limits, interrupt/deadline budgets,
profile validation and allocation-free execution after preparation. New tests
also cover arithmetic carry dependencies, slice termination before IF changes,
MMIO and faults, and code modified by a RAM write within a slice. A DMA test
changes sample RAM while executing and compares timestamped PCM with single
stepping, detecting delayed audio deadlines rather than just final-state agreement. This is
startup equivalence evidence, not full interactive gameplay acceptance.

## Guest hotspots by phase

The separate `profile_dynamic_hotspots` run sampled the cached interpreter
through one billion logical steps. It collected 181,551 randomized PC samples
and 90,752 steps in sparse short traces, with no unavailable or capacity-dropped
observations. These frequencies measure the guest workload, not host CPU cost.
The trace and randomized-PC tables have separate denominators.

| 256-byte region, CS=0160 | Steps 0–100M | Steps 100–500M | Steps 500M–1B |
| --- | ---: | ---: | ---: |
| 001F6000 | 7.263% | 11.938% | 14.979% |
| 001E0C00 | 6.432% | 10.805% | 13.939% |
| 001D7E00 | 4.537% | 10.259% | 13.506% |
| 001F8A00 | 5.380% | 9.638% | 12.470% |

Local code inspection identifies a candidate at `0160:001F6041–001F6069`:
three dword RAM reads, register arithmetic and conditional branches, with a
backedge to `001F6041`. The loop body has no stores, calls, stack operations or
I/O. A side exit to `001F604F` must leave native execution before subsequent
stores; the normal exit is `001F606B`. This needs RAM loads and internal side
exits beyond the initial register-only native loop.

The region's 14.979% frequency must not be attributed to that loop alone. Nine
retained trace edges in the candidate account for 4,098 of the 90,752 traced
steps (4.516%, a lower bound from a truncated edge table). The captured data
does not establish many iterations per entry. That iteration distribution is
the next measurement needed before claiming this is a dominant tight loop.
Another hot loop near `001D7E17` contains an indirect call; the frequently
sampled `001F8AE1` fragment is a helper with RAM comparisons and returns.

The address/count-only observations are in
[native-core-hotspots.csv](native-core-hotspots.csv). Guest code bytes remain
outside the repository.

## Remaining optimization tasks

The follow-up work was split into three independent tasks: an experiment that
separates batching from native compilation, a phase-by-phase guest workload
profile, and a review of compiler/runtime boundaries. The first two add host
diagnostics. The review led to a separate implementation and test task for
bounded native loops.

The implemented loop contains a register-only body and one terminal short/near
Jcc/JMP back to its start, within the existing 16-instruction limit. Cranelift
keeps registers and flags in the generated function across iterations and
stores architectural state on return. It checks the remaining hardware-bounded
slice budget before each whole iteration and returns the exact step count.
Taken backedges and conditional exits preserve the correct EIP and full flags.
The first version computes arithmetic flags conservatively inside the loop;
it does not claim flag liveness optimization across iterations. The later RAM
trace compiler adds read-only memory and internal side exits with a per-node
budget. Stores, calls and indirect branches remain interpreter boundaries.

The code review found repeated work at native entry: `dynamic_batch` first
looks up and validates a decoded instruction, then `try_native_block` resolves
the code context and checks the native cache. Every native exit also makes the
architectural registers and flags available to the interpreter. These operations
are present in the code, but their individual share of host time has not been
measured. Short native blocks alone do not prove that function-call overhead is
the main bottleneck.

| Task | Smallest useful scope | Acceptance evidence |
| --- | --- | --- |
| Block-level budget and flag analysis | Reduce the current trace's per-instruction guards and flag work within proven basic blocks. Preserve exact partial progress before every memory guard and external exit. | Every exit and budget remainder, carry dependencies across paths/iterations, full state on return, PIT/DSP deadlines and timestamped PCM, repeated real-workload measurements. |
| Wider operand scope | Measure word operands, byte arithmetic beyond CMP/TEST, remaining shifts/extension operations, or nonflat segments after the implemented byte MOV/CMP/TEST and memory-left comparisons. | Segment permissions/limits, A20 and range boundaries, MMIO fallback without extra device reads, identical guest state and streams. |
| Wider store operations | Measure memory-destination ALU operations or word stores after the implemented byte/dword MOV, stack writes and dword memory XCHG. | Self-modifying next instruction/block, cross-page writes, code-space exhaustion and bus replacement, exact guest RAM/VRAM/time/PCM. |

`NativeRam` supplies the read-only borrow and `NativeRamMut` supplies exclusive
RAM and page-generation borrows. The existing `is_plain_ram` boolean alone cannot
justify retaining a pointer to guest RAM. Native stores and exchanges advance
code generations and reject writes to active code pages before effects; broader store
support must preserve those guarantees.

Direct links between separately compiled functions come after these steps.
A backedge within one bounded function can remove repeated dispatch without
introducing persistent links that must be invalidated and detached. Future flag
liveness optimization must account for the loop backedge; the straight-block
linear analysis is insufficient. Full observable flags remain required whenever execution returns
to the interpreter or reaches an IRQ boundary.

Persisting native code ahead of execution (AOT) is deferred until execution
coverage improves and startup measurements justify it. Removing all of the
previous experiment's roughly 0.12 s of preparation would save only about 0.5%
of its roughly 25 s run, before artifact loading and binding costs. AOT does not
remove runtime validation, interpreter fallback or short block boundaries.
Artifacts would also need host ISA/features/ABI and compiler/IR version checks,
plus validation against actual relocated guest bytes and current memory state.
The existing DSJ1 profile contains observed bytes and address hints, not native
machine code, execution-frequency counts or successor profiles.

## Reproduce

Set `DOSLET_PRINCE_EXE` to the local installation before these commands. Keep the
profile outside the game directory; it contains observed guest code bytes and
must not be redistributed with commercial game data.

```sh
cargo run -p doslet-headless --release --features profiling --locked -- run-stub "$DOSLET_PRINCE_EXE" --steps 300000000 --profile-cpu --no-dynamic-core
cargo run -p doslet-headless --release --features profiling --locked -- run-stub "$DOSLET_PRINCE_EXE" --steps 300000000 --profile-cpu --core-profile target/prince.core-profile
cargo run -p doslet-headless --release --features profiling --locked --example bench_dynamic
cargo run -p doslet-headless --release --features profiling --locked --example bench_dynamic_warmup -- "$DOSLET_PRINCE_EXE" target/prince.core-profile
cargo run -p doslet-headless --release --features profiling --locked --example profile_dynamic_hotspots -- "$DOSLET_PRINCE_EXE" 1000000000
cargo test -p doslet-headless --release --features profiling --locked local_prince_dynamic_core_matches_reference_during_execution -- --ignored --nocapture
```

For the test, export `DOSLET_PRINCE_EXE` into the command environment. Warm up both
runners, then repeat three pairs with alternating order and compare medians.
Guest data and profiles are intentionally absent from this report.

The current warmup diagnostic includes four modes and four rounds. An optional
third argument from 1 to 10 limits the number of 100-million-step windows per
session. Its `batching` mode enables the dynamic scheduler and discovery but
never loads a profile or calls `prepare_dynamic`; each window asserts zero
compiled blocks and zero native execution. Rotation balances each mode's
position across the four rounds. Compile once before collecting timings, and
run the hotspot diagnostic separately so that it does not compete for CPU time.
