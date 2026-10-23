# Pinned v86 CPU and JIT

Source: [copy/v86](https://github.com/copy/v86/tree/cc37f35c43bf14865d74ad76879ecf9410f69eb1),
revision `cc37f35c43bf14865d74ad76879ecf9410f69eb1`.

This directory contains the upstream Rust CPU/JIT, its instruction-table
generators, and the bundled Berkeley SoftFloat C source. It does not include the
JavaScript machine, frontend, firmware or disk images. `LICENSE` preserves the
v86 BSD-2-Clause notice; SoftFloat's notices remain in `softfloat/softfloat.c`.
The upstream `LICENSE.MIT` is retained for provenance, although the floppy
emulator it covers is not imported.

Doslet changes:

- `Cargo.toml` builds an isolated WASM cdylib, outside the main workspace.
- `rust/doslet.rs` exports initialization, stepping, JIT preparation, segment
  setup and external interrupt delivery. `rust/lib.rs` includes this adapter
  and omits the unused Zstandard module.
- Attached-machine entry points export/import architectural context, apply
  external A20 mapping, enforce grants, and stop at software-interrupt handlers.
  I/O/control boundaries return before execution so Doslet commits prior time.
  Ordinary extended opcodes retain their instruction grant; system/time
  opcodes such as RDTSC still return to the machine.
- `rust/cpu/cpu.rs` routes all I/O ports, including PIC ports, to the host;
  external scheduling owns interrupt acknowledgement. Interpreter chunks and
  generated-loop checkpoints use a 256-instruction limit. Interpreter chunks
  also stop at that limit on forward paths.
- `rust/cpu/instructions.rs` leaves hardware timers to the host at HLT.
- `rust/cpu/memory.rs` routes mapped reads/writes, including APIC ranges, to
  the external bus and disables the private SVGA linear-framebuffer shortcut.
- `rust/cpu/string.rs` yields after one REP element and commits comparison flags
  at that boundary. `rust/cpu/call_indirect.rs` counts actual JIT entries.
- `rust/jit.rs` and `rust/codegen.rs` correct precharged block counts on early
  fault/SMC exits. Unexecuted suffixes and retried SMC instructions are removed
  from the count; architectural fault delivery retains one attempted CPU unit.
  Generated basic blocks must fit the remaining host grant.
- `gen/*.js` retain the upstream generation algorithms, with module/runtime
  imports and filesystem operations removed. The Rust build tool evaluates
  them with Boa, captures their output and writes the six generated Rust tables
  under `rust/gen/`. No Node runtime or package manager is used.

The isolated upstream module still uses `std`, unsafe Rust and SoftFloat C.
Those are compiled into WASM; no upstream raw pointer becomes a native Rust
pointer. The main workspace's `doslet-v86` wire crate is `no_std`, and the native
host confines a scoped device borrow to a small unsafe module. This is not a
conversion of upstream v86 into a generic `no_std` Rust CPU library.

Build and validation: [v86 core](../../docs/compatibility/v86-core.md).
