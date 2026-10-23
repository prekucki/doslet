# Experimental v86 CPU/JIT core

Doslet includes the actual CPU and x86-to-WASM JIT from
[copy/v86, revision cc37f35c43bf14865d74ad76879ecf9410f69eb1](https://github.com/copy/v86/tree/cc37f35c43bf14865d74ad76879ecf9410f69eb1).
The native `doslet-v86-host` adapter runs that CPU and its generated modules in
one Wasmtime store. `doslet-v86` defines the `no_std` wire types. This backend
is separate from Doslet's existing `doslet-x86-wasm` translator.

Select it on desktop with `doslet --v86 --game GAME.EXE` or
`doslet --v86 --config games/GAME.toml`. v86 owns CPU execution and RAM;
Doslet supplies DOS/BIOS services, Rust devices and the iced frontend.
Port and mapped-memory callbacks reach the existing VGA, MPU-401, PIC,
PIT and audio devices. No JavaScript runtime is required at execution time.
Synthetic tests establish only the behaviors they exercise; the bounded Quake
startup observations below do not establish general game compatibility.

## Build and run

Requirements: the repository's Rust toolchain, `wasm32-unknown-unknown`, and
Clang with its WASM target. The build uses Cargo, a Rust JavaScript engine (Boa)
for the imported table-generation algorithms, and Clang for the upstream
SoftFloat implementation. It does not execute Node, npm, npx or the upstream
Makefile. The generator is a separate locked Cargo package; it does not add Boa
to application dependencies.

From the repository root:

```sh
cargo run --manifest-path tools/v86-build/Cargo.toml --release --locked
cargo run -p doslet --release --locked -- --v86 --demo
cargo run -p doslet --release --locked -- --v86 --config games/Quake.toml
cargo run -p doslet-v86-host --example smoke --locked --offline
cargo test -p doslet-v86-host --locked --offline --test core -- --ignored
cargo test -p doslet-runtime --features v86 --locked --offline --test v86 -- --ignored
cargo clippy -p doslet-v86 -p doslet-v86-host --all-targets --locked --offline -- -D warnings
cargo check -p doslet-v86 --target wasm32-unknown-unknown --locked --offline
```

The first command regenerates the checked-in instruction tables and writes
`target/v86/doslet-v86.wasm`. It runs the isolated upstream compilation through
Cargo and uses the Rust build tool itself as the linker wrapper. After Cargo
dependencies are available, append `--offline` to the first command too.
The desktop's default features include `v86`; builds without default features
need `--features v86`. The launcher looks for `doslet-v86.wasm` beside the
executable, then `target/v86/doslet-v86.wasm` relative to the working directory.
Use `--v86-module PATH` to select another location. A missing module fails
the launch with the build command.
The RAM-wait boundary requires module ABI 4. Rebuild the WASM module after
updating Doslet; older modules are rejected at launch.
The smoke program verifies `INC AX; LOOP; HLT`, including nonzero actual JIT
entries. It is a synthetic CPU test, not a DOS game launch.

Integration tests are explicitly ignored in an ordinary workspace test run
because their generated WASM fixture must first be built. The command above
enables them; a missing fixture is an error, not a silently skipped check.

## Ownership and execution

`Core::compile` consumes the module bytes produced by the pinned builder.
It opens no files and can instantiate independent CPUs. Each `Cpu<B>` owns one
WASM memory and a host bus implementing `doslet_bus::CpuBus`. Ordinary guest
RAM resides only in that memory. `ram` borrows a view; `write_ram` invalidates
the upstream JIT before writing. Host callbacks forward port widths and
mapped memory operations to `B`; bus errors stop execution explicitly.
The VGA aperture is excluded from ordinary RAM views.

`PcMachine::enable_v86` migrates the initial RAM once. Subsequent CPU, DOS,
DMA and host-service accesses borrow that same backing. During synchronous
CPU execution, `DeviceHost::access` borrows the suspended instance's actual
RAM and the Rust device owner. The host adapter confines the lifetime-erased
native pointer to one internal module; public callers cannot retain or move
the installed borrow. It clears the borrow on success, error and unwind.
Small architectural values are exported at scheduling boundaries for runtime
services. Context is imported only when those services change it. Ordinary
batches continue in v86, including its own interpreter for untranslated code;
the Doslet interpreter does not execute the remainder.

With an entered runtime DPMI host, CPU exceptions exit v86 before IDT lookup
and before pushing an interrupt/error-code frame. The native owner exports the
faulting context and passes the exception vector and optional 32-bit error code
through the same DPMI exception path as the native CPU. Registered `0203h`
handlers receive the locked-stack exception frame; absent/nested handlers stop
at the original fault. Software INT instructions and externally acknowledged
PIC vectors retain ordinary interrupt delivery. Without runtime DPMI, v86
continues to deliver exceptions through the guest's IDT. The interception flag
is cached and changes only with the owning host's state.

The current module uses v86 wire ABI 4. Rebuild the executable and WASM module together;
an older module is rejected at instantiation rather than run with incompatible
exception handling.

`MOV SS`, `POP SS` and `LSS` now retain the one-instruction interrupt shadow
across the WASM/host boundary. The next guest instruction executes before the
external PIC or mouse IRQ can be accepted. A real-mode regression checks this
for both the interpreter and JIT execution policy.

`reset_real_mode` initializes integer state and invalidates old compiled
bindings without replacing RAM. Protected-mode transitions execute as guest
instructions. `Execution::Interpreter` uses v86's interpreter;
`Execution::Jit` uses its interpreter and hot-code compiler. `prepare_current`
can request compilation outside ordinary execution. Compiled module counts
show preparation; `jit_entries` shows actual generated-code execution.

Attached machines compile generated modules on one background worker. The
worker receives only WASM bytes and the engine; CPU state, devices and RAM
stay with the emulation owner. At most one job is outstanding, matching v86's
asynchronous compilation protocol. Until it finishes, v86 interprets the code.
The owner installs completed functions at execution boundaries and honors code
invalidation while compilation was pending. `prepare_current` and
`wait_for_compilation` are explicitly blocking setup/diagnostic operations.
Native compilation units cover one guest page, with at most 32 extra basic
blocks, to reduce Cranelift compilation latency.

The default machine clock requests at most 4096 execution units per call. Generated basic blocks
must fit the remaining grant; v86 interprets short remainders. Machine grants
also end before the next scheduled device event. Generated-loop checkpoints
and interpreter chunks use 256-instruction limits.
REP yields after one element. STI may execute the following instruction inside
the same interpreter call to preserve its interrupt shadow. Attached-machine
mode returns before I/O and control/interrupt boundaries, commits preceding
hardware time, then executes the boundary instruction through v86. Software
interrupt dispatch returns at the handler entry, allowing the runtime's DOS
shim to intercept INT 21h while preserving guest-installed vectors.
Early exits remove the unexecuted suffix of a precharged JIT block. A store
retried after an SMC exit is counted only when it executes; architectural fault
delivery retains one attempted CPU execution unit.
Ordinary extended instructions such as MOVZX do not end a machine grant;
system/time instructions such as RDTSC retain their boundary.

`--v86` defaults to maximum CPU throughput with device time supplied by
the native owner's monotonic clock. `--v86-host-clock` remains an explicit
alias for this default. `--v86-instruction-clock` selects instruction-based
timing for comparisons and allows `--cpu-mhz`. Native JIT/AOT,
execution tiers, core profiles and automatic idle/analysis policies are disabled
when launching v86. The host-clock mode rejects `--cpu-mhz`; its CPU panel
shows maximum throughput without MHz controls.
Runtime policy setters also keep native JIT, idle observation and tracing-based
controllers disabled while v86 is attached, including when a profile or panel
reapplies settings after launch.

The host-clock path recognizes one idempotent 32-bit RAM timer-polling loop
without game-specific addresses. It can park that loop while the owner advances
devices and services IRQs. The native desktop CPU panel provides a live switch
and park counter. [Idle detection and waits](idle-detection.md) documents the
recognizer, its guards and the distinct native methods.
The CPU panel also exposes v86 DOS keyboard polling. On a stable screen,
repeated empty INT 21h/AH=0Bh queries can yield to the host clock until input
or the next device deadline, capped at 16 ms. This is an optional live switch
and a timing heuristic rather than a full idle proof.

In host-clock mode, a runtime batch contains at most 4,096 execution units
and 128 execution iterations. Both limits matter: a long REP/MMIO operation
can stay inside v86, while segment/service boundaries can cause many short
returns to Rust. CPU work does not advance device time. Long batches used to
merge PIT edges in the PIC before the owner could service them, slowing CIV's
BIOS ticks, animations and MIDI despite device time tracking host time.
Between grants the owner advances devices toward elapsed host time in slices
of at most 1 ms, split internally at audio samples, DMA and timer deadlines.
Port I/O also yields the runtime batch to this owner, so short PIT calibration
and MPU-401 write-ready polls observe advancing time. MMIO/framebuffer writes
do not force this additional host yield.
The existing PIC queues resulting IRQs until the CPU can accept them. During
catch-up, the owner runs the CPU when an IRQ is ready or IF is clear, allowing
interrupt handlers to finish before advancing more device slices. It skips
CPU work only with IF set and no pending IRQ; elapsed PIT edges therefore do
not collapse into one request across a host stall. HLT yields to the owner so
elapsed time can wake it through an IRQ. Pausing and
resuming rebases the host anchor; paused time is excluded.

This mode intentionally changes the relationship between instruction counts
and guest time. Device I/O sees time committed at CPU boundaries; it is not
cycle accurate or deterministic. Long CPU calls or compilation can delay
device service. Audio sample-clock tests do not establish audible quality in
every game. The graphics toolbar reports presented FPS separately from VGA Hz.

MIDI synthesis runs on the separate `doslet-audio` thread. Its input horizon
still follows guest device time, so a slow instruction clock can starve output
even when MIDI bytes and notes arrive. The local CIV app comparison exercises
the real synthesis worker and output queue with a simulated 48 kHz consumer:
it checks native and instruction-clock v86, then compares host-clock v86 with
and without the output reserve, using CM-32L ROMs. It reports missing frames
and zero runs after the first signal. Run it with local data and ROMs available:

```sh
cargo test -p doslet --release --locked --offline local_civ_app_audio_matches_between_cpu_backends -- --ignored --nocapture
```

This establishes generated output, not physical speaker playback.

Host-clock output waits for 80 ms of queued samples before starting playback
and after an empty sink. This reserve absorbs short scheduling delays without
advancing guest hardware ahead of host time or changing samples. Pause/reset
rearms the reserve; instruction-clock output keeps its existing pacing.
The CPAL callback holds that reserve in the application queue. PulseAudio
uses an 80 ms stream target and prebuffer for both CPU backends, and explicitly
keeps each new/reset stream corked for its initial 80 ms while PCM accumulates.
The target excludes device latency (`adjust_latency=false`); its source
serves available chunks immediately. Format transitions drain and resume without
adding another startup delay. Holding 80 ms in the source while asking
Pulse for a 30 ms buffer would repeatedly starve server requests. These policies
add playback latency; a delay longer than the reserve can still interrupt sound.

The CPAL queue counts missing frames after playback begins, including refill
waits; intentional initial buffering is excluded from its underrun counts.
Two local eight-second CIV comparisons with the reserve reported zero output
underrun frames; the unbuffered reference had gaps after music began. These are
native Wasmtime queue measurements, not a physical listening test or browser
result. The shorter comparison can be repeated with:

```sh
cargo test -p doslet --release --locked --offline local_civ_host_clock_audio_reserve_comparison -- --ignored --nocapture
```

Host time is explicit via `set_time_microseconds`; the core does not read a
wall clock. `inject_interrupt` accepts an already acknowledged vector when IF
permits it, including waking HLT. The external machine owns its PIC and timing.
Unknown host operations fail. A WASM trap or bus failure during execution makes
that instance unusable; recreate it rather than resuming partially committed
upstream state.

Wasmtime fuel instrumentation is disabled. The explicit guest instruction
grant still bounds normal CPU execution, but there is no independent fuel or
epoch interruption of a malfunctioning WASM call or hot-code generation.
Memory is limited to
256 MiB per store; supported guest RAM is 1-64 MiB. Function tables have at most
2048 slots. Wasmtime retains generated instances after upstream table eviction,
so the host allows at most 256 generated modules per CPU lifetime and then
disables further compilation. Already compiled code and v86 interpretation
remain available. Exhaustion is visible in `Statistics`; reset does not renew
the lifetime allowance. These bounds do not establish allocation-free execution
or measured performance.

## Validation scope

The opt-in native tests cover real-mode arithmetic, flags, stack/RAM effects,
HLT, x87 arithmetic, one-element REP in RAM and mapped device memory, comparison
flags, host port dispatch including PIC ports, error propagation, independent
instances, host-write invalidation, actual JIT execution, external IRQ/IRET,
guest self-modification of active code, and a guest transition into protected
code32 followed by JIT execution.
Selected integer operations are compared instruction by instruction against
Doslet's interpreter. Another test compares complete ordinary low RAM and
integer state after a bounded loop through v86's JIT and interpreter.
Session tests compare the complete VGA backing after the color-bar COM demo,
including BIOS mode setting and DOS exit, exercise MPU-401 output through Rust,
and verify actual JIT execution without context reimports on ordinary batches.
The DPMI regression enters a protected-mode client and returns from INT 31h
through an LDT code selector. Context slots follow v86's ES/CS/SS/DS/FS/GS/TR/LDTR
order; TR and LDTR must not be exchanged.

The opt-in local Quake test runs the version-2 profile with temporary overlays
and preloaded media, without a window or host audio:

```sh
cargo test -p doslet --release --locked --bin doslet quake_v86 -- --ignored --nocapture
```

On the local shareware image it completed 16 seconds of emulated time at the
profile's 266 MHz, with nonzero actual JIT entries and a captured main menu.
The native test took approximately 12 host seconds after the integration
changes, versus 59 seconds with synchronous multi-page compilation and broad
extended-opcode boundaries. This includes startup; it is not a frame-rate
benchmark.
This verifies a bounded real-game startup run, not interactive gameplay or
real-time performance. Hot-code generation runs inside the WASM call; guest
execution retains its separate instruction grant without Wasmtime fuel
instrumentation. CPU execution failures preserve the full upstream cause
for the desktop notice and stderr.

`IRET` validates null/out-of-table CS selectors, code type and privilege,
presence and the destination offset before consuming the return frame. Invalid
selectors/rights deliver guest `#GP(selector)`; null selectors and an out-of-limit
IP deliver `#GP(0)`. A valid code descriptor without its present bit delivers
`#NP(selector)`. Descriptor lookup requires all eight bytes to fit the table.
The upstream branches previously panicked for several of these cases, including
`not present` and `Unimplemented: CS selector is invalid`.
The core regression covers 48 cases across 16/32-bit returns and both execution
policies, verifying exception codes and preservation of the original frame.

The first rejected return records a bounded `IRET fault:` diagnostic with its
instruction address, operand width, return CS:IP/flags, SS:ESP/cache and GDT/LDT
bounds. The host preserves it if a later exception-delivery panic overwrites
the ordinary upstream message; a regression checks that error chain too.
Only rejected returns produce this diagnostic. These changes remove the tested
WASM panics. A later PRINCE replay traced a different corrupt return to the
DPMI host's locked-stack segment: its 16-bit handler copied the frame through
16-bit `SI/DI` while `SS` was flat and `ESP` exceeded 64 KiB. The dedicated
stack segment now lets that handler return in the scripted headless run; see
[PRINCE](prince.md#v86-startup-probe).

These checks do not establish complete CPU,
paging, exception, SSE or x87 conformance; PC timing; save-state interchange;
browser integration or interactive gameplay.

## Native performance reproduction

The diagnostic runner restores identical portable states, uses temporary disk
overlays and hashes RAM, VRAM/scanout, palette, audio and CPU values after a
bounded instruction window. It records outstanding compilation drain time
separately from execution time, and waits before dropping each owner so jobs
from one repeat do not leak into the next. Optional profiling timers do not
drive guest hardware time.

```sh
cargo run -p doslet-headless --release --features v86-test --locked --example v86_profile -- --config games/Quake.toml --state target/quake-x87-demo.state --steps 20000000 --repeats 3
```

The state is a local fixture from the Quake diagnostic workflow, not a bundled
asset. Omit `--state` to compare launches from the loaded executable; add
`--warmup-steps 100000000` to measure a later window after code preparation.
Historical native measurements of the local saved Quake window (three repeats)
used fuel instrumentation and predate the host-clock mode:

| Execution path | Median for 20 million scheduling steps |
| --- | ---: |
| Previous v86 integration, cold generated code | 9.18 s |
| Background compilation and precise boundaries, cold code | about 0.45 s |
| Updated v86, after 100 million warmup steps | about 0.26 s |
| Rust interpreter, same later window | about 0.19 s |

The cold window's integer registers, flags, guest time, RAM, visible pixels
and audio matched across the before/after runs. The later comparison also
matches VRAM, palette and IRQ counts. These are native Wasmtime measurements,
not browser/WASM results or a claim that v86 beats the Rust interpreter on
every workload. Raw x87 saved environments differ between the two CPU
implementations; this diagnostic does not establish complete CPU equivalence.

The desktop defaults to host-clock v86 execution, with an optional instruction
clock.
Native AOT procedure modules,
speed tiers, recurrence/idle tracing and GDB attachment are incompatible with
this mode. The CLI rejects explicit conflicting policies and turns off
automatic AOT/tier activation. Saving a v86 session fails explicitly because
the portable state format does not yet include all upstream architectural
state. Existing native snapshots can be loaded before enabling v86.

The local tempo regression runs CIV for 30 guest seconds on the native and
host-clock v86 backends, through the internal mixer. After five startup seconds,
it requires v86 BIOS ticks and delivered IRQs to reach at least 95% of the
native cadence. Unlike output continuity checks, this detects a slow guest
clock even when samples are present and device nanoseconds follow host time.
A local native Wasmtime run recorded 544 BIOS ticks and 74 notes with the
bounded batches/catch-up, versus 549 ticks and 75 notes on the native CPU over
30 seconds. Before the scheduling fix, v86 recorded 359 ticks and 23 notes
while still reaching 30 seconds of device time with no output underruns.
These measurements use the internal mixer, not physical system-mixer playback.

```sh
cargo test -p doslet --release --locked --offline local_civ_tempo_diagnostic -- --ignored --nocapture
```

The upstream code, licenses and local changes are recorded in
[vendor/v86](../../vendor/v86/README.md). Its `std`, unsafe Rust and C code stay
inside the isolated WASM build. Existing emulation crates retain their current
`no_std` boundaries; the host adapter is native platform code.
