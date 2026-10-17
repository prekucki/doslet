# x87 implementation audit

This audit covers Intel's 387 instruction contract and captured native x86-64
x87 results. The target is i386 plus optional 387. This report
separates opcode execution coverage, tested numerical behavior and remaining
architectural limits; it does not certify full x87 or game compatibility.

## Result

All 1620 documented 387 D8–DF/ModR/M pairs now select interpreter handlers.
The other pairs remain 168 later P6/SSE3 encodings and 260 reserved encodings.
These counts include repeated memory addressing forms, not distinct operations.
The [encoding map](x87.md) lists every family. P6/SSE3 operations and
undocumented aliases have deliberately not been enabled on the 386/387 model.

The implementation now includes real80 and packed BCD transfers, environment
save/restore, FUCOM/FXAM, FSQRT, FXTRACT, FPREM/FPREM1, FYL2XP1, FPATAN and the
trigonometric family, alongside the previously implemented basic arithmetic,
constants, FYL2X, F2XM1 and FSCALE. Missing opcode handlers are no longer the
main compatibility boundary. Stack faults, exception signaling and some
nonfinite numerical domains remain incomplete.

## Implementation boundary

Doslet uses portable `no_std` arithmetic with retained 64-bit significands,
independent fixed-width integer kernels and fixed local arrays. Runtime code
has no new dependencies, unsafe blocks, host math calls or allocations.
Native assembly is confined to the standalone reference generator; it is not
linked to any emulator or WASM target.

## Register and transfer model

Each physical register retains its raw 80-bit encoding, normalized finite
metadata where valid, and a diagnostic binary64 view. Raw loads/stores and
register copies preserve payloads, pseudo-denormals and unsupported encodings.
Arithmetic uses finite metadata or explicit special classifications, never the
diagnostic binary64 exponent range. Diagnostic overflow/underflow can produce
infinity/zero without constraining guest values or panicking the host.

Basic arithmetic supports the real80 normal/subnormal range, all supported PC
choices and four RC modes. Masked overflow chooses infinity or maximal finite
according to RC/sign; underflow and memory stores round to their format grid.
Native boundary probes distinguish tininess detection from simply inspecting
the final rounded exponent. Memory and register basic arithmetic share
zero/infinity/NaN handling. Real32/64 inputs retain source-format denormal and
signaling-NaN information. Extended transfers do not quiet or reject raw bit
patterns merely because later arithmetic may find them unsupported.

Environment transfers implement 14/28-byte environments and 94/108-byte full
states, selected by operand size, independently of effective address size.
Protected/real pointer layouts, opcode, CW/SW/TW and logical ST(i) contents are
included. Control/environment instructions preserve last arithmetic pointers.
FNSTENV masks and FNSAVE resets only after completed writes. Restore stages the
whole input before changing FPU state. Synthetic injected bus faults test these
state guarantees; partial memory writes themselves are not transactional.

## Numerical kernels and bounds

| Family | Algorithm / bound | Boundary |
| --- | --- | --- |
| Basic arithmetic | Integer significands and u128 intermediates | PC=24/53/64; reserved PC=01 explicitly unsupported |
| FSQRT | 64-iteration restoring integer square root plus guest rounding | Normal/subnormal finite input, signed zero and selected specials |
| FPREM/FPREM1 | At most 63 quotient bits per instruction | Larger exponent gaps return partial remainder with C2; completed quotient bits C0/C3/C1 |
| FYL2X | 256-bit repeated squaring, 192 fractional log bits | Positive finite input and selected zero/negative-domain results |
| F2XM1 | Factored Q2.254 series, at most 96 iterations | Finite [-1,1], retaining tiny inputs without cancellation |
| FSCALE | Truncated exponent adjustment, full significand | Finite inputs, masked overflow/subnormal/underflow and DE |
| FSIN/FCOS/FSINCOS/FPTAN | Q64.192 pi/2 reduction and at most 64 series terms | Finite magnitude below 2^63; larger inputs preserve operand/TOP and set C2 |
| FPATAN | Magnitude ratio, quadrant handling, transformed atan series up to 192 terms | Finite operands; extreme tiny ratios use retained finite division |
| FYL2XP1 | Factored near-zero series up to 256 terms; otherwise 1+x and existing logarithm | Finite numerical domains; some nonfinite cases remain explicit errors |

Transcendentals ignore PC and honor RC. Their bounded approximate algorithms
and selected tiny-input shortcuts do not establish correctly rounded results
for every extended input. In particular, large-angle reduction uses a 192-bit
pi/2 constant rather than the physical FPU's limited approximation. Neither
historical-387 low bits nor universally identical C1 are promised. Intermediate
rounding in tangent, atan and the large-argument log1p path needs broader
adversarial/high-precision testing before a universal accuracy claim.

## Verification evidence

| Check | Cases | Established scope |
| --- | --- | --- |
| Opcode classification/interpreter routing | 2048 byte pairs | Documented 387 handlers; separate later/reserved diagnostics |
| Basic arithmetic | 360 native captures | Tested PC/RC results and flags for division/multiply/subtract |
| Constants | 28 native captures, expanded PC/mask checks | Seven constants, every RC, retained bits and precision independence |
| FYL2X | 540 native captures | Sampled agreement within two adjacent extended representable values |
| F2XM1 | 144 native captures | Sampled agreement within two adjacent extended representable values |
| Original FSCALE | 300 native captures | Exact retained bits for tested finite fractional scales and PC/RC |
| Root/remainders | 456 native captures | Exact results and tested defined condition flags, including repeated partial reductions |
| Extended scale boundaries | 256 native captures | Exact result/status at finite extrema, masked range errors and all RC/sign combinations |
| Memory-store boundaries | 160 native captures | Exact binary32/64 results/status, underflow ties and overflow directions |
| Trig/atan/log1p | 2088 captures, 1944 compared | Sampled results within four adjacent extended values; huge-angle hardware comparisons excluded |
| Real80 transfers | Raw special/extreme encoding matrix | Exact bytes, tags and status, including signaling NaNs and pseudo-denormals |
| CPU environment integration | Synthetic size-override and bus-fault cases | Widths, pointer recording, reset/mask ordering and preservation on incomplete reads/writes |
| Game execution | Bounded real HEROES.EXE headless run, mounted local CD, 100 million steps | Still Running in VBE101 with no reported CPU error; no interactive gameplay acceptance |

The bounded game command used the read-only local installation:

```sh
DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog cargo run -p doslet-headless --release --locked --example inspect_civ -- games/HoMnM/HEROES.EXE '' 100000000
```

A probe without the CD mount exited with the game's "Unable to access CD Drive"
message; that is not x87 execution evidence. The mounted probe is still only a
bounded startup execution, not evidence that every gameplay path works.

Native fixtures were captured on a modern x86-64 host, not a physical 80387.
The fixture tolerances are observed test thresholds, not complete-domain
accuracy guarantees. No ROM or game data is included in fixtures.

## Remaining architectural limits

- **Masked stack faults:** empty operands still return explicit StackError;
  overflow completion with an indefinite value is not implemented consistently.
- **Exception signaling:** the runtime still stops on an unmasked arithmetic
  error. Pending external 387 IRQ13/FWAIT delivery is not modeled. Loading CW
  or an environment is not a substitute for implementing that delivery path.
- **Range traps:** unmasked register overflow/underflow still stop without the
  architectural exponent-wrapped result. Register precision traps now commit
  the rounded result/pop before reporting, as the native FADDP probe requires.
- **Memory exceptions:** unmasked memory precision completion and precise
  exception/restart behavior across partially failed writes need further work.
- **Special transcendental operands:** several NaN/infinity combinations in
  scaling/logarithms/atan/trig still reject their domain or have only selected
  masked results. Complete exception-priority and NaN propagation tables are
  not certified for these families.
- **Numerical accuracy:** finite fixtures do not replace an independent
  high-precision interval reference across all extended inputs and RC choices.

## Reproducing new native fixtures

The standalone [reference tool](../../tools/x87-reference/README.md) contains
native assembly and must be run on x86-64. It has its own dependency-free Cargo
workspace and is not used by normal builds. Its output can regenerate the
new utility/range/store/trigonometry fixtures. Existing fixture-based tests run
on native and WASM without executing that assembly.

```sh
cargo test -p doslet-x87 -p doslet-x86 --locked
cargo test --workspace --locked
cargo check -p doslet -p doslet-web-worker -p doslet-web-audio --target wasm32-unknown-unknown --locked
```

## Architectural sources

- [Intel SDM Volume 1, chapters 4 and 8](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-1-manual.pdf)
- [Intel instruction reference](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)
