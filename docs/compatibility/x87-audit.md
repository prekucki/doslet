# x87 implementation audit

Audited on 2026-10-15 against the current Doslet working tree. The target is an
i386 with an optional 387, not a P6/SSE processor. This report covers the
decoder, numerical representation, supported instructions, exceptions,
memory transfers, CPU integration and tests. It is an implementation audit and
a bounded set of differential probes, not a proof of complete x87 correctness.

## Result

The encoding map is complete under the documented-instruction policy; the FPU
execution model is a useful but incomplete subset. The main limitations are the
restricted exponent range, missing real80/environment transfers, inconsistent
special-value handling, and an explicit-stop exception policy that differs
from architectural exception completion. Adding instruction handlers alone
will not resolve these limitations.

The audit also delivered finite FYL2X, F2XM1 and FSCALE support, a fix for stale C1 after FLD
ST(i), and a guard against a host panic in FRNDINT's diagnostic conversion.
The latter is a containment fix: the formerly crashing numeric range still
returns an explicit unsupported error.

## Evidence and boundaries

| Check | Evidence | What it establishes |
| --- | --- | --- |
| Encoding classification | All 2048 D8–DF/ModR/M combinations | Complete classification under the declared ISA policy |
| Interpreter routing | Each of those combinations enters the CPU decoder | Implemented entries have handlers; missing/reserved entries have explicit errors |
| Basic arithmetic | 360 captured native division/multiply/subtract cases | Agreement for these inputs, PC/RC choices, retained results and tested flags |
| Constants | 28 native captures; tests vary PC and precision masking | Seven constants, all RC choices, exact retained bits, precision independence |
| FYL2X | 540 native captures | Sampled results within two adjacent extended-precision representable values; tested PC independence and precision status |
| F2XM1 | 144 native captures | Sampled results for signed zeros, tiny inputs, endpoints and interior values; tested PC independence and precision status |
| FSCALE | 300 native captures | Exact retained results, including signed zeros/FLDLN2 and fractional scale operands across PC/RC choices |
| Audit probes | Register duplication, upper-binade FRNDINT, memory/register divide-by-zero, infinity comparison, unmasked precision | Specific mismatches and fixes listed below |
| Game execution | User supplied failures at FTST, FLDLN2 and FYL2X in Heroes | Those instruction paths are reached; it does not establish complete game compatibility |

Native probes used the host x86-64 x87, not a physical 80387. In particular,
transcendental low bits and C1 are not assumed identical across x87 generations.
The FYL2X comparisons intentionally do not assert bit-identical C1. Their
observed tolerance is not a universal accuracy bound over all possible inputs.

After the changes in this audit, 1400 of the 1620 documented 387 byte pairs
select an implemented handler; 220 select a missing handler. A further 168
pairs identify later P6/SSE3 instructions and 260 are reserved under this
policy. These counts repeat memory addressing forms. They must not be used as
a percentage of numerical or game compatibility. Some mnemonics, notably FLD
and FSTP, have both implemented and missing formats.

The detailed byte map is in [x87 encoding coverage](x87.md).

## Confirmed findings

| Priority | Finding | Reproducer / consequence | Current status |
| --- | --- | --- | --- |
| P0 | Host panic in diagnostic conversion | Load `f64::MAX`; FRNDINT returns the already integral value; `Finite::to_f64` used an unchecked range assumption | Fixed by rejecting that unsupported result before conversion; full range remains pending |
| P1 | Unmasked precision uses the wrong completion model | CW=005Fh, ST(1)=1, ST(0)=binary64(1/3), FADDP: emulator leaves TOP=6/source unchanged; native x87 has TOP=7 and result AAAAAB0000000000h at exponent 3FFFh, SW=BAA0h | Open; current tests encode the explicit-stop policy, not hardware completion |
| P1 | Register and memory divide differ on zero | `divide(1, false)` with 1/0 returns masked infinity; `arithmetic_memory(0, 6)` returns Unsupported | Open; shared special-value/exception rules are needed |
| P1 | Real80 range is absent | Arithmetic accepts only nonzero result exponents −1022 through +1022; valid extended intermediates outside that range stop | Open; range is deliberately restricted in `finite::round` |
| P1 | Stack exceptions are not architectural masked operations | Empty operands return StackError; overflow sets selected status bits and returns rather than completing with an indefinite value | Open; affects more than the missing opcode list |
| P2 | FLD ST(i) retained stale C1 | Start with 1.75; FIST m16 sets SW=3A20h; duplicate ST(0): emulator previously gave 3220h, native gives 3020h | Fixed; PE remains sticky while C1 clears |
| P2 | Memory special-value comparisons are narrower than register comparisons | FCOM with a memory +infinity returns Unsupported; ordered register comparison can compare infinities | Open; explicitly limited today |
| P2 | FRNDINT special inputs remain incomplete | FRNDINT on a register infinity returns Unsupported | Open; register infinities can be produced by implemented masked division |
| P2 | Memory denormal provenance is lost | m32/m64 input becomes a `Finite`/binary64 value without an operand-format exception result | Open; inspect DE handling when unifying memory conversions |
| P2 | Missing pending-exception delivery | Arithmetic returns a host error; FWAIT checks availability but does not deliver pending FPU exceptions | Open; distinguish 386/387 external error signaling from later integrated-FPU policies |

The negative-zero addition probe did **not** find a defect: adding −0 to −0
preserved −0 in both the emulator and the native probe. Similar-looking code
is not treated as a confirmed mismatch without a counterexample.

## Numerical representation

`X87` currently stores a diagnostic binary64 value, optional normalized finite
metadata with a 64-bit significand, a physical tag word, TOP in the status word,
and control/status words. Retaining the finite significand is necessary:
FILD and constants can contain bits that the diagnostic binary64 copy loses.
Basic finite arithmetic correctly uses the retained metadata in the tested
paths. Replacing these paths with host `f64` or a binary64 logarithm would
discard that benefit.

The dual representation also creates risks. Some classification and division
branches use the binary64 copy while other operations use retained data.
NaNs do not have a full independent extended encoding; the representation is
not sufficient for raw payloads, signaling/quiet distinctions or unsupported
real80 patterns. `Finite::to_f64` assumes a restricted range and still contains
an `expect`; the newly added result guard addresses the confirmed reachable
FRNDINT panic, not a general proof that every future caller is safe.

Recommended foundation:

1. Use an authoritative extended register encoding, such as a 64-bit
   significand plus sign/exponent, with explicit classification.
2. Derive binary64 only for inspection/output. Diagnostic overflow must not
   restrict otherwise valid guest arithmetic or crash the host.
3. Retain integer intermediates for basic arithmetic and conversions. Add
   exponent-boundary handling and sticky rounding information explicitly.
4. Preserve the existing physical tags and TOP ownership, but centralize their
   validation and mutation with result/exception completion.

This can remain allocation-free and `no_std`; it does not require sharing the
machine with a host math library or an audio/UI callback.

## Instruction families

| Family | Working subset | Remaining work |
| --- | --- | --- |
| Stack and signs | FLD/FST/FSTP ST(i), FXCH, FFREE, FCHS/FABS, TOP rotation | Masked stack fault semantics, extended special encodings |
| Constants | All seven constants, retained precision, RC independent of PC | Broader architectural exception integration |
| Real transfers | Selected finite m32/m64 loads and rounded stores | m80, nonfinite loads, denormal exceptions, overflow/subnormal stores |
| Integer transfers | FILD/FIST/FISTP m16/m32/m64, signed limits and RC | BCD; consistent exception completion and memory-fault handling |
| Basic arithmetic | All documented register directions/pop forms; finite real and integer memory operands | Shared zero/infinity/NaN/denormal rules; full exponent range |
| Comparison | FCOM/FCOMP/FCOMPP, FICOM/FICOMP, FTST | FUCOM/FUCOMP/FUCOMPP, FXAM; consistent memory specials |
| Rounding and scaling | FRNDINT and FSCALE on supported finite inputs | FXTRACT, full-range/special FRNDINT/FSCALE |
| Remainders | Decoded and named | FPREM/FPREM1 including partial reduction and quotient condition bits |
| Roots | Decoded and named | FSQRT with PC/RC, exceptional domains and range handling |
| Logarithms | Finite FYL2X with zero/negative finite domain exceptions | FYL2XP1, nonfinite FYL2X, broader reference coverage and certified bounds |
| Exponential | Finite F2XM1 on [−1, 1], including signed zero and endpoints | Nonfinite inputs and broader numerical validation |
| Other transcendental | Decoded and named | FPATAN, FPTAN, FSIN, FCOS, FSINCOS |
| Control/status | FNINIT/FNCLEX, FLDCW/FNSTCW, FNSTSW, compatibility NOPs | Pending exceptions, control-word policy and accurate historical signaling |
| Environment | Operand formats decoded | FLDENV/FNSTENV, FRSTOR/FNSAVE, last instruction/data pointers and opcode |
| Later ISA | Named as P6/SSE3 | Deliberately not enabled for the i386/387 target |

## FYL2X added during the audit

The Heroes sequence reaches FLDLN2 followed by FYL2X, a common natural-log
construction. This is evidence of execution, not evidence of the actual
numeric operands at the reported fault: no register dump accompanied the log.

The new handler reads both operands before mutation, writes ST(1), and pops
once after successful supported execution. Positive finite inputs use 256-bit
integer squaring to extract 192 fractional log2 bits before the multiply.
This retains the low bits of input values near one and avoids rounding either
input through binary64. Iteration counts and temporary arrays are fixed.

The result uses extended significand precision independently of PC and honors
RC. Finite zero/negative inputs implement selected masked invalid/zero-divide
results; unmasked exceptions retain the current explicit-stop policy.
Nonfinite operands, full extended-range results and unsupported exception
completion remain explicit errors. C1 derives from this algorithm's rounding,
not from an attempt to reproduce a particular physical FPU's approximation.

The 540 native vectors cover nine positive binary64 x values, including values
on either side of one and large/small magnitudes; five y choices, including
retained FLDLN2 and a 61-bit integer; three PC settings; and all four RC choices.
They establish sampled agreement, not a complete-domain proof. More adversarial
extended inputs and an independent high-precision interval reference should
precede a stronger accuracy claim.

## F2XM1 added during the audit

F2XM1 was also reported by the user during this audit. Its implementation uses
Q2.254 integer arithmetic for the factored series
`x * ln(2) * (1 + t/2! + t^2/3! + ...)`, with `t = x * ln(2)`.
It runs at most 96 series iterations and handles tiny inputs without subtracting
two binary64 values close to one. Inputs outside [−1, 1] or nonfinite inputs
remain explicit errors. The same numerical-range and exception-completion
limitations apply. The 144 native cases permit a two-representable-value
tolerance; they do not establish a universal bound or exact hardware C1.

## Exception and memory integration

FSCALE was reported after F2XM1. Its finite handler changes the exponent without
rounding the retained significand, truncates ST(1) toward zero independently of
RC, and preserves ST(1)/TOP. The 300 native cases match exactly for the tested
normal results and signed zeros. Out-of-range results and nonfinite operands
remain explicit errors under the current numerical model.

The current code mixes numerical result production with mutations of status,
tags and TOP. It consistently stops several unmasked errors before popping,
but the native precision probe shows that this cannot become a universal
architectural rule. Pre-completion faults and post-completion precision/range
conditions need separate outcomes.

A shared outcome should carry the result encoding, sticky exception bits,
condition changes, whether a result is committed, and the stack effect. The
instruction adapter then applies the correct masked/unmasked policy.
Instruction/data pointers and pending errors belong to architectural state,
not error strings or a frontend.

Memory adapters already select 16/32-bit addressing independently of fixed data
widths, validate segment access and avoid popping before a successful store.
That is useful structure to retain. However, stores update FPU status before
all bus writes finish, and multi-byte bus writes can fail partway through.
Precise CPU exception/restart semantics are not established by the present
tests. Fault injection is needed before claiming atomic/restartable memory
transfers. Environment operations must also account for their state-changing
side effects and operand-size/mode-dependent layouts.

## Implementation order

1. **Preserve host safety and diagnostics.** Keep the new panic regression and
   explicit unsupported-range errors; add precise operand/CW/SW/TW capture to
   future game failures when needed.
2. **Unify the register and exception model.** Add authoritative extended values,
   conversion outcomes and separate pre/post-completion exception policies.
   Revisit tests that currently encode the nonarchitectural unmasked-precision
   rollback behavior.
3. **Complete real transfers and environment state.** Implement raw m80 and
   FLDENV/FNSTENV/FRSTOR/FNSAVE with mode/size fixtures and missing pointers.
4. **Finish classification, comparison and utility arithmetic.** FUCOM family,
   FXAM, FSQRT, FXTRACT and FPREM/FPREM1 are separate, testable pieces; extend
   FSCALE's numeric range and special-value handling with the common model.
5. **Extend transcendental coverage.** FYL2XP1 and then trig/atan with
   explicit range reduction, special-value tables, C2/stack effects, and
   independent accuracy testing. Do not use binary64 as a silent fallback.
6. **Validate games.** Record real Heroes/PRINCE runs independently of decoder
   coverage, synthetic CPU tests or a successful build.

No reliable time estimate follows from opcode count alone. Environment and
special-value work crosses many existing handlers; transcendental accuracy is
a separate numerical task.

## Acceptance matrix

- Raw extended values: zeros of both signs, finite extrema, subnormals,
  infinities, signaling/quiet NaNs, payloads and unsupported encodings.
- PC/RC: supported PC choices and reserved-PC policy; all RC choices; tie,
  sticky-bit, cancellation and sign cases.
- Stack: every TOP/index, empty source/destination, overflow, one/two pops,
  masked versus unmasked results, and preserved values/tags on the appropriate
  pre-completion failures.
- Exceptions: each exception masked/unmasked; sticky bits and priority;
  pending-error delivery and FWAIT/no-wait behavior; post-completion results.
- Memory: m16/m32/m64/m80, segment/address/operand overrides, environment image
  layouts, and injected read/write faults at byte boundaries.
- Algorithms: native vectors plus an independent high-precision reference;
  monotonicity and rounding boundaries for log/root/trig; no claim of a global
  ulp bound from a finite fixture set.
- Runtime: unchanged owner/bus contracts, no execution allocation, bounded
  iterations, native/WASM agreement and recorded game evidence.

## Source pointers

- [Opcode map](../../crates/cpu/doslet-x86/src/x87_decode.rs)
- [Interpreter adapter](../../crates/cpu/doslet-x86/src/engines/interpreter/x87.rs)
- [FPU state and operations](../../crates/cpu/doslet-x87/src/lib.rs)
- [Finite arithmetic and rounding](../../crates/cpu/doslet-x87/src/finite.rs)
- [Bounded logarithm](../../crates/cpu/doslet-x87/src/logarithm.rs)
- [Bounded exponential](../../crates/cpu/doslet-x87/src/exponential.rs)
- [Native logarithm captures](../../crates/cpu/doslet-x87/src/native_logarithms.txt)
- [Intel SDM Volume 1, chapters 4 and 8](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-1-manual.pdf)
- [Intel instruction reference and opcode maps](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)

Intel's reference establishes that PC affects selected basic arithmetic and
FSQRT; it does not reduce every operation to PC precision. Its logarithm
accuracy discussion also distinguishes approximation accuracy from exact
correct rounding. Those distinctions are essential to interpreting the native
FYL2X probes.
