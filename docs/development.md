# Developing Doslet

Validation commands, implementation status, headless diagnostics and
execution-engine details. For everyday use, see [using Doslet](usage.md).

## Check the work

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check -p doslet -p doslet-time -p doslet-executable --target wasm32-unknown-unknown --locked
cargo check -p doslet-web-worker -p doslet-web-audio --target wasm32-unknown-unknown --locked
cargo run -p doslet-headless --release --locked -- bench --steps 5000000
cargo run -p doslet-headless --release --locked --example bench_dynamic
```

The benchmark reports host execution steps per second, not emulated MHz or a
game performance claim. REP iterations count as separate execution steps so a
batch stays bounded.

`bench_dynamic` compares five million synthetic protected-mode register and RAM steps
against the cached interpreter in three alternating pairs after warmup, verifies
registers/flags/time and exposed RAM/VRAM, and reports median execution and compilation separately.
It is not a game benchmark. For the local PRINCE JIT
comparison, use the ignored test
`local_prince_dynamic_core_matches_reference_during_execution` in place of the
decode-cache test above. Set `DOSLET_PRINCE_EXE` when the installation is elsewhere.

To count native exit reasons and addresses separately from timing, run:

```sh
cargo run -p doslet-headless --release --features profiling --locked --example profile_dynamic_exits -- /path/to/PRINCE.EXE 1000000000
```

This opt-in diagnostic validates state against the cached interpreter at each
100-million-step checkpoint and writes metadata CSV. It is not a throughput
benchmark. See [native exit measurements](compatibility/native-core.md#measured-native-exit-reasons).

After building the web bundle, run the optional real-browser check with Firefox
and Python's standard library (no Node, Selenium or package installation):

```sh
python3 tests/browser_smoke.py
python3 tests/browser_smoke.py --prince
python3 tests/browser_components.py
```

The second command needs the local `target/prince-game.zip` created by
`mise run package-prince`. The isolated headless browser mutes its audio sink;
it checks nonzero rendered samples, ZIP execution, transfer channels,
pause/resume/reset, output recovery, and continued audio/CPU progress during an
800-ms window-thread stall. Web MIDI uses a mock device, including a port without
`clear()`; MT-32 checks cover automatic upgrade, translation, RPN replay and raw
SysEx output with explicit permission; this does not establish playback on a real external synthesizer.

The component check also needs `openssl`. It exercises real IndexedDB/Web Locks,
HTTPS Range/CORS, candidate configuration, DOS save acknowledgements and
save export/import/reset. A mocked Gamepad API drives the actual browser
transport, input router and guest gameport. The HTTPS fixture generates a
self-signed certificate in a temporary directory and uses an isolated Firefox
profile; it does not install certificates in system or user trust stores.
The temporary certificate, key and profile are removed when the test ends.

Capture the guest framebuffer without opening a window:

```sh
cargo run -p doslet-headless --locked -- demo --frame /tmp/doslet-demo.ppm
```

[`mise.toml`](../mise.toml) provides the same workflows as `mise run test`,
`mise run check`, `mise run demo`, `mise run run`, `mise run web` and
`mise run inspect-prince`. `mise run run` uses the optimized release profile;
`mise run run-debug` retains the debug profile for development.

## Implementation status

| Component | Implemented | Still needed |
| --- | --- | --- |
| x86 CPU | Selected real/protected-mode 386 operations, cached GDT/LDT segments, EIP32, 16/32-bit ModR/M/SIB, same-privilege gates, bounded REP | Full instruction/privilege coverage, paging, precise faults, timing |
| x87 | All documented 387 opcode handlers, raw real80/context transfers, extended-range arithmetic, BCD, root/remainders and bounded transcendental kernels | Full exception delivery, masked stack faults, some nonfinite transcendental operands; see the [audit](compatibility/x87-audit.md) |
| i8042 / CMOS | Controller/A20, keyboard command/ACK protocol, CMOS register bank and memory-size fields | Raw keyboard/mouse protocols and IRQs, RTC clock/IRQ behavior and persistence |
| PIC | Master CPU interrupt delivery; independent slave register bank at A0h/A1h; masks, fixed priority and EOI | Cascading and remaining operating modes |
| DMA | Independent 8237 state, PC page registers, bounded byte/word RAM transfers and auto-init | Full arbitration/cascade timing, MMIO and memory-to-memory |
| PIT | Binary modes 0/2/3, latches, gates, channel-2 output at 61h, fractional guest clock input and IRQ0 | Modes 1/4/5, read-back, BCD and precise periodic load timing |
| VGA/VBE | Mode-13h and banked 640×480×8 pixels, palette writes, borrowed scanout, configuration readback, selected VBE 1.2 BIOS calls | Text/planar modes, register rendering effects, raster timing, more SVGA modes |
| DOS/BIOS | COM/MZ/overlay setup, bounded memory arena, immutable input files/directories, optional sparse RAM writes, timer/user-hook shims | Full DOS kernel, extender compatibility, raw mouse hardware, persistent storage |
| MPU-401 / MIDI | UART setup, bounded output at 330h/331h; independent procedural synth, guest-timed WAV, native and Web Audio output | General MIDI instrument fidelity, serial timing, IRQs and intelligent mode |
| Sound Blaster 16 | Independent DSP/mixer, selected DMA PCM playback, auto-init/pause/resume, block IRQs and bounded output queue | More commands, precise timing, mixer gain, OPL and host audio |
| Frontends | Shared iced desktop/web view, native `--game`, browser Worker/AudioWorklet, ZIP mount/program selection, optional system MIDI on both platforms, headless CLI | Guest input, native CPU thread runner |
| Executable inspector | Borrowed MZ metadata/relocations and LE/LX header detection | LE/LX loading and execution |

## Component layout

```text
crates/
├── foundation/          bus contracts and integer emulated time
├── cpu/                 reusable x86 interpreter and architectural state
├── hardware/
│   ├── controllers/     independent PIC and PIT libraries
│   ├── audio/           independent MPU-401 UART
│   └── video/           VGA memory, palette and scanout
├── machines/            PC composition and memory/port routing
├── system/              executable inspection and DOS process setup
└── runtime/             bounded sessions and BIOS/DOS service adapters
apps/
├── doslet/              iced frontend and pixel presentation
└── doslet-headless/     demo, COM/MZ runners, inspector and benchmark
```

The portable emulation and MIDI synthesis libraries compile with `no_std`;
components that own buffers use `alloc`. The optional `doslet-x86-jit` native
compiler adapter uses `std` and executable memory. CPU execution uses static bus
dispatch. Scanout borrows device
memory, and RGBA conversion writes into caller-owned storage. A regression test
checks **zero heap allocations while running the demo and converting its frame
after setup**, and while executing the relocated MZ fixture with console output.
It also covers mounted-file reads, MIDI queue output, protected execution and
banked VBE pixels with full scanout conversion after setup.

The iced image adapter currently creates an owned RGBA image when the guest frame
changes. That presentation boundary has an explicit copy and allocation cost;
the emulator does not clone RAM or framebuffer storage per execution step.

## A local game target

Keep your copy of the game in `PRINCE/`, or use a symlink to its installed
directory. That path is ignored by Git. The project does not redistribute game
executables, assets, configuration or saves.

```sh
cargo run -p doslet-headless --locked -- inspect PRINCE/PRINCE.EXE
```

The supplied executable has a DOS MZ stub and an appended LE program. Together
with `DOS4GW.EXE`, this establishes that the target goes beyond the current
real-mode demo. The [compatibility report](compatibility/prince.md) records
the inspected file, evidence, unresolved requirements and acceptance stages.

## Headless runners

Run a standalone real-mode MZ program with a finite execution budget:

```sh
cargo run -p doslet-headless --locked -- run-mz program.exe --steps 200000
```

The loader validates the declared image, memory requirements, relocation targets,
entry and stack before guest writes. The CPU still supports a selected 16-bit
subset; unsupported instructions and services stop execution explicitly. Overlays
and NE/PE/LE/LX programs are rejected. The original test program in
[`tests/fixtures/relocated_mz.rs`](../tests/fixtures/relocated_mz.rs) uses a relocated
data segment to print `Relocated MZ` and exits through DOS with code 7.

Trace a DOS startup stub with its containing directory preloaded as C: input:

```sh
cargo run -p doslet-headless --locked -- run-stub PRINCE/PRINCE.EXE --steps 20000000
```

`run-stub` initially loads the MZ portion of an LE/LX program, supplies a DOS
environment and supports loading MZ overlays from that input drive. A guest
extender can then load the extended image through DOS services. The runner skips
symlink entries within the directory. Existing guest files may open read/write
using a 1-MiB sparse RAM overlay; original host files and buffers remain immutable.
`--trace N` prints at most N initial step snapshots; `--trace-tail` retains the
last 64 snapshots. `--peek HEXADDRESS:DECIMALLENGTH` prints a final borrowed RAM
view, limited to 128 bytes per range. The current local PRINCE run loads DOS4GW,
prints its banner and reaches the game's flat 32-bit initialization and device
probes. It now renders the HDI logo, introductory letter/map and main menu.
`--midi-log PATH` records the actual UART bytes with emulated nanosecond timestamps,
draining the bounded MIDI queue; it provides diagnostic evidence, not sound.
Without a MIDI consumer the queue eventually backpressures the guest.
`--midi-wave PATH` renders the guest UART stream into a 48-kHz stereo PCM WAV
without independently loading a MIDI file. Unsupported behavior still stops
with a diagnostic. The native frontend uses the same game loader; see the
[recorded startup result](compatibility/prince.md#doslet-startup-attempt).

## Profiling and execution engines

To measure execution separately from indexed-to-RGBA conversion:

```sh
cargo run -p doslet-headless --release --features profiling --locked -- run-stub PRINCE/PRINCE.EXE --steps 300000000 --profile-cpu
```

The optional feature counts decoded-instruction cache hits, misses, invalidations
and fallbacks, CPU/IRQ/HLT scheduling units, skipped VGA-wait units
and batches, and actual dispatched bus reads,
samples guest PCs at varying intervals, and drains MIDI without synthesizing it.
The timed CPU boundary excludes sampling, UI rendering and host audio. A game
still running at the finite step limit reports budget exhaustion and exits with
code 1 after printing its profile. Ordinary builds contain no profiling counters.
Interrupt-enabled HLT waits can advance together up to the next PIT/DSP deadline,
the remaining step budget, or 1 ms of guest time. Confirmed
`IN AL,DX; AND AL,08h; JZ/JNZ -5` and `IN AL,DX; TEST AL,AH; JZ/JNZ -5`
loops (AH=08h for TEST) on the active VGA status port can batch
whole iterations strictly before the next raster/PIT/DSP event; other polling
loops execute normally. Append `--no-vga-waits` to disable recognition and
batching for reference measurements. [The driver report](compatibility/prince-svga.md)
records the supported scope and differential checks.

For diagnostic inventories of game polling, the optional `inspect_civ` wait
tracer and Python/objdump scanner correlate backward branches with executed I/O.
See the [Heroes wait inventory](compatibility/heroes-waits.md) for commands,
addresses and the distinction between confirmed waits and ordinary repeated work.
The native `--experimental-heroes-timer-waits` flag optionally batches three
version-specific Heroes countdown waits. It is disabled by default; the terminal
reports the first actual batch. These sites were inactive in the sampled main
menu, so the flag does not promise a reduction in menu CPU usage.

Native x86-64 and AArch64 builds of `doslet` and `doslet-headless` enable the
`dynamic-core` Cargo feature by default. Cranelift compiles hot blocks of up to
16 register-only dword instructions in protected 32-bit code. A terminal relative
Jcc/JMP back to the block's start can repeat inside one native call, keeping
registers and flags local until the loop exits or its step budget runs out.
RAM traces can also follow relative branches and internal exits, with at most
16 decoded instructions in a 240-byte span. They support LEA, dword memory-left
CMP/TEST, byte MOV/CMP/TEST (including AH), PUSH/POP32, near direct/indirect CALL,
RET/RET imm16 and dword XCHG with memory. CALL/RET can remain inside the same
prepared graph; an external target returns to native dispatch. Data segments must be flat and readable;
stack operations additionally require a full flat SS with a 32-bit stack, and
CALL/RET require full flat CS. The PC exposes RAM from 500h with A20 enabled,
excluding the VGA aperture and IVT/BDA. Every complete access is bounds checked.
Byte/dword MOV stores continue natively in ordinary RAM with writable flat segments.
Stores update touched page generations and exit before modifying an active code
page. Segment safety is cached until a used selector or descriptor changes.
Other stores, I/O, REP, real-mode code and unsupported forms use the interpreter.
The scheduler combines native blocks and validated ordinary-RAM
interpreter work in slices of up to 256 steps, checking IRQs and updating clocks
and counters once per slice. Device access, mode/IF changes and hardware deadlines
end a slice. Code bytes/generations and CS bounds remain checked. WASM keeps its
interpreter and does not depend on Cranelift. See the
[measurement report](compatibility/native-core.md) for workload-specific
results, warmup comparisons and correctness evidence.

Use `--no-dynamic-core` in either native frontend to select the interpreter with
its decode cache and ordinary scheduling, or build with `--no-default-features`
to omit the native JIT.
Headless `--no-decode-cache` disables both engines' caches for the byte-fetching
reference. Native CLI output reports actual compiled and executed block counts.

Optionally reuse observed code on subsequent launches:

```sh
cargo run -p doslet --release --locked -- --game PRINCE/PRINCE.EXE --core-profile target/prince.core-profile
```

The same `--core-profile FILE` option works with headless runners. Profiles are
local hints containing bounded guest code bytes, never saved host machine code.
Keep them outside game directories and do not redistribute profiles of commercial
games. The desktop saves on pause, reset and normal shutdown; headless saves at
the end of a run, including budget exhaustion. Every profile block is decoded
again and checked against current RAM before execution. Relocated or changed
bytes fall back to interpretation. Automatic LE object/relocation analysis and
object-relative profile identities remain future work.

Compilation happens in explicit `Session::prepare_dynamic` maintenance calls
between execution batches, with at most two blocks per ordinary frontend call.
It may allocate and adds host latency; `Session::advance` remains allocation-free.
The code cache holds at most 4096 resident functions in 16 groups. When full,
preparation evicts the least recently used group and continues compiling;
hot groups remain usable. Genuine compiler failures stop further compilation
once, preserving interpreter fallback. See
[the native core contract](architecture.md#native-dynamic-core) for boundaries.

Selected interpreter instructions in default-32-bit code use a fixed single-instruction
decode cache shared by native and WASM builds. Page write generations, A20 mapping
and CS context validate entries. Other forms retain the interpreter; REP and IRQ
boundaries stay unchanged. Append `--no-decode-cache` for a reference run using
the same bus and generation tracking. This comparison measures the cache's
execution benefit; generation tracking also has a cost relative to older builds.

Run the opt-in local comparison through 300 million PRINCE steps:

```sh
cargo test -p doslet-headless --release --features profiling --locked local_prince_decode_cache_matches_reference_during_execution -- --ignored --nocapture
```

It compares registers, flags, control state and guest time after varied bounded
batches, all emitted MIDI/PCM, periodic RAM/scanout/palette snapshots and full
final ordinary RAM and all physical VRAM. This headless check does not measure UI, host synthesis or
browser throughput. Guest-PC sampling counts guest activity; it does not locate
host CPU time by emulator function.

Decode-cache profiling distinguishes CS context transitions and discarded entries
from page-write, mapping, bus-owner and access invalidations. These counters count
rejected entries, not writes to guest code. Collisions count lookups at occupied
slots with another key; evictions count successful replacements. Decode attempts
and failed decodes expose repeated unsupported-form probing. `fallback_hits` is
reserved for experiments and remains zero in the current implementation.
