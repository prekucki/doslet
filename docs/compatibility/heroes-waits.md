# HEROES.EXE wait and polling inventory

The local Heroes of Might and Magic main menu keeps executing while music
plays. This investigation identifies polling sites and separates them from
audio processing, event dispatch and ordinary backward transfers. The inventory
also documents an optional native experiment for three counter-wait sites below.

## Recorded scope

Two ISO-backed runs recorded 50 million individual execution steps: startup
steps 0..45M, and main-menu steps 350M..355M after Esc at 100M. Both used the
RAM file overlay; the original installation, ISO and configuration were unchanged.
The trace recorded no HLT execution and dropped no entries. The main-menu
sample includes 3,433 INT 33h/000Bh relative-motion queries.

The executable SHA-256 is
`0e87d3025f437158dfb9099ca2921c83bac5bf9e4708bad04e4d5fc8297218a2`;
SB16.DIG is
`c8a1e7a0e14c8b696e665520e338711e4c20cc3bb4733f83527add41dd6f6b38`.
Addresses below are linear guest addresses for this loaded version. The nested
LE header is at file offset 37CF4h. Its executable object, with load bias
18E000h, occupies 19E000h..23F527h; the data object is excluded from scanning.
Object flags and layout follow the
[Open Watcom LE definitions](https://github.com/open-watcom/open-watcom-v2/blob/master/bld/watcom/h/exeflat.h).
The loaded SB driver uses 16-bit code at base 17B50h.

## Wait sites

| Addresses (hex) | Condition and behavior | Recorded evidence |
| --- | --- | --- |
| 224274..224277; 22427A..22427D | VGA retrace end/start, port 3DAh, bit 3 through TEST AL,AH | Second loop repeated 1,855,270 times across both traces. This form already has validated batching. |
| 22E077..22E07A; 22E07C..22E07F | VGA retrace using TEST AL,08h; port derived from BIOS CRTC base + 6 | Not executed in these traces; not batched. |
| 22F783..22F789; 22F78B..22F791 | VGA retrace using TEST EAX,8; outer LOOP at 22F793 counts frames | Not executed in these traces; not batched. |
| 17E67..17E6A; 17E6C..17E6F | SB driver waits for VGA transitions; outer LOOP at 17E71 counts frames | Startup repetitions 3,781 and 173 respectively; not batched. |
| 18131..1813C | DSP write readiness: port 22Ch, busy bit 7, bounded timeout | 15 ready reads; no polling repetitions. |
| 18160..18165 | DSP read readiness: port 22Eh, bit 7, bounded timeout | Both reads already ready. |
| 1817D..1817E | Fixed reset-pulse delay through 20 port 226h reads | 19 LOOP repetitions; this is a counted delay. |
| 18186..18198 | Reset response: wait on 22Eh, read 22Ah, compare AAh, bounded timeout | First response succeeded. |
| 1E281C..1E2823; 1E2881..1E2888; 1E331A..1E3321 | Tight loops until dword at 281098h becomes zero | Three new pure memory-wait candidates; absent from both recorded paths. |
| 205C24..205C31 | Wait until pointed-to timer reaches zero while calling event pump 206FC0h | Cooperative wait; cannot omit the event pump. |
| 22B181..22B18B | INT 21h/AH=2Ch until DOS seconds change | 10,898 startup repetitions. |
| 22B191..22B198 | Same clock query, counting iterations to calibrate delay | 12,977 startup repetitions; preserving the measured count matters. |
| 22B1CF..22B1D6 | Calibrated counted delay with clock queries | Branch follows decrement flags, rather than comparing returned time. |

The three memory waits read the second slot in a six-dword timer array starting
at 281094h. Routine 219F90h decrements positive slots, with a store at 219FABh.
Several callers write 281098h, including 1CBCADh, 1CBE41h, 1CC2A4h,
1E27BFh, 1E328Dh and 205BF1h. The cooperative wait also uses that timer.
The callback registration and runtime delivery are examined in the
[counter-wait audit](#counter-wait-audit).
These cold sites do not explain the sampled main-menu CPU load.

## Counter-wait audit

All three tight waits execute the same two-instruction operation: compare the
32-bit value at 281098h with zero, then branch back if unequal. They do not call
the event pump, modify registers, or write memory. The guard at 281168h can bypass
them. Their surrounding routines update object direction/frame/position fields;
these are paced object operations rather than the main-menu event loop.

The producer is now traced through the game's timer machinery:

1. Initialization at 21A22Ch registers 219F90h; 21A239h requests frequency 40h,
   or 64 Hz. An alternate initialization path at 21A059h does the same.
2. Frequency conversion at 22F938h computes 1,000,000 / Hz. The observed timer
   slot 2 is active (state 2), points to 219F90h and stores period 15,625 us.
3. Code at 22F299h installs the IRQ0/vector-8 handler at 22EFFEh. It programs
   PIT channel 0 through ports 43h/40h and accumulates per-slot elapsed time.
4. The handler dispatches due callbacks at 22F0ACh. In an actual main-menu run,
   219F90h's stack return address is 22F0B2h, confirming that dispatch path.
5. 219F90h decrements each strictly positive timer slot and increments a tick
   serial at 2831D0h. For the watched slot, the store at 219FABh runs with EAX=4.

An opt-in diagnostic observes each instruction without changing guest code or
data. In main-menu steps 350M..360M (1.212121212 seconds of guest time), it records
76 callback entries, zero visits to all three wait branches, zero cooperative-wait
visits, and no changes to 281098h: it stays zero. Actual callback completion
intervals range from 11.944485 to 28.458666 ms, averaging 16.107277 ms over 75
intervals. The configured 15.625-ms period is therefore nominal; interrupt
latency, callback dispatch and the PIT/software timer phase must be retained.
Observer runtime is not a performance measurement.

An additional scripted New Game/Standard Game attempt sampled steps 350M..440M
and 440M..550M. Both windows also recorded no counter writes or visits to these
waits. The final captured frame showed the animated background without a menu;
this attempt does not establish that gameplay was reached or that the waits are
inactive during gameplay. The main-menu audit was repeated after adding guards
against counting a pre-interrupt PC as an executed instruction, with the same
76 callbacks and interval statistics.

A value of 2 requests two future decrements; it is not invariably a 31.25-ms
delay because the first tick may be imminent and delivery may be delayed.
Negative values are a special case: the callback skips them, while the tight
loops test inequality rather than positivity. Such a value would keep those
loops running unless another path rewrote it. The cooperative variant at
205C24h instead exits on nonpositive values and calls 206FC0h between checks.

These tight loops are suitable candidates for **whole-iteration batching**, not
for setting the counter to zero or jumping directly to the exit. A safe future
implementation needs to validate the actual compare/branch bytes, data mapping,
unchanged memory and flags; preserve eligible interrupts and the phase between
compare and branch; stop strictly before PIT, DSP/DMA or other memory-changing
events; and disable batching for debugging or unsupported state. The ordinary
guest IRQ handler must still perform the decrement, and audio must advance on
the same timeline. Replacing the loop with HLT would change architectural state
and is not an equivalent general solution.

### Experimental native patch

An explicit native flag now enables whole-iteration batching at just these
three instruction offsets:

```sh
target/release/doslet --config games/HoMnM.toml --experimental-heroes-timer-waits
```

It is off by default and is retained on Reset. No executable or mounted file
is modified. The PC layer accepts caller-selected sites and operand addresses;
the native launch adapter supplies the version-specific Heroes values. Every
batch checks the exact nine-byte CMP/JNE sequence, 32-bit code/data access,
ordinary RAM, a positive counter and invariant arithmetic flags. Pending IRQs,
interrupt shadows, disabled interrupts, paging and debug state force ordinary
execution. Batches stop before the next PIT/DSP event and are bounded to 1 ms.
The counter still changes only when normal guest code runs.

The terminal first prints that the experiment is enabled. A separate one-time
`experimental timer waits active; batched ... logical steps` message appears
on the first batch in each session. Without that second message, these sites
have not accelerated execution. The sampled main menu does not use these loops,
so this patch does not promise lower CPU there.

Headless comparison uses `DOSLET_PROBE_HEROES_TIMER_WAITS=1` and prints
`MEMORY WAIT batched_steps=...` at the end. Do not combine a performance test
with instruction-by-instruction auditing: a one-step budget cannot batch.
Differential synthetic tests cover countdown exit, partial budgets, IRQ0,
SB IRQ/DMA/PCM, complete conventional RAM, VRAM, registers, flags and guest time.
Fallback tests include changed code, zero/negative values, incompatible flags,
IF/TF, pending IRQ, paging and the STI shadow. These tests establish selected
batching semantics, not lower CPU in an unrecorded gameplay path.

A paired ISO-backed run to 360M steps, Esc at 100M, with and without the patch
matched SHA-256 for low RAM, 15-MiB extended RAM, physical VRAM, RGB frame and
MIDI output (empty in this SB configuration). Final CPU registers/flags and PCM
frame/nonzero counts also matched. Both reported zero batched memory-wait steps,
as expected for this main-menu path; no native CPU reduction is claimed there.

Reproduce the main-menu audit with the exact local executable version above:

```sh
DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog \
DOSLET_PROBE_HEROES_TIMER_AFTER=350000000 \
cargo run -p doslet-headless --release --features profiling --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 360000000 '100000000:01:1B'
```

`DOSLET_PROBE_HEROES_TIMER_AFTER` is version-specific and opt-in. It reports
callback callers, counter writes, tight/cooperative wait visits and measured
callback completion intervals, then prints the live timer-slot configuration.
It uses instruction-sized execution once the requested step is reached and
does not alter original files, registers or memory outside normal execution.

The outer DSP retry path at 18127..18155 performs recovery/acknowledgment before
retrying. Sound-card probing at 22DABC..22DBC3 performs port writes and indirect
calls. Neither can be treated as a side-effect-free polling loop.

## Repetition that performs useful work

| Addresses (hex) | Work performed | Main-menu sample |
| --- | --- | --- |
| 23BE81..23BE9D | Convert and pack audio samples while advancing input/output pointers | 13,299 repetitions |
| 23D356..23D38C | Scale stereo samples by volume and accumulate into a mixing buffer | 13,299 repetitions |
| 216D70..216D8C | Walk an event-handler list and invoke handlers | 24,038 repetitions |
| 22F04E..22F07F | Update timer counters and phases across slots | 2,145 repetitions |
| 22F094..22F0BC | Dispatch timer callbacks, including an inner counted loop at 22F0B4 | 2,002 outer repetitions |

About 41.7% of the sampled main-menu execution steps occur in 16-bit code.
That is an instruction-frequency observation, not a host CPU-time attribution.
Backward transfers to shared return helpers are also common. A lower jump target
alone does not establish a loop: the scanner follows direct control flow and
stops at returns. Static HLT-looking bytes in tables are not evidence of idle.

## Reproduce and inspect other paths

The opt-in host tracer records actual backward branches, successful port reads,
BIOS/DOS services, HLT and code-segment metadata. It observes the machine and
uses host allocations outside emulation. Once enabled, one-step execution makes
poll iterations visible; its runtime is unsuitable for a performance benchmark.

Capture startup and then the main-menu window, including RAM snapshots:

```sh
DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog \
DOSLET_PROBE_WAIT_AFTER=0 \
DOSLET_PROBE_WAIT_OUTPUT=/tmp/doslet-heroes-startup-waits.json \
cargo run -p doslet-headless --release --features profiling --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 45000000

DOSLET_PROBE_CDROM=games/HoMnM/homm1.gog DOSLET_PROBE_RAM=1 \
DOSLET_PROBE_WAIT_AFTER=350000000 \
DOSLET_PROBE_WAIT_OUTPUT=/tmp/doslet-heroes-menu-waits.json \
cargo run -p doslet-headless --release --features profiling --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 355000000 '100000000:01:1B'
```

Scan loaded executable objects and sampled code-segment spans with Python
stdlib and GNU objdump; no Node tooling is involved:

```sh
python3 tools/wait_loops.py /tmp/doslet-probe-extended.bin \
  --le games/HoMnM/HEROES.EXE --bias 0x18e000 --observed-segments \
  --trace /tmp/doslet-heroes-startup-waits.json \
  --trace /tmp/doslet-heroes-menu-waits.json \
  --output /tmp/doslet-heroes-waits-all-high.json

python3 tools/wait_loops.py /tmp/doslet-probe-low.bin --ram-base 0 \
  --region SB16.DIG:0x17cba:0x18664:16 --observed-segments \
  --trace /tmp/doslet-heroes-startup-waits.json \
  --trace /tmp/doslet-heroes-menu-waits.json \
  --output /tmp/doslet-heroes-waits-all-low.json
```

The JSON inventories contain all decoded backward-branch/HLT candidates in the
declared regions, including ordinary work and suspected inline data. They carry
classifications, addresses, control-flow uncertainty and observed branch/port
counts. Matching uses source, target and code mode. They are diagnostic artifacts,
not a list of loops that can safely be skipped. Linear decoding, indirect flow,
overlays and unrecorded gameplay paths prevent a claim that every possible wait
has been identified. Game bytes and RAM dumps remain local, outside the repository.

Validation: the Rust trace regression compares machine registers, RAM and guest
time against an untraced run. Four Python tests cover executable-region selection,
polling versus backward transfers, flag waits versus pointer traversal, and trace
merging. Workspace Clippy with profiling passes. No additional acceleration or
native UI CPU reduction is claimed by this inventory.
