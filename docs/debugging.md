# Debugging a guest with GDB

The native desktop and headless runners accept GDB remote connections over TCP.
They expose registers and ordinary RAM, instruction stepping and up to 64 address
breakpoints. The browser frontend does not expose GDB.

## Desktop: attach during a game

Open **Details → Enable GDB** to listen on `127.0.0.1:9001`, or select an address
at launch:

```sh
cargo run -p doslet --release --locked -- --config /path/to/heroes.toml \
  --gdb 127.0.0.1:9001
```

The game keeps running normally while the listener waits. Connect GDB when the
interesting behavior occurs; attachment stops the guest before the next step.
Use `continue` to play with GDB attached, Ctrl-C to inspect, and `detach` to
return control to the desktop. The Session panel displays the address and
connection state, and **Stop GDB** closes the listener/client. Detach or stopping
GDB restores the previous JIT/idle/VGA acceleration settings and resumes a guest
that was running at attachment. A guest that was initially stopped stays paused.
An unexpected broken connection or GDB `kill` leaves the guest stopped; the
window remains open. Reattachment is supported while the listener is enabled.

The emulation owner polls the protocol without waiting for input, including
incomplete packets; input, rendering requests and window shutdown keep working
while GDB is stopped. Storage workers can finish host operations, but their
completions stay queued until resume so they cannot change stopped guest registers.
Reset, UI stepping, run/pause and
machine replacement are refused while GDB controls execution. Keyboard/mouse
input remains available while continuing through the debugger. Guest clocks
freeze at stops, and desktop pacing restarts its host-time anchor on resume.

Normal execution and GDB execution use separate `tick_mode<M: Execution>`
instantiations. `Normal` calls the existing batch API directly; only the connected
mode checks individual instruction boundaries. The CPU instruction loop has no
new debugger hook, breakpoint check or function pointer. With GDB disabled there
is no listener or hotspot allocation. An enabled listener checks for attachment
between host work turns; it retains ordinary batches until a client connects.

## Headless: stop at an exact step

Build and stop Heroes after a repeatable startup:

```sh
cargo build -p doslet-headless --release --locked
target/release/doslet-headless run-stub games/HoMnM/HEROES.EXE \
  --cdrom games/HoMnM/homm1.gog --no-dynamic-core \
  --steps 400000000 --key-at 100000000:01:1B \
  --gdb-after 350000000 --gdb 127.0.0.1:9001
```

`--gdb-after` defaults to zero. `--steps` is an absolute execution limit and must
exceed the attachment step; its ordinary default of 200,000 is too small for
this example. `--key-at STEP:SCAN_HEX:ASCII_HEX` injects a BIOS key at the stated
step, here Esc to skip the introduction. Multiple scheduled keys are allowed.
`--cdrom` mounts a read-only ISO image as D:. The stub's existing RAM file overlay
keeps guest writes away from the installation. Debug mode drains MIDI output
even without `--midi-log`, so an unconsumed MPU queue cannot distort the analysis.

Connect from another terminal:

```text
gdb -nx -q
(gdb) target remote 127.0.0.1:9001
(gdb) set disassembly-flavor intel
(gdb) monitor state
(gdb) info registers
(gdb) break *0x219f90
(gdb) continue
(gdb) x/17i $pc
(gdb) stepi
(gdb) detach
```

While connected, `continue` executes through the interpreter one bounded guest step at a time.
Ctrl-C interrupts execution; `detach` resumes the ordinary headless runner with
its remaining budget. `kill` ends the run. Pausing for GDB freezes guest clocks.
JIT, idle-loop skipping and VGA polling acceleration are disabled upon connection.
The headless runner leaves these disabled after detach. This is a diagnostic mode, not a performance
benchmark. A step can be one REP element or an interrupt boundary, so a repeated
PC does not imply a useless loop.

## Address and register conventions

GDB `$pc`/`$eip`, memory addresses and breakpoints use **bus-linear addresses**:
cached CS base plus architectural EIP, before the PC bus applies A20 mapping.
`monitor state` prints both forms, code width, segment bases/limits, guest time,
step count and the last fault. General registers retain their architectural
values; for example, a segmented stack address requires adding the SS base to
the appropriate SP/ESP offset.

General registers, flags and PC are writable. PC writes must fit the current
CS window. Segment selectors and their hidden descriptors remain read-only;
guest instructions perform segment changes. x87 registers are exposed read-only
when present. SSE registers are not advertised.

Memory access validates the whole span before copying. It allows ordinary RAM
and contiguous A20 aliases, rejects VGA/MMIO, address overflow and spans crossing
an A20 discontinuity, and performs no device reads. A RAM write updates page
generations and the idle-proof write log, so cached code is invalidated. Address
breakpoints are checked before execution and do not insert INT3 bytes into RAM.

For a temporary code experiment, first save the original bytes and restore them
before continuing unless the experiment intentionally executes the patch:

```text
(gdb) set $original = *(unsigned char*)0x1e281c
(gdb) set {unsigned char}0x1e281c = 0x90
(gdb) x/bx 0x1e281c
(gdb) set {unsigned char}0x1e281c = $original
```

This edits loaded RAM only. Translating a loaded address into an EXE offset
requires its object layout, relocations and load bias; a RAM address is not a
file offset. No persistent EXE patching command is provided.

## Inspecting mixed 16/32-bit code

The remote target describes i386 registers. On the tested GNU GDB, `x/i` assumes
32-bit defaults even when the guest CS is 16-bit, and `set disassembler-options`
is unsupported for this architecture. Check `code=16` in `monitor state` before
trusting a disassembly. Dump a small span and explicitly select GNU objdump's
16-bit address and operand defaults:

```text
(gdb) dump binary memory target/guest16.bin 0x147ea 0x147f0
(gdb) shell objdump -D -b binary -m i386 -M intel,addr16,data16 --adjust-vma=0x147ea target/guest16.bin
```

This correctly decodes `F3 A5` at 147EAh as `rep movs WORD PTR es:[di],WORD PTR
ds:[si]`. In another 16-bit CS, `67 F3 66 A5` at 11FB74h instead selects 32-bit
addressing and dword operands. Instruction prefixes must be interpreted relative
to the CS default. RAM dumps contain game data and belong in ignored `target/`
or another local scratch directory.

## Finding frequently executed addresses

```text
(gdb) monitor hotspots on
(gdb) monitor stop-at 350100000
(gdb) continue
(gdb) monitor hotspots
(gdb) monitor hotspots off
```

`hotspots on` clears the fixed table and starts counting instruction-boundary PC
observations. `hotspots` prints the top 20 with CS, code width and a dropped count.
Counts include REP elements and IRQ boundaries. Storage and collision probing
are bounded; these counts identify inspection targets, not proven idle loops.
`stop-at` takes an absolute decimal step greater than the current step and no
greater than the CLI budget. The budget itself stops with SIGXCPU.
`monitor key SCAN_HEX ASCII_HEX` injects a BIOS key while stopped.

There are no hardware watchpoints, reverse execution or guest signal injection.
A fault stops with SIGILL and remains inspectable; editing registers does not
recover the runtime from its faulted state. Halts stop with SIGSTOP. Storage
that requires asynchronous host completion is serviced by the desktop owner;
the synchronous headless adapter does not provide an asynchronous storage host.

See the [Heroes investigation](compatibility/heroes-waits.md#live-gdb-audit)
for recorded observations and the existing static/trace inventory.

## Validation

The ordinary workspace suite covers side-effect-free stops/reads, rejected
memory spans and PC edits, A20 mapping, warmed-code patch invalidation,
write-log participation, register round trips, breakpoints and finite budgets.
It also compares ordinary wrapper execution against the reference interpreter
and checks that wrapper setup/execution allocates no debugger storage. TCP tests
cover an idle listener, partial packets, reattachment and desktop command handling
while GDB controls the guest, plus remote Ctrl-C followed by resume. Desktop
tests check that a GDB stop does not enqueue input release or accumulate paused
wall time for subsequent catch-up.
The opt-in integration test uses an installed GNU GDB and localhost TCP:

```sh
cargo test -p doslet-debugger --locked \
  local_gdb_client_exercises_registers_ram_breakpoints_step_and_detach \
  -- --ignored --nocapture
```

The desktop owner has a second real-client test, without opening a window/audio
device:

```sh
cargo test -p doslet --locked local_gdb_desktop_owner_steps_patches_and_detaches \
  -- --ignored --nocapture
```

The headless test writes registers and RAM, steps a patched instruction, stops at an address
breakpoint and a step limit, reads monitor output, and detaches. Its synthetic
guest checks the transport contract separately from the recorded Heroes run.
