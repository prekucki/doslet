# Future work

## Known bugs

### Gameport timing differs between CPU backends

Status: open. Reported on native ROTT with v86; the user reports that the same
controls work with AOT.

With the gameport enabled and `JoystickEnabled 1` in ROTT's saved `CONFIG.ROT`,
arrow keys can have no effect in the menu or game. Keyboard IRQs and scan codes
still arrive. ROTT reads keyboard direction first, then lets joystick
deflection overwrite it. A headless menu comparison using the saved overlay
reproduced the failure; changing only `JoystickEnabled` to `0` restored menu
navigation.

Both backends use the same gameport device, but advance its clock differently.
ROTT's joystick calibration counts polling-loop iterations. The interaction
with v86's host clock is the suspected timing cause; equivalent axis readings
and stability across backends have not yet been measured.

Workaround: disable the joystick in ROTT's **Options → Controls**, or disable
the gameport in the Doslet profile. The separate crash on reading disabled
port `201h` is fixed: reads return `FFh` and writes are ignored. That fix does
not resolve timing with the gameport enabled.

Next: compare axis polling counts and reported neutral position on both
backends, then verify stable neutral and directional input in ROTT with the
gameport enabled. See the [v86 keyboard and gameport diagnostics](compatibility/v86-core.md)
for the saved-overlay comparison and its validation limits.

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

## Resolved: CPU batching regression test

The previously reported batch-length mismatch no longer reproduces. The
release audit on 2026-10-24 passed this regression both in the workspace suite
and independently with `dynamic-core` enabled:

```sh
cargo test -p doslet-pc --features dynamic-core --locked batches_stop_before_if_changes_ports_mmio_and_faults
```
