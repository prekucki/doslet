# Future work

## OPL3 without `std`

The initial AdLib/Sound Blaster FM integration uses the published
[`nuked-opl3` 0.1.0](https://github.com/tgies/nuked-opl3-rs) Rust port without
local dependency patches. This library requires `std`, so the PC/runtime
dependency graph currently also requires `std` despite retaining `#![no_std]`
in the other emulation libraries. Native and WASM targets provide it; a
freestanding target is not supported by this integration.

Revisit an upstream `no_std` option if freestanding embedding becomes a target.
Keep the current synthesis/timing tests and the allocation regression test as
acceptance checks. `std` availability and allocation during emulation are
separate concerns; the latter must stay forbidden in the execution path.

## CPU batching regression test

`batches_stop_before_if_changes_ports_mmio_and_faults` fails with a batch length
of 256 where the test expects 1. The same result was reproduced in an isolated
copy with the OPL integration removed. Determine whether the batching behavior
or the test's expectation needs correction; FM tests do not resolve this.

```sh
cargo test -p doslet-pc --features dynamic-core --locked batches_stop_before_if_changes_ports_mmio_and_faults
```
