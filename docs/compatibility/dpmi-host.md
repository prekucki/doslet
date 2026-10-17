# Experimental DPMI host

Doslet can provide a DOS Protected Mode Interface (DPMI) host to DOS/4GW.
The extender discovers it through INT 2Fh/AX=1687h and delegates descriptor,
memory and real-mode services to the runtime. Native desktop launches enable
the host in `direct` mode by default. Use `--experimental-dpmi off` to select
the guest extender's own host.

The experiment has two modes:

- `reflect`: run real-mode services through guest interrupt vectors and stacks.
- `direct`: dispatch selected unchanged Doslet BIOS/DOS shims directly from
  protected mode. Other services and guest-installed handlers use reflection.

Both modes expose allocation telemetry. The host uses fixed metadata and
identity-mapped guest RAM; guest allocation requests do not allocate host memory.
Guest executables and DOS4GW.EXE remain ordinary input files.

## Run and compare

From the experimental checkout, launch a native window with:

```sh
cargo run -p doslet --release --locked -- \
  --game /path/to/PRINCE.EXE --experimental-dpmi direct
```

Use `--experimental-dpmi reflect` for the reference host and
`--experimental-dpmi off` for the original extender path. The setting is a native
launch option, not a saved machine-profile field or a browser UI option. Native JIT and its interpreter
fallback retain their existing controls.

The headless example prints live blocks, requested/committed bytes, peaks,
failed requests, direct/reflected calls and unsupported DPMI function numbers:

```sh
cargo run -p doslet-headless --release --no-default-features --locked \
  --example dpmi_game -- /path/to/PRINCE.EXE 100000000 direct

cargo run -p doslet-headless --release --no-default-features --locked \
  --example dpmi_game -- /path/to/PRINCE.EXE 100000000 compare

cargo run -p doslet-headless --release --no-default-features --locked \
  --example dpmi_game -- /path/to/HEROES.EXE 100000000 compare /path/to/homm1.gog
```

`compare` checks the reflected and direct hosts at completed service boundaries
at million-step checkpoints and at the end. It compares integer registers and
segment caches, control registers, flags, logical steps/time, all exposed RAM
outside the VGA aperture, VRAM, scanout/palette and console. It also compares
MIDI/PCM counts and streaming hashes. It does not compare every internal device
field or prove that unvisited code behaves identically.
The comparison also checks x87 state and, in profiling builds, machine execution
counters; JIT/decode-cache statistics are expected to differ.

Use `bench` in place of `compare` for a warmup pair followed by three pairs in
alternating order. Loading and state comparisons are outside the timed region.
Each pair must have the same final state. Run without concurrent builds or
other emulator instances. The default example explicitly uses the interpreter.
For a native JIT comparison, retain default Cargo features and append
`- '' jit` after the mode, or `ISO '' jit` for a CD game. Its timing includes
synchronous compilation; it is not a measurement of the desktop background
compiler or browser execution. The optional key string uses `|` for Enter.

All example runs keep guest writes in the existing RAM overlay. They neither
persist saves nor open a window/audio output device.

To check background speed-tier learning against local game assets:

```sh
RUST_MIN_STACK=16777216 DOSLET_DPMI_GAME=/path/to/PRINCE.EXE \
  cargo test -p doslet --locked local_dpmi_game_learning -- --ignored --nocapture
```

For a CD game, also set `DOSLET_DPMI_CD=/path/to/image.iso`. This test profiles
startup and active-DPMI working copies for 100 ms of guest time at 10 and 30 MHz,
requires learned labels, and checks that the live step count and DPMI statistics
remain unchanged. It does not open a window or validate interactive gameplay.

## What the memory counters mean

`Session::dpmi_stats()` reports current and peak bytes requested through DPMI
0501h/0503h, page-rounded committed bytes, successful allocation/free/resize
counts and failed requests. `Session::dpmi_blocks()` exposes the remaining
handle/address/requested/committed records without copying guest buffers.

These are extender allocations, not the game's minimum RAM requirement or
working set. A game can reserve almost all available RAM and manage its own heap
inside that reservation. The counters exclude conventional DOS allocations,
host tables/stacks, VRAM and emulator/JIT memory. Failed requests include size
probes; a failure alone does not establish that the game ran out of memory.
Records remain available after exit until the session is destroyed/reset.
Determining a minimum requires separate runs with smaller guest RAM and the
same actual gameplay workload. Working-set measurements require memory-access
instrumentation in addition to these allocation counters.

## Implemented boundary

The host implements the subset exercised by the local 32-bit DOS/4GW clients:
LDT descriptors and aliases, conventional DOS blocks, interrupt vectors,
real-mode interrupt/IRET/far calls with zero stack-copy words, callbacks,
raw mode-switch entry points, basic memory information/allocation/free/resize,
resident-memory lock calls and virtual interrupt-state queries.

Direct dispatch currently covers selected VBE/palette, mouse, nonblocking
keyboard and DOS status/query services. Both the target address and runtime
shim signature must match. Blocking calls, arbitrary real-mode procedures and
changed guest handlers execute through reflection. An optimized call preserves
the scratch IRET frame, returned registers/flags, logical time and bounded
execution budget. A replaced real-mode IRET consumes one machine timing step;
the protected-mode IRET still executes normally.
Debugger attachment selects reflection for subsequent calls and restores the
selected mode on detach.
Observed analyzer steps use reflection to retain the actual bus transactions.
Address-specific speed labels support direct dispatch: its replacement step
is charged to the current protected-mode dispatcher address and code width.
The following protected IRET is charged separately at that same address.
Changed or missing code labels retain the existing GREY fallback. Reflected
execution charges the real IRET at its real-mode address; comparisons with
different labels at those two addresses can therefore have different timing.
Background speed-tier learning supports both modes. Its working copy retains
the DPMI host, allocation metadata and pending callback/return frames, and uses
the captured dispatch mode. The native default is direct in both the live game
and its profiling copies.

The CPU exposes a small integer context save/restore boundary without depending
on DOS, BIOS or the runtime. The runtime owns host state. Context restoration
reloads hidden segment caches and invalidates code-context hints even when the
numeric selector is unchanged. Floating-point state remains owned by the CPU.

Mouse events received while the client executes protected code are delivered
through vector 74h and the host's real-mode reflection path. The driver wrapper
can call a registered DPMI callback and then restore the interrupted client.
Regression tests cover direct/reflect delivery, event coordinates, masked-interrupt
deferral, nested callback return, and preservation of registers, flags and stack.
The reported PRINCE regression was observed live: driver coordinates changed
while the menu cursor remained stationary; disabling the host restored movement
according to the user. The corrected binary still requires a fresh game run.

This is an experimental subset, not a conforming general-purpose DPMI server:

- Portable save states reject sessions with this host enabled: its descriptors,
  allocation metadata and pending callback contexts do not yet have a portable
  state representation. In-process diagnostic working copies preserve these
  fields independently. Ordinary sessions keep their existing file save/restore
  support.

- Paging, demand commitment, VM86 and privilege-changing gates remain absent.
  Clients execute at CPL 3 with IOPL 3 under the existing CPU model.
- Exception vectors can be registered, but CPU faults still stop with the
  existing explicit diagnostic; protected exception delivery is not implemented.
- Real-mode stack-argument copying (`CX != 0`) is rejected. Metadata is bounded
  to 8191 allocatable LDT entries, 512 memory blocks, 32 callbacks and 16 nested
  host frames. Nested real-mode calls have 1 KiB private stack slices.
- Resizing only succeeds in place. Large tiled conventional allocations for
  16-bit clients are rejected; 16-bit-client compatibility is unvalidated.
- Unsupported functions return DPMI carry/error status and are counted.
  Observed optional probes include 0A00h, 0702h and 0506h.
- External hosts, nested clients, extender replacement after entry and resource
  teardown followed by another protected client in the same session are outside
  the validated scope. There is no mid-run fallback to the original DOS/4GW host.

## Recorded local evidence

On 2026-10-17, native interpreter comparisons ran the local PRINCE and Heroes
executables for 100,000,000 logical steps each with 16 MiB guest RAM. Both reached
VBE mode 101h and remained running. Both matched the comparison fields at 100
checkpoints and final state. These are bounded startup/intro runs without user
input, not interactive gameplay or audible-playback acceptance.

The same 100-million-step comparisons passed with the native JIT and profiling
enabled for both games, including machine counters and x87 state. PRINCE's
direct run executed 50,903,953 JIT steps; Heroes executed 40,625,438. Both reported
zero compilation failures. WASM compilation is checked, but browser execution
and browser performance with this host remain unmeasured.

| Workload | Direct calls | Reflected calls | Current requested bytes | Peak requested bytes |
| --- | ---: | ---: | ---: | ---: |
| PRINCE | 177 | 1084 | 15,441,696 | 15,441,696 |
| Heroes | 361 | 1528 | 4,334,064 | 15,565,296 |

PRINCE retained 15,454,208 committed bytes in 11 blocks. Heroes retained
4,345,856 committed bytes after 32 allocations and 18 frees; its committed peak
was 15,577,088 bytes. These observations illustrate why a peak allocation is
not a minimum-memory measurement.

Synthetic regressions cover direct/reflected result and memory equivalence,
custom real stacks, modified vectors/shims, callback return structures,
descriptor exhaustion, allocation/resize failures, context restoration and
zero host allocations during repeated service calls after setup.

For the same 100-million-step workload, native release interpreter timing after
one warmup pair and three alternating measured pairs was:

| Workload | Reflect median | Direct median | Direct / reflect |
| --- | ---: | ---: | ---: |
| PRINCE | 861.350 ms | 851.948 ms | 0.9891 |
| Heroes | 1512.452 ms | 1516.300 ms | 1.0025 |

Every measured pair had the same final comparison state. Individual timings
overlapped (PRINCE: 836–863 ms reflected, 850–861 ms direct; Heroes: 1508–1517 ms
reflected, 1510–1519 ms direct). This does **not establish a game-level speedup**.
Only hundreds of eligible calls occur in these 100-million-step runs; the main
demonstrated results are service equivalence and allocation visibility.

Validation commands passed: workspace tests, Clippy with warnings denied,
format checking, and WASM checks for the desktop library, executable/time
libraries and web worker/audio crates. Workspace tests that open loopback
HTTP/GDB/MCP sockets required execution outside the restricted sandbox.

The ABI reference is the [DPMI specification](https://www.delorie.com/djgpp/doc/dpmi/),
particularly [host discovery](https://www.delorie.com/djgpp/doc/dpmi/api/2f1687.html),
[memory allocation](https://www.delorie.com/djgpp/doc/dpmi/api/310501.html) and
[callbacks](https://www.delorie.com/djgpp/doc/dpmi/api/310303.html).
