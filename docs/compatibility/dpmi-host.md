# DPMI host

Doslet can provide a DOS Protected Mode Interface (DPMI) host to DOS/4GW.
The extender discovers it through INT 2Fh/AX=1687h and delegates descriptor,
memory and real-mode services to the runtime. Native desktop launches and
browser bundles enable the host in `direct` mode by default. Use `--dpmi off`,
or `machine.dpmi = "off"` in a profile, to select the guest extender's own host.

The host has two modes:

- `reflect`: run real-mode services through guest interrupt vectors and stacks.
- `direct`: dispatch selected unchanged Doslet BIOS/DOS shims directly from
  protected mode. Other services and guest-installed handlers use reflection.

Both modes expose allocation telemetry. The host uses fixed metadata and
identity-mapped guest RAM; guest allocation requests do not allocate host memory.
Guest executables and DOS4GW.EXE remain ordinary input files.

## Run and compare

Launch a native window with:

```sh
cargo run -p doslet --release --locked -- \
  --game /path/to/PRINCE.EXE --dpmi direct
```

Use `--dpmi reflect` for the reference host and `--dpmi off` for the original
extender path. Profiles record the choice as `machine.dpmi`, the Machine page of
the settings offers it, and `--dpmi` overrides it for one launch. Earlier
commands using `--experimental-dpmi` keep working. Native JIT and its
interpreter fallback retain their existing controls.

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
real-mode interrupt/IRET/far calls, callbacks, raw mode-switch entry points,
basic memory information/allocation/free/resize, resident-memory lock calls,
paging hints and virtual interrupt-state queries.

- Real-mode calls (0300h–0302h) copy up to 256 words (`CX`) from the client's
  stack, above the INT 31h frame, to the real-mode stack below the return
  address, as arguments; they are not copied back. Calls with arguments always
  reflect instead of using direct dispatch.
- 0503h resizes in place when the following pages are free and otherwise moves
  the block to the lowest gap that fits, copying its bytes, as DPMI allows. The
  handle stays the same. DJGPP's `sbrk` grows its heap this way. The copy is a
  single host move bounded by installed RAM, not an unbounded guest operation.
- 000Dh allocates a specific descriptor among LDT entries 1–15 (selectors
  04h–7Ch); 0000h and the host's own aliases start at entry 16, as DPMI 0.9
  reserves those entries.
- Without paging every page is resident, so 0600h–0603h and the paging hints
  0702h and 0703h succeed without effect.

### Exceptions

A client handler registered with 0203h receives the client's divide errors
(#DE), not-present (#NP), stack (#SS) and general-protection (#GP) faults with
their error codes, as DPMI 0.9 describes. The host builds the frame on a locked
16 KiB stack below its frame stacks: return CS:EIP, error code, the faulting
CS:EIP, EFLAGS and SS:ESP, as dwords for 32-bit clients and words for 16-bit
ones; only the 32-bit form is tested. The handler runs with interrupts disabled. Its far return resumes the
client at the frame's CS:EIP, EFLAGS and SS:ESP, which it may change, for
example to skip the instruction; IOPL stays the host's. Portable states keep a
running handler.

Faults are precise while any handler is registered: the CPU restores the
general registers, EIP, EFLAGS, CPL and the CS and SS caches from before the
instruction. Memory written before the fault, such as stack slots below ESP,
stays written. DOS/4GW registers handlers in ROTT, Heroes and PRINCE, so
precise faults are on there; over 30 guest seconds this cost about 0.8% in ROTT
(5.64 s against 5.60 s), within run-to-run variation, and nothing measurable in
Heroes. Unimplemented instructions, paging and bus errors are Doslet
limitations, not guest faults, and still stop the session.

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

The BIOS timer's real-mode `INT 1Ch` now invokes an installed protected-mode
handler before completing the real-mode return. This implements the timer case
of the [DPMI software-interrupt reflection contract](https://www.delorie.com/djgpp/doc/dpmi/ch4.4.3.html).
The runtime keeps the interrupted real-mode context on its bounded host frame
stack, uses a private protected stack, and prevents a handler chaining to the
default reflector from recursively invoking itself. The return restores the
real-mode registers, flags and stack before executing the original IRET.
This path requires the unchanged default BIOS `INT 1Ch` shim; reflection of
`INT 23h` and `INT 24h` is not needed: Doslet's DOS reports errors through
return codes and never raises either interrupt, so a client's handlers for them
are never called.

Hardware interrupts follow the same path. An IRQ that arrives in real mode
reaches its BIOS handler: IRQ 0, IRQ 1, or the default handler that the BIOS
installs for IRQs 2–15. IRQ 12's vector holds the mouse driver's wrapper. If
the client installed a protected-mode handler with 0205h, the host calls that
handler and returns through the BIOS handler's final IRET, skipping the BIOS
handler's own work. DJGPP programs hook IRQs only in protected mode, so without
this routing Quake lost keys pressed while it waited in DOS. Reflections of the
client's own interrupts, 0300h–0302h calls and chains to the default handler
leave the host's return address on the real-mode stack. They run the BIOS
handler, so a chained handler cannot recurse. A routed handler does not nest:
while one runs, further real-mode IRQs and ticks use the BIOS handlers.
Without a client handler, the BIOS default acknowledges the PIC and returns.
Before these defaults existed, those vectors were 0000:0000, and Quake's Sound
Blaster block IRQ executed the interrupt table.

Without that timer delivery, a PRINCE menu click left its PCM effect in the
continuous DMA buffer. Sound Blaster block IRQs were acknowledged on time, but
the effect repeated because the game's software mixer did not receive timer
ticks. A local interpreter reproduction produced 644 nonzero stereo frames for
the click, then repeated those samples with the host enabled. With timer
reflection, the same click produces 644 nonzero frames followed by silence,
including a further five-second observation window. This compares generated
guest PCM without a host audio device; it does not establish audible output
quality. The opt-in regression runs the menu sequence with the original DOS/4GW
host, reflected DPMI and direct DPMI:

```sh
cargo test -p doslet --release --locked local_prince_menu_pcm_and_keyboard -- --ignored --nocapture
```

Portable save states (version 4) carry the host's records; see
[save states](../save-states.md). The host is a subset aimed at games, not a
conforming general-purpose DPMI server. Remaining gaps:


- Paging, demand commitment, VM86 and privilege-changing gates remain absent.
  Clients execute at CPL 3 with IOPL 3 under the existing CPU model.
- A divide error without a 0203h handler, or inside one, becomes interrupt 0,
  as DPMI hosts reflect exceptions 0–7. It reaches the client's 0205h handler,
  or the host's gate reflects it to the real-mode vector, by default DOS's
  "Divide overflow" abort with exit code 0. Of exceptions 0–7, the CPU model
  raises only the divide error. Other faults without a handler, faults while a
  handler runs and faults in host code still stop the session with the existing
  diagnostic.
- Metadata is bounded to 8176 LDT entries for 0000h plus 15 for 000Dh, 512
  memory blocks, 32 callbacks and 16 nested host frames. Nested real-mode calls
  have 1 KiB private stack slices.
- Large tiled conventional allocations for 16-bit clients are rejected;
  16-bit-client compatibility is unvalidated.
- Unsupported functions return DPMI carry/error status and are counted.
  ROTT, Heroes and PRINCE probe only 0A00h (vendor extensions) and 0506h (DPMI
  1.0 page attributes), and continue after the error. 0800h (physical address
  mapping) is unsupported: the VBE 1.2 adapter has no linear frame buffer to
  map. Debug watchpoints (0B00h–0B03h) are unsupported.
- External hosts, nested clients, extender replacement after entry and resource
  teardown followed by another protected client in the same session are outside
  the validated scope. There is no mid-run fallback to the original DOS/4GW host.

## Recorded local evidence

On 2026-10-20, a local Quake v1.06 shareware run (DJGPP, normally with
CWSDPMI) used `--dpmi direct` and the interpreter without AOT. It ran for 60
emulated seconds. Space was pressed at 3 s to dismiss the "MSCDEX not loaded"
notice, and ESC at 12 s to skip joystick calibration. Quake then played its
demo behind the main menu with no fault, after 3,990,010,000 steps in 87 host
seconds. Reaching that point needed these changes, in order:

- MZ loading of the DJGPP stub;
- segment reloads after descriptor changes;
- DOS 33h/58h;
- the DOS SFT and JFT for `fstat()`;
- TRUENAME (60h);
- the IPX probe;
- IRQ defaults and routing;
- IOCTL 4409h;
- an SDA refusal (5D06h);
- BT/BTS/BTR/BTC with a register bit offset.

This is a menu-and-demo run without gameplay input, audio listening or the
desktop application.

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
