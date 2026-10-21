# x87 interpreter performance

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
