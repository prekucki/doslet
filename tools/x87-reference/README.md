# Native x87 reference captures

This dependency-free standalone Cargo workspace captures reference results from
the host x86-64 x87. It is deliberately outside the Doslet workspace: its unsafe
assembly never enters emulator/native/frontend/WASM code. Run it on x86-64 with
floating-point exceptions masked by the tool. Each case restores the original
control word and empties its temporary x87 operands.

From the repository root:

```sh
cargo run --manifest-path tools/x87-reference/Cargo.toml --locked --bin utilities > /tmp/native_utilities.txt
cargo run --manifest-path tools/x87-reference/Cargo.toml --locked --bin range > /tmp/native_ranges.txt
cargo run --manifest-path tools/x87-reference/Cargo.toml --locked --bin store > /tmp/native_stores.txt
cargo run --manifest-path tools/x87-reference/Cargo.toml --locked --bin trig > /tmp/native_trigonometry.txt
cargo run --manifest-path tools/x87-reference/Cargo.toml --locked --bin raw
cargo run --manifest-path tools/x87-reference/Cargo.toml --locked --bin special
```

Compare the outputs with `crates/cpu/doslet-x87/src/native_*.txt` before replacing
fixtures. `utilities` repeats FPREM/FPREM1 until C2 clears; its rows represent
completed reductions, not a specified hardware partial-reduction algorithm.
`raw` prints the transfer/tag matrix checked in the real80 tests. `special`
prints the two-result NaN/infinity behavior for FPTAN/FSINCOS/FXTRACT.

Results describe this host, not all historical 387 implementations. Large-angle
trigonometric outputs deliberately differ between hardware approximation and
Doslet's more precise range-reduction constant. The [audit](../../docs/compatibility/x87-audit.md)
records sample exclusions and comparison tolerances.
