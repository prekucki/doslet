# Future work

## Procedure AOT: delayed interrupts and external waits

The proposed procedure-level WASM backend executes small computational pieces
without interior interrupt polling and accounts work/device time on return.
Delayed interrupt delivery within a piece is a known timing limitation for the
initial experiment; precise interior delivery is deferred. The existing region
backend retains its strict event-free grant contract.

Before selecting a whole procedure, identify loops whose continuation depends
on memory that the loop and its callees cannot write. These are candidates for
waiting on an external writer, commonly an IRQ handler, and must yield to the
runtime while waiting rather than spin until a procedure return that the IRQ
itself enables. Cover variable-to-variable predicates and unrelated local
stores; unresolved aliases are uncertainty, not proof of an IRQ wait. Runtime
proof remains necessary before skipping iterations. See the
[procedure AOT proposal](compatibility/wasm-core.md#procedure-level-aot-proposal-and-known-interrupt-limitation)
for the execution contract, known problem and acceptance requirements.

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
