# Cooperative wait regulator: first fixture experiment

The city prototype replaces **8.799% of scheduler execution with hardware-only
waiting at a fixed 133 MHz**, preserving the recorded PCM, final PNG and all 31
displayed-frame hashes sampled after animation updates. All 31 updates finish
before the next countdown callback. This is an offline cooperative experiment,
not an instruction-equivalence proof or a measured desktop CPU saving. The
existing `--adaptive-cpu` option remains unchanged.

The accompanying startup audit shows why MHz reduction needs separate evidence:
66 MHz reproduces a five-second intro segment's PCM and 128 image-copy outputs,
but some copies finish **165.670 ms later** than at 133 MHz.

The follow-up [speed-tier replay experiment](speed-tier-replays.md) tests MAX
execution for hot work, 8/1 MHz for cold candidates, and 133 MHz for grey paths.

## Implementation

`session/wait_budget.rs` contains allocation-free budget arithmetic. It does not
know game addresses, discover loops or change MHz. The caller must qualify each
complete empty iteration and supply service/work horizons.

The `state_graph_trace` adapter reads
`tools/fixtures/heroes-city-wait.json` and performs these checks:

1. Enter the described foreground head with the expected CS, stack and segment
   caches, a positive animation counter, no audio-service flag and an inactive
   IRQ callback guard.
2. Record one candidate path, bounded to 16,384 steps and one guest millisecond.
   Reject interrupted iterations and configured work entries. Freeze the first
   completed candidate as the template; do not automatically admit unknown paths.
3. Match the exact sequence of executed PCs, successors and 15-byte code windows
   on subsequent iterations. Recheck context and guards at the boundary.
4. Require eight consecutive admitted iterations, then grow the per-iteration
   cap by 1 us. Requested idle is at most one third of naturally executed waiting
   above a 10-us reserve, the configured cap, the input-polling limit, and the
   remaining hardware service horizon minus that reserve.
5. Spend that credit once, immediately after the completed backedge. Unspent
   credit does not accumulate; inserted idle never counts as natural waiting.
   Work and IRQ service continue at the configured MHz.
6. Revoke the budget on an IRQ, work, changed path or failed guard. A pending
   animation update at the next countdown callback, or a new DSP ACK deadline
   miss, forces 250 ms of ordinary execution before another probation period.

The fixture's guarded work is released by IRQ callbacks. The next hardware
service horizon therefore bounds waiting without predicting future releases
from a guest software countdown that can itself slow down. The
[timing audit](city-work-deadlines.md) defines the next-countdown boundary as a
cadence criterion, not an externally specified graphics deadline.

`Session::debug_cooperative_idle` advances devices through their ordinary bounded
event slices, including PCM production. It leaves the CPU at its current PC and
stops when an IRQ becomes eligible. Each call allows at most 250 us and 256 device
slices. Time is quantized: a requested 25 us can take 25.023 us at 133 MHz. There
is no wall-clock sleep. The helper refuses paused execution and competing idle
or analyzer policies; the caller must independently admit a wait boundary.

The path description is manually supplied research data, **not an automatically
proved static summary**. Equal control flow does not prove equal data effects or
future behavior. There is no injected input in the recorded experiment. The
adapter has per-step instrumentation and is not connected to ordinary JIT
execution, MCP, the desktop UI or save-state data. Runtime budget code contains
no Heroes addresses.

## City results

Independent owners restore `target/heroes-states/city.pb.gz`, described in the
[static audit](city-state-static-analysis.md), for five guest seconds. JIT, other
idle accelerations and the MHz controller are disabled; PCM drains every guest
millisecond.

| Measurement | Reference, 133 MHz | Regulated, 133 MHz |
| --- | ---: | ---: |
| Complete animation updates | 31 | 31 |
| Resets after next countdown callback | 0 | 0 |
| Median update execution time | 4.517503 ms | 4.517504 ms |
| Worst release-to-start time | 0.099669 ms | 0.103068 ms |
| Minimum margin before next callback | 5.317834 ms | 5.299187 ms |
| PCM frames | 110,250 | 110,250 |
| DSP ACK deadline misses | 0 | 0 |
| Hardware-only idle | 0 | 439.970 ms |

The template contains 3,231 steps. The run admits 41,170 complete paths, rejects
1,288 interrupted paths and performs 32,159 idle calls. The configured cap is
25 us; probation and immediate IRQ revocation limit the saving to 8.799% of
executed scheduler steps. This is not a host CPU benchmark.

`compare_wait_runs.py --require-pass` checks the same MHz and duration, PCM bytes,
final PNG bytes, all 31 intermediate frame hashes, animation and timer counts,
zero cadence-boundary misses, zero PCM timestamp gaps/overlaps, and completed
audio copies in all complete DMA block intervals. It does not establish upstream
buffer-generation freshness beyond the observed PCM window.

A separate 66-MHz run detects one late reset and invokes recovery. Its ordinary
reference already has one late reset; minimum margins are −0.521 ms regulated
and −0.466 ms unregulated. This rate is **not accepted** by the zero-late-reset
criterion. Revoking waits cannot repair an insufficient configured execution rate.

Evidence: `target/city-regulator/reference-133/`, `final-133/comparison.json` and
`66/`. These are ignored local artifacts, not redistributable game fixtures.

## Startup and intro

`startup_timing` loads the local executable and ISO from the beginning, with a
RAM write overlay and the interpreter. It disables accelerations, sends no input,
drains audio continuously, samples metrics every 100 ms and saves one screenshot
per guest second. Full runs last 45 guest seconds. Menu detection samples its
known head and callback every 256 scheduler steps: the first timestamp is an
observed upper bound, not an exact breakpoint. Screenshots confirm all three menus.

| MHz | First audio in 100-ms samples | First sampled menu |
| --- | ---: | ---: |
| 33 | 4.5 s | 34.028 s |
| 66 | 2.8 s | 30.777 s |
| 133 | 2.5 s | 30.021 s |

At three seconds, 33 MHz still shows DOS/4GW initialization while 133 MHz shows
the animated New World Computing logo. Startup duration mixes initialization
and animation, so it cannot establish an animation's minimum throughput. All
full runs record one PCM timestamp discontinuity; uninterrupted-playback
assumptions do not cover startup stream changes. This audit does not assign the
discontinuity to an underrun.

A portable snapshot at guest time 3,000,003,578 ns in the 133-MHz run isolates
the animation. Independent owners resume it for five guest seconds:

| MHz | Image-copy returns at `0x212777` | PCM equals 133 MHz | Final PNG equals 133 MHz |
| --- | ---: | --- | --- |
| 33 | 106 | No | No |
| 66 | 128 | Yes | Yes |
| 133 | 128 | Reference | Reference |

Probes hash scanout after each completed image copy, including partial updates.
All 128 hashes at 66 MHz match the corresponding 133-MHz sequence, but their
completion times lag by **17.757–165.670 ms**. This is measured phase drift, not
a proved violation of an external frame deadline. Equal outputs and counts do
not establish unchanged cadence.

Additional execution at 133 MHz concentrates in repeated servicing/polling:
the call at `0x20444B` executes 637,736 times versus 171,217 at 66 MHz, while both
complete 128 image copies. These are candidates for a separate guarded intro
summary; they are not admitted by the city template.

Evidence: `target/startup-timing/{33,66,133}/`, `intro-state/capture.pb.gz`,
`target/intro-cpu-sweep/summary.json` and
`target/intro-timing/66/frame-comparison.json`. No live audio-device acceptance
or production interpreter/JIT speedup is claimed.

## Reproduce

```sh
RUSTFLAGS='-C target-cpu=native' cargo build -p doslet-headless \
  --example state_graph_trace --example startup_timing \
  --features profiling --release --locked --offline
target/release/examples/state_graph_trace target/heroes-states/city.pb.gz \
  target/city-regulator/reference-133 2500000000 133 5000 \
  tools/fixtures/heroes-city-deadlines.json
target/release/examples/state_graph_trace target/heroes-states/city.pb.gz \
  target/city-regulator/final-133 2500000000 133 5000 \
  tools/fixtures/heroes-city-deadlines.json tools/fixtures/heroes-city-wait.json
python3 tools/compare_wait_runs.py target/city-regulator/reference-133 \
  target/city-regulator/final-133 --require-pass
```

Full startup, isolated-state capture and animation comparison:

```sh
for mhz in 33 66 133; do
  target/release/examples/startup_timing games/HoMnM/HEROES.EXE \
    games/HoMnM/homm1.gog "target/startup-timing/$mhz" "$mhz" 45 \
    tools/fixtures/heroes-startup-markers.json
done
target/release/examples/startup_timing games/HoMnM/HEROES.EXE \
  games/HoMnM/homm1.gog target/startup-timing/intro-state 133 3 \
  tools/fixtures/heroes-startup-markers.json - 3000
for mhz in 66 133; do
  target/release/examples/state_graph_trace \
    target/startup-timing/intro-state/capture.pb.gz \
    "target/intro-timing/$mhz" 2500000000 "$mhz" 5000 \
    tools/fixtures/heroes-intro-probes.json
done
python3 tools/compare_frame_probes.py target/intro-timing/133 target/intro-timing/66
```

The optional startup Esc argument is guest milliseconds; `-` leaves the intro
uninterrupted. The final optional argument captures one portable state at or
just after that guest millisecond. State/image artifacts contain game data and
must remain under ignored `target/`.

## Validation and remaining scope

Validation passes: 105 runtime library tests, the PC hardware-idle differential
test, three trace-example tests, targeted Clippy with warnings denied, formatting
and diff checks, and the city fixture acceptance. Tests cover credit bounds,
unknown horizons, probation/recovery, pending IRQs, CPU preservation, changed
code, failed guards and interrupted/work-containing paths.

Production integration still needs automatically established path/effect summaries,
input and scene-transition invalidation, generic work-release/completion evidence
and interpreter/JIT overhead measurements. The intro needs its own summary and
timeliness criterion. These gaps are not hidden behind the desktop adaptation
switch or inferred from the city result.
