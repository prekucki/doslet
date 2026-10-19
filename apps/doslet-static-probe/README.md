# Static compiler CPU acceptance probe

This diagnostic builds the actual Rust CPU interpreter as a standalone WASM
module with no JS or WASI imports. The static program imports its memory and
`static_runtime(frame_pointer, reason)` function directly. The native driver
performs setup and reads small results; execution callbacks stay inside WASM.

```sh
cargo build -p doslet-static-probe --target wasm32-unknown-unknown --release --locked --offline
cargo run -p doslet-wasm-aot --example compare-static-cpu --release --locked --offline -- target/wasm32-unknown-unknown/release/doslet_static_probe.wasm 500000 3
```

The workload is a fixed protected-mode register loop: `INC EAX; ADD DX,3;
XOR BH,AL; DEC ECX; JNZ loop`. It exercises dword, word and high-byte aliases
with nonzero preserved register bits. Five instructions execute per iteration;
the final HLT is an endpoint marker and is never executed. The interpreter runs
exactly that instruction count using `Cpu::interpret_batch` when available and
ordinary `Cpu::step` fallback. Counted static execution receives one extra budget
unit so it reaches the same endpoint callback before its soft budget expires.
HOT omits instruction accounting and reports its exact total as unavailable.
Both static modes retain independent structural service callbacks every 64
regions. These synthetic callbacks synchronize small CPU state and enforce an
endpoint; they do not emulate hardware scheduling.

The driver uses Wasmtime 45.0.3 with Cranelift Speed, Backtracking and compiler
inlining for all paths. Each case owns a separate CPU/bus, receives one full
untimed warm-up, and preserves a warmed decode cache across register resets.
It rotates case order, reports repeated medians/ranges, and checks all eight
GPRs, EFLAGS, EIP, the complete 64-KiB guest RAM hash and counted work outside
every timed run. An independent algebraic oracle checks the register/flags
result, and RAM must match its initial hash. Untimed runs at 1, 2, 63, 64 and 65
iterations verify the structural checkpoint boundary. Allocation, reset,
compilation/loading and inspection are
outside timing. Iterations default to 500000 (maximum 2000000); timed repetitions
default to three (maximum 21).

The trusted synchronous FFI accepts only live handles from this module instance.
Each handle owns one CPU, bus and 72-byte wire frame. The frame is the first
field of a `repr(C)` owner, so the handle is also its frame pointer. No Rust
reference is retained while generated code borrows frame bytes; its callback
synchronously recovers the sole mutable owner. Reentry and concurrent calls with
the same handle are forbidden. Only this application wrapper dereferences raw
handles; the CPU and compiler libraries remain safe. Guest RAM is never exported
or copied to the native driver.

This synthetic acceptance check does not establish game compatibility, device
timing, real-game speedups, or production-browser performance.
