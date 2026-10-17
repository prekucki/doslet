# City snapshot: CPU-budget sweep and differential path counts

Increasing nominal CPU throughput from 133 to 1000 MHz mostly increases repeated
dispatch and polling in this Vulcania snapshot. In five guest seconds, the city
loop repeats **47,068 versus 373,696 times**, while the inspected drawing routine
completes **403 times in both runs**. The complete emitted PCM sample streams and
final rendered pixels match exactly at 66, 133 and 1000 MHz.

This makes throughput variation useful for **discovering candidate wait paths**.
It does not prove that every growing path can be skipped. At low throughput,
audio and animation can diverge even when the output queue and IRQ deadline
counters look healthy.

The follow-up [work-timing audit](city-work-deadlines.md) measures release,
completion, timer reentry and audio service latency to explain these differences.

## Controlled experiment

Every run independently restores the same
[portable city state](city-state-static-analysis.md#state-and-method). Only the
configured MHz changes. There is no injected input, warmup, adaptive controller,
JIT, VGA acceleration or learned waiting. These are nominal Doslet execution
rates, not cycle-accurate measurements of physical x86 hardware.

Two bounded intervals were recorded:

- **One guest second:** 1, 4, 8, 16, 33, 66, 133, 266, 533 and 1000 MHz.
- **Five guest seconds:** 1, 4, 8, 16, 33, 66, 133 and 1000 MHz.

Every interval completed. IRQ-entry steps remain separate from instruction
successors. Counts include REP microsteps. Raw node and successor counts are
retained, so differences are not limited to the addresses selected for the
tables below.

The simulation runs offline. PCM is drained every **1 ms of guest time**, not
host time, and the snapshot's preexisting output is drained before measurement.
Thus an instrumented run taking longer than real time does not fabricate a host
audio underrun. The 1000-MHz five-second run took **112.905 host seconds**, or
22.58 host seconds per guest second, excluding state loading and artifact output.
That is audit overhead, not a production JIT benchmark or a claim of real-time
playability. No physical audio device or host callback queue was exercised.

## Which paths grow?

The one-second sweep separates recurring foreground dispatch from work whose
frequency remains tied to device time. Each value is a count over exactly one
guest second.

| MHz | City loop `219D1A` | Object dispatch `216D77` | Fast-return edge `215FD3 → 215FE6` | VGA reads `22427A` | Drawing completions `21DF2D` |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 37 | 30 | 1,538 | 0 |
| 4 | 25 | 1,262 | 1,055 | 6,968 | 13 |
| 8 | 213 | 10,474 | 8,763 | 22,907 | 37 |
| 16 | 750 | 36,770 | 30,768 | 37,100 | 52 |
| 33 | 1,945 | 95,329 | 79,764 | 116,962 | 65 |
| 66 | 4,492 | 220,143 | 184,199 | 71,747 | 78 |
| 133 | 9,599 | 470,339 | 393,548 | 156,132 | 78 |
| 266 | 19,733 | 966,916 | 809,053 | 323,430 | 78 |
| 533 | 40,080 | 1,963,908 | 1,643,269 | 659,571 | 78 |
| 1000 | 75,668 | 3,707,756 | 3,102,407 | 1,247,576 | 78 |

The callback targets and conditional fast-return path are described in the
[static graph audit](city-state-static-analysis.md). These are scene/version
observations, not production address recipes.

At 133 and 1000 MHz, the one-second traces visit the same 4,196 `(PC, bitness)`
nodes: higher throughput does not discover an additional executed path in this
interval. It repeats existing paths more often. Across 66, 133, 266, 533 and
1000 MHz, the timer counter callback executes 64 times, palette callback six
times, and each inspected audio processing loop body 22,528 times.

VGA polling is not monotonic across the entire range: 33 MHz records more reads
than 66 MHz. Arrival phase relative to retrace changes the amount of polling.
A detector should therefore compare several budgets and complete episodes,
rather than classify from a single ratio between two short traces.

### Five-second confirmation

| MHz | City-loop iterations | Drawing completions | Timer counter callbacks | PCM equal to 133 MHz? | Final RGBA equal to 133 MHz? |
| ---: | ---: | ---: | ---: | :---: | :---: |
| 1 | 2 | 0 | 35 | No | No |
| 4 | 133 | 65 | 127 | No | No |
| 8 | 1,052 | 182 | 226 | Yes | No |
| 16 | 3,594 | 288 | 281 | Yes | No |
| 33 | 9,780 | 364 | 313 | Yes | No |
| 66 | 21,998 | 403 | 313 | Yes | Yes |
| 133 | 47,068 | 403 | 313 | Yes | Yes |
| 1000 | 373,696 | 403 | 315 | Yes | Yes |

For 133 → 1000 MHz over five seconds:

| Path | 133 MHz | 1000 MHz |
| --- | ---: | ---: |
| Object-handler calls at `216D77` | 2,306,320 | 18,311,116 |
| Conditional fast return at `215FD3 → 215FE6` | 1,929,777 | 15,321,548 |
| VGA status reads at `22427A` | 1,761,067 | 11,225,198 |
| Drawing completions at `21DF2D` | 403 | 403 |
| Audio processing body at `23D356` | 110,592 | 110,592 |
| Audio copy/conversion body at `23BE81` | 110,592 | 110,592 |
| Palette callback at `217540` | 30 | 30 |

The extra CPU capacity is predominantly absorbed by repeated checking and
dispatch. Exact output equality is stronger evidence than matching counts alone,
but final RGBA equality does not prove that every intermediate frame or input
latency matched. The two extra counter callbacks at 1000 MHz also show that the
entire execution is not identical.

66 MHz is the lowest **tested** rate with these matching five-second graphics
results. This is not a discovered safe minimum for Heroes: there is no input,
battle, scrolling, scene-transition or long-duration coverage here.

## Why queue health alone is insufficient

Every five-second run produces and drains **110,250 stereo PCM frames at
22,050 Hz**, with:

- no timestamp gaps or overlaps;
- a maximum pending emulated output queue of 23 frames;
- no stalled `advance` calls;
- zero missed acknowledgements in 108 SB DMA block IRQ service windows.

The production emulated PCM queue has capacity 4,096 frames; this test's
guest-time draining keeps it far from full. The counts above cover that queue
and IRQ acknowledgement, not the guest application's success in preparing fresh
audio data.

At 1 MHz, **107,986 of 110,250 emitted frames differ** from the 133-MHz reference.
Among 107 complete 1,024-frame blocks, 48 are all-zero, 45 consecutive block
comparisons match, and 27 nonzero blocks match the block two positions earlier.
The reference has no all-zero or repeated blocks at those offsets. At 4 MHz,
95,022 frames differ, with two all-zero complete blocks. The inspected mixing
body executes 13,339 times at 1 MHz and 95,264 at 4 MHz, versus 110,592 at
8 MHz and above.

This is consistent with insufficient guest-side audio preparation and repeated
or silent buffer contents despite continuing DMA and acknowledgements. It does
not establish the exact buffer-refill failure point. No listening test was used
to infer sound quality; the evidence is sample content, timestamps and execution
counts. Captures are the emulated PCM stream, not an external MIDI synthesizer
or the final host audio mix.

At 8 MHz and above, the **entire five-second PCM sample stream is byte-identical**
to 133 MHz, SHA-256:
`ca2bb2309f32662f` (prefix; complete digest in `summary.json`). However, graphics
at 8, 16 and 33 MHz still differ. Preserving audio alone is insufficient.

## A discovery strategy supported by this result

Use differential profiling at fixed device time to propose regions for static
analysis:

1. Restore an identical state at several CPU budgets, keeping device clocks and
   input schedules tied to guest time. Discard ranges where required work loses
   cadence or output changes unexpectedly.
2. Rank **edges and guarded paths** whose execution counts grow with available
   CPU while useful-work counters and outputs remain stable. This discovers
   expensive caller/dispatch paths as well as narrow device-poll loops.
3. Inspect those candidates in the speculative graph: identify their inputs,
   effects, callback targets and exits to work. Increased computation, repeated
   rendering or input-dependent processing must not become unconditional wait
   regions merely because counts scale with MHz.
4. Cache validated path summaries and count actual eligible runtime time. Keep
   code/context/table invalidation, unrecognized exits, IRQ work and deadline
   pressure as reasons to restore full throughput.

A nonzero average wait fraction is not enough to establish spare capacity.
At 33 MHz, the fast-return path still accounts for **14.58% of five-second
execution steps**, yet drawing completions fall from 403 to 364 and the final
image differs. A reserve and recovery decision must consider complete episodes,
service latency and progress, not only a global percentage of empty work.

This experiment adds candidate evidence and reusable offline tools. It does not
change the adaptive policy or install any Heroes-specific runtime marker.

## Reproduction and artifacts

```sh
RUSTFLAGS='-C target-cpu=native' cargo build -p doslet-headless \
  --example state_graph_trace --features profiling --release --locked --offline

for mhz in 1 4 8 16 33 66 133 266 533 1000; do
  target/release/examples/state_graph_trace \
    target/heroes-states/city.pb.gz "target/city-cpu-sweep/$mhz" \
    2500000000 "$mhz" 1000
done
python3 tools/compare_state_traces.py target/city-cpu-sweep

for mhz in 1 4 8 16 33 66 133 1000; do
  target/release/examples/state_graph_trace \
    target/heroes-states/city.pb.gz "target/city-cpu-confirm/$mhz" \
    2500000000 "$mhz" 5000
done
python3 tools/compare_state_traces.py target/city-cpu-confirm
```

Each run retains `trace.json`, `wait-trace.json`, `metrics.json`, initial RAM
dumps and `audio.s16le` (stereo signed 16-bit little-endian samples). The final
tool also writes `final.png`; the first one-second sweep predates that addition.
The five-second recordings have matching `audio.wav` files created from the raw
samples at 22,050 Hz. Game data, screenshots and recordings remain under ignored
`target/`.

The comparison tool writes `nodes.csv` and `edges.csv` with both raw counts and
counts per guest second, plus `summary.json` with audio hashes, buffer metrics
and repeated-block counts. It rejects incomplete durations and mismatched
trace/PCM accounting. The tracer caps execution at 2.5 billion steps, ten guest
seconds and ten minutes of instrumented host execution per run.

Validation: the release example built, Clippy passed with `-D warnings`, and the
three existing speculative graph tests passed. All 18 runs completed and their
trace and PCM accounting passed the comparison checks. This is one deterministic
trajectory per interval/rate, not repeated production performance benchmarking.
