# Prepared interpreter handlers on Linux

Measured on Linux x86-64 with Rust 1.99.0 and ordinary Cargo release builds. These measurements replace
neither the earlier macOS baseline nor browser measurements.

## Fixed-step game measurements

Baseline CPU code is commit `7af1d50`; prepared CPU code is `da808cd`.
Both binaries use the same updated
`bench_interpreter_game` probe, including A20-aware RAM hashing and CIV keyboard
input. JIT is disabled, the guest clock is 50 MHz, and existing VGA/idle
acceleration stays enabled. Guest file writes use the RAM overlay. MIDI and PCM
are drained after each bounded 10,000-step advance; there is no window, host
audio device or synthesis in the probe.

Each game/version was warmed once, then measured in three pairs, ordered
A/B, B/A, A/B, without concurrent builds or emulator runs.
CPU time is `/usr/bin/time -p` user plus system time for the whole process,
including mounting and final hashing. Internal wall time covers the execution
loop only. Reported ns/step divide by logical guest steps, which can include
existing accelerated waits; they are not host instruction latency.

| Game | Steps | CPU ns/step before | After | CPU reduction | Loop wall ns/step before | After |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Heroes | 390,000,000 | 16.44 | 13.00 | 20.9% | 15.861 | 12.415 |
| Prince | 300,000,000 | 9.30 | 7.20 | 22.6% | 9.267 | 7.120 |
| CIV | 100,000,000 | 9.40 | 8.30 | 11.7% | 9.472 | 8.283 |

Raw paired samples include warmup rows (`round=-1`).
Heroes uses the local ISO and a fixed 390-million-step endpoint; this probe does
not detect the first menu frame, so it does not establish a new exact
startup-to-menu timing. CIV uses keys `151` (VGA, Roland, keyboard), reaches
mode 13h and drains 1,488 MIDI bytes. These are recorded startup workloads,
not interactive gameplay or browser throughput claims.

Build the probe for each revision, preserve each binary outside Cargo's output
path, and run the same command in both versions:

```sh
cargo build -p doslet-headless --release --locked --example bench_interpreter_game
/usr/bin/time -p target/release/examples/bench_interpreter_game \
  games/HoMnM/HEROES.EXE 390000000 games/HoMnM/homm1.gog cached
/usr/bin/time -p target/release/examples/bench_interpreter_game \
  games/PRINCE/PRINCE.EXE 300000000 - cached
/usr/bin/time -p target/release/examples/bench_interpreter_game \
  games/civ/CIV.EXE 100000000 - cached 151
```

The probe at `da808cd` must also be copied to the baseline checkout, because
`7af1d50` predates the probe's A20 fix and keyboard support. Use `reference`
instead of `cached` to disable decoding and batching; the Heroes and CIV final
state lines also match that byte-fetching path in separate validation runs.

## State equivalence

All eight baseline/prepared runs per game (warmup plus three measured pairs)
have identical general/segment registers, IP, EFLAGS, step count, guest time and
the following FNV-1a hashes. Low RAM is `0..A0000h`; high RAM is `C0000h` to the
configured RAM end with A20 enabled, or to `100000h` otherwise. The VGA aperture
is excluded from RAM views and all physical VRAM is hashed separately.

| Game | Low RAM | High RAM | VRAM | Indexed frame | Palette | MIDI |
| --- | --- | --- | --- | --- | --- | --- |
| Heroes | `d3f4101a759ba9b7` | `3a32513383135107` | `a70be2f5518431d4` | `68f424c8430b31d4` | `222ec55411dc4f10` | `cbf29ce484222325` |
| Prince | `2065cff7cd1cc942` | `a4858fab56eca176` | `fff1d61426d74db8` | `cf87496227914db8` | `61f6c8748b10d27a` | `56c4f569f9c0afba` |
| CIV | `b5f5e336b5344623` | `f3285b6c831d2e6a` | `ba98a050a6feddbb` | `af51d25faf5f55bb` | `7d34f63fe40f1401` | `4dde2d97ec3c19a1` |

The empty MIDI hash for Heroes reflects this local installation's output;
the receiver is still drained. PRINCE and CIV produce nonempty MIDI streams.
Final architectural state, preserved here independently of host timing:

```text
Heroes: state steps=390000000 ns=31199879200 regs=[2e3c, 0, a8, f0, 6740, 674a, 6896, 681e] segments=[88, a8, 8, a8, 0, 20] ip=a1c2 flags=2
Prince: state steps=300000000 ns=23999977440 regs=[232100, 0, 3da, 7f, 2320f0, 232414, 23210c, 23240c] segments=[160, 168, 168, 168, 0, 1c8] ip=1de7ba flags=246
CIV: state steps=100000000 ns=7999999040 regs=[1a4, 2, 0, 1a1, f03e, f092, f0, f0bd] segments=[1010, 3a90, a000, 3a90, 0, 0] ip=a4e flags=206
```

The ignored PRINCE differential test also compares 136,359 varied bounded
batches against byte fetching through 300 million steps: registers, control
state, flags, guest time, RAM/VRAM, scanout/palette, emitted MIDI and PCM agree.
It passes with and without profiling. Its old profiling assertion compared
implementation counters (dispatched VGA reads and combined wait iterations)
which already differed on the baseline at step 77,389,073. It now compares
logical CPU/idle steps and IRQ entries, while retaining all guest-state and
output comparisons. Cache coverage includes both single hits and batch steps.

## Linux host profile and assembly

Separate 500-million-step Heroes profiles use GNU gprofng 2.42 hardware CPU
cycle samples, not the paired timing runs:

```sh
gprofng collect app -p off -S off -h cycles,hi -o target/heroes-cycles.er \
  target/release/examples/bench_interpreter_game \
  games/HoMnM/HEROES.EXE 500000000 games/HoMnM/homm1.gog cached
gprofng display text -header -functions target/heroes-cycles.er
objdump -t target/release/examples/bench_interpreter_game
# Use the exact interpret_batch symbol from the symbol table:
objdump -d --disassemble=SYMBOL target/release/examples/bench_interpreter_game
```

Clock-based attempts reported changed interval timers and were discarded.
Disabling clock and periodic sampling removed that warning; the hardware-cycle
profiles report no collection errors. CPU frequency varies, so use relative
exclusive cycle shares rather than gprofng's converted seconds.

| Function | Baseline exclusive cycles | Prepared |
| --- | ---: | ---: |
| `try_batched` | 18.61% | 3.06% |
| `interpret_batch` | 12.19% | 14.10% |
| `batch_block` | 8.47% | 8.37% |
| `steps_to_event_nanos` | 6.02% | 8.12% |
| `Cpu::step` | 5.99% | 6.06% |
| `PcBus::read_u32` | 2.38% | 3.04% |
| `Registers::address` | 3.06% | 2.88% |
| `Ram::view_mut` | 2.08% | 2.84% |

Prepared operations now occupy their own handler symbols, with arithmetic
helpers inlined; a lower `try_batched` share alone is not an end-to-end gain.
The paired CPU measurements above establish that gain for these workloads.
No branch-miss event measurement was taken, so the profile does not attribute
the complete improvement to a particular mispredicted jump.

`objdump` confirms that ordinary dispatch loads one 16-byte prepared record and
calls through the handler table. Its cursor advances by `10h`, replacing the
baseline's `24h` stride and two vector loads plus the remaining four bytes.
The cold fallback still loads 36-byte decoded IR. Register MOV handlers have
no runtime operand-form/width/ALU-operation match. Register-index decoding,
segment/RAM guards, code checks and inter-batch scheduling remain execution
costs. No additional RAM fast path bypassing bus tracking was introduced.

## Implemented forms and validation

- MOV: register/register, register/immediate, register/memory, memory/register,
  memory/immediate.
- ADD/OR/ADC/SBB/AND/SUB/XOR/CMP/TEST: the existing encodable register,
  immediate and memory forms.
- Register and memory INC/DEC, LEA, register PUSH/POP, relative near CALL/JMP/Jcc,
  RET/RET imm16, immediate/CL shifts and rotates, decoded MOVZX/MOVSX.
- Widths 1/2/4 and effective-address sizes 16/32 are const parameters where
  supported by the decoder. Native candidates retain the old predicate and
  native decoders reject word/non-32-bit-branch forms. Other forms use
  `try_batched` and the existing cached executor.

Validation includes format, workspace Clippy with warnings denied, workspace
tests (including allocation regression), doslet-pc profiling tests and the
WASM worker check. New differential tests compare prepared batches with both
cached single steps and byte fetching, including all shift counts 0..33,
carry/overflow, high byte aliases, SIB/16-bit addressing, segment limits,
VGA/unmapped operands, zero-effect guard refusal, 16-bit address/branch/stack
wrap and a store rewriting the next instruction in the same block. The local
Heroes background interpreter/JIT pacing test retains its identical final RAM,
frame, palette, registers, flags and guest-time assertion.

The hot block table shrinks from 1,312 to 672 KiB on x86-64; preserving fixed
cold decoded storage increases total block storage by 512 KiB. This is an
explicit locality/space tradeoff. WASM compilation is verified, but browser
performance and interactive gameplay remain unmeasured by this work.
