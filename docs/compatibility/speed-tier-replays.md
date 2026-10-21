# Hot, cold and grey CPU tiers: snapshot replay experiment

With **hot/MAX, cold/8 MHz and grey/133 MHz**, all three city replays retain 31
animation updates, identical PCM and image sequences, and zero late resets.
At cold/1 MHz, grey improves the result from 28 to 29 updates, but 17–21 resets
per run remain late. Grey protects rare or uncertain work; it cannot by itself
bound the time needed to leave a slow polling/dispatch path.

These results describe the original offline experiment on city and intro
snapshots, including its former MAX override for IRQ scopes. The native adapter
below uses saved labels and separately reports confirmed IRQ code at the GREY rate. The
historical replay results must not be attributed to the revised policy.

## Try weighted execution in the game

Build without instruction profiling and start with the locally generated combined
profile. `--hot-mhz` is optional: omitting it selects MAX. All numeric tier rates
accept 1–1000 MHz; defaults are MAX / 8 / 133 for hot / cold / grey.

```sh
RUSTFLAGS='-C target-cpu=native' cargo build -p doslet --release --locked
target/release/doslet --config games/HoMnM.toml \
  --speed-tier-profile target/tier-weighted-profiles/heroes-verified-irq.json \
  --hot-mhz 500 --grey-mhz 100 --cold-mhz 8 \
  --gdb 127.0.0.1:5003 --mcp 127.0.0.1:5004
```

To resume a specific scene, replace `--config games/HoMnM.toml` with
`--load-state target/heroes-states/view-world.pb.gz` (or another local fixture).
The executable, snapshots and generated profiles containing guest instruction
bytes stay local; the repository does not redistribute them.

The CPU looks up labels only when preparing decoded instructions/native traces
or handling an unbatched fallback. Cached instructions carry their tier in unused
metadata bits. Native traces compile path-local HOT/COLD/IRQ counters; GREY is the
remaining completed work. Register-only native blocks use their known iteration
counts. Colours never split JIT blocks or force an exit. Existing device, memory,
control-flow and bounded-execution guards still apply. A changed instruction or
unknown PC becomes GREY; replacing the profile invalidates both execution caches.

The machine charges completed counts once per CPU batch. For 500 / 100 / 8 MHz,
the cost relative to 100 MHz is `0.2 * hot + grey + irq + 12.5 * cold`. Integer rational
arithmetic carries the common fractional remainder between batches. Already
consumed time remains in the guest clock when a quantum ends; excess is not
forgiven at the next quantum. MAX has zero instruction-budget cost. Its bounded
host-work quantum still advances device time when weighted work has not covered
that elapsed time, so all-HOT loops cannot starve timers or input. IRQ entry is
GREY. Confirmed IRQ labels use the same rate as GREY, with separate counters;
there is no per-instruction live IRQ-stack tracing or dynamic IRQ override.

Normal idle-loop proofs and structural VGA wait acceleration remain enabled.
Skipped proven iterations contribute the same per-tier counts as their bodies.
The offline `state_speed_tiers` experiment below retains its older diagnostic
planner and deliberately disables these accelerations; its old measurements do
not describe the new live scheduler.

Click the tier button in the footer for **Use normal CPU** / **Enable speed tiers**.
Ordinary CPU/analyzer controls disable tiers first. GDB bypasses the policy while
attached. The native adapter cannot be combined with the separate adaptive-idle
or synchronization-controller experiments.

The top-right overlay displays recent **Steps** and **Guest time** shares. Steps
include accounted proven-wait iterations; neither row measures OS CPU utilization
or per-colour host execution time. Mixed native blocks cannot supply exact
per-colour host time without finer instrumentation. The overlay remains transparent
to mouse input and clears on pause/toggle/GDB. MCP exposes `speed_tiers.steps`,
`guest_nanos`, numeric rates (null HOT means MAX), and total `execution_nanos`.
The array order is HOT, COLD, GREY, IRQ; `irq_mhz` equals `grey_mhz`. IRQ shares
count instructions carrying a confirmed IRQ label, including calls to the same
code outside interrupts. They are not a measurement of time spent inside IRQs.

### Correct labels through MCP

Use `inspect_code_tags` before `tag_code_range` to correct a loaded profile in
the running native emulator. Both accept decimal physical addresses `start`
and exclusive `end`, plus `bits` (16 or 32). A request spans at most 65536 bytes.
For example, inspect `{"start":306956,"end":306993,"bits":16}` before tagging
that same range with `"tier":"hot"`. These addresses describe one local CIV
474.03 layout; verify the loaded code before reusing them.

Tagging accepts `hot`, `cold` or `grey`. It changes existing instruction labels
whose starts fall in the range and whose saved 15-byte signatures match current
RAM. It does not discover instructions at unknown addresses. Changed code,
conflict guards and confirmed IRQ labels remain unchanged. Results report the
previous tier counts, matching signatures, skipped mismatches, protected labels
and number changed. Shared functions receive the same tag in every caller.

The guest may be running or paused. Tagging requires enabled speed tiers and
no GDB connection or pending machine preparation. It preserves MHz, guest
state, clock fractions and cumulative counters. Manual tags survive automatic
learning, including a manual GREY correction; setting another tag replaces a
previous manual choice. Corrections are saved asynchronously to the active
profile cache, with `save_pending` reporting outstanding writes. Keep an exported
profile in sync if it is explicitly supplied on subsequent launches: a manual
label in that file takes precedence over a conflicting cached label. Normal
shutdown flushes pending writes.

### Scene profiles and repeatable checks

The current combined local profile includes city, menu, puzzle, map and View World.
The four additional scenes use paired three-second guest traces at 133 and 400 MHz.
The city source uses the existing 133/1000-MHz evidence. Scene merging uses
COLD > HOT > ordinary GREY: unknown observations cannot erase known colours, and
growth observed in one scene takes precedence over stable counts in another.
This is an empirical policy, not proof that every growing path is dispensable.
Different code bytes at the same address and IRQ classifications remain guarded
GREY. These guards survive input reordering and subsequent merges.

The original consensus profile (`heroes-scenes.json`) contains 1,446 HOT, 791 COLD
and 4,075 GREY sites. The corrected local `heroes-scenes-cold-first.json` contains
1,586 HOT, 826 COLD and 3,900 GREY sites. Outside IRQ paths, the source profiles
contain 172 sites with both known and unknown observations, only three sites with
both HOT and COLD observations, and no differing code bytes. All three HOT/COLD
sites belong to the same short routine: city counts are 775/775, while puzzle
counts are 92,920/287,123. This supports treating that routine as COLD in the
merged profile, rather than assuming a large population of conflicting routines.
The merge's `conflicts` count now counts distinct addresses with HOT/COLD or code
disagreements, not repeated pairwise disagreements with unknown observations.
The fixture named
`battle-candidate` is a map screen and is not battle evidence.

A three-guest-second interpreter replay at HOT/GREY/COLD 500/100/8 MHz checked
the original and corrected profiles from identical saved states. Puzzle GREY
steps fell from 32.27% to 29.62%; city GREY steps fell from 9.48% to 9.41%.
Each run completed 65 SB16 deadline checks with zero misses, and final VRAM
hashes matched within each pair. City PCM hashes also matched. Puzzle output
had 66,150 versus 66,151 PCM frames at slightly different final guest boundaries,
so it is not a claim of identical PCM output or verified live audio quality.
Artifacts: `target/merge-cold-first-check/{puzzle,city}-{before,after}.json`.
Six merger tests cover unknown evidence, COLD precedence, input permutations,
and persistent IRQ/code guards. These changes affect offline profile merging;
the running GUI retains its in-memory profile until a new profile is loaded.

```sh
python3 tools/merge_speed_tier_profiles.py \
  target/tier-weighted-profiles/heroes-scenes-cold-first.json \
  target/speed-tiers/city-static-grey.json \
  target/tier-weighted-profiles/menu.json \
  target/tier-weighted-profiles/puzzle.json \
  target/tier-weighted-profiles/map.json \
  target/tier-weighted-profiles/view-world.json
RUSTFLAGS='-C target-cpu=native' cargo build -p doslet-headless \
  --example bench_weighted_tiers --release --locked --offline
target/release/examples/bench_weighted_tiers \
  target/heroes-states/city.pb.gz target/tier-weighted-profiles/heroes-scenes-cold-first.json \
  3000 500 100 8 jit target/tier-weighted-results/city.json
```

At equal 133-MHz rates, 10 million city steps produce identical registers, flags,
PC, guest time, conventional/extended RAM, VRAM and PCM with ordinary JIT, labelled
JIT and labelled interpreter execution. Unit tests also check mixed-rate idle
skips, partial native paths, guard exits, modified code, policy toggles and exact
clock accounting. These checks do not establish live audio quality on a host
under load.

### Measured scheduling improvement

A paired desktop-path benchmark compares the previous adapter with the weighted
adapter using the same city snapshot and original city profile. Both builds use
`target-cpu=native`, without instruction profiling, a window or a physical audio
sink. Each trial restores independently, warms for two guest seconds and measures
the next three. Two old/new/new/old groups give six trials per engine/version.

| Engine | Previous host s / guest s | Weighted host s / guest s | Reduction |
| --- | ---: | ---: | ---: |
| JIT | 0.3016 | 0.1257 | 58.3% |
| Cached interpreter | 0.2966 | 0.1051 | 64.5% |

These are median elapsed work times, not OS CPU-utilization percentages. The
interpreter remains about 16% cheaper than JIT in this city comparison. Evidence:
`target/tier-weighted-results/desktop-comparison.json` and adjacent per-group logs.

All 45 additional five-second headless trials (five scenes, three policies, three
repeats) completed with zero new SB16 DMA service misses. Policies were ordinary
133 MHz, MAX/133/8 and 500/100/8. The third repeat overlapped the final build for
puzzle/city/map/View World; those rows are marked `timing_contended_by_build` and
must not support throughput claims. These runs include synchronous JIT compilation
and are separate from the warmed desktop comparison. Results and final PNGs are
under `target/tier-weighted-results/`; they do not substitute for listening to live
audio or verifying every animation deadline.

The native presentation was started from the city snapshot with the merged profile,
`--hot-mhz 500 --grey-mhz 100 --cold-mhz 8 --no-dynamic-core`. MCP confirmed the city
framebuffer and zero guest SB16 deadline misses. Listening and interaction remain
user acceptance checks.

## Classification

`tools/speed_tier_profile.py` compares equal-duration traces from the same
initial PC, guest time and RAM. It rejects incomplete runs, unequal initial RAM
and CPU ratios below two. Counts are normalized per guest second; changed code
windows are not admitted. Each key is `(linear PC, bitness)` with a 15-byte code
window, rather than an entire function.

| Group | Evidence in both traces | Execution rate |
| --- | --- | --- |
| Hot | At least 32 executions/s; higher-budget count is 0.8–1.2 times the lower-budget count | MAX |
| Cold candidate | At least 32 executions/s; higher-budget count grows by at least 1.5 times | 8 or 1 MHz |
| Grey | Other counts, rare paths, unknown code or invalidated code | 133 MHz |

Here, **cold means a candidate for throttling**, not infrequently executed code.
Infrequently executed code is grey. The thresholds are explicit research
parameters, not proven universal boundaries. The original results below used
MAX for IRQ entry and the observed IRQ scope, including nested handlers. Current
profiles use the static grey rule described above.

The city profile compares 133 and 1000 MHz: 708 hot, 971 cold and 2,517 grey PCs.
The intro profile compares 133 and 266 MHz: 2,156 hot, 120 cold and 1,899 grey PCs.
Both training pairs have identical final PNG and complete PCM streams. These
profiles are trained and evaluated on the same short scene interval; this is
not validation on unseen paths or other scenes.

Two earlier variants provide comparisons:

- `--policy stable-hot`: only stable hot PCs use MAX; everything else is slow.
- `--policy growing`: only growing hot PCs are slow; uncertain PCs use MAX.
- `--policy three` (default): hot/MAX, cold/low rate, grey/133 MHz.

## Timing and instrumentation

`state_speed_tiers` enables the native JIT and disables other accelerations.
Each CPU batch runs with device time held constant. The tool then advances PIT,
VGA, keyboard, DSP and OPL time through bounded slices, splitting at device
deadlines. PCM drains every guest millisecond. No instructions are skipped.

MAX has **no configured MHz ceiling**: its charged time is the elapsed host time
around `Session::debug_external_advance`. Profile lookups, graph decoding, probes,
image hashes, JIT preparation and output serialization are outside that timer.
The timed call still includes runtime dispatch overhead; host preemption inside
the call is included. Thus the measured equivalent MHz is not production JIT
capacity and does not include the cost of the whole tool.

Cold time uses four nominal clocks per scheduler step at the selected rate,
with a floor at measured execution time in MAX experiments. Grey uses exactly
four clocks per step at **133 MHz in emulated time**, retaining the fractional
nanosecond remainder. Grey time is independent of diagnostic host cost. An
offline run can therefore take longer than its guest duration. There is no host
sleep or physical audio playback in this test.

Batches end at tier changes, control transfers, I/O, unknown effects and probe
PCs, with at most 64 scheduler steps. REP MOVS/STOS are bounded microsteps.
Pending IRQs force a single step; fixed-rate batches are also bounded by known
service horizons. Hardware events crossed during a CPU batch are serviced at
the next batch boundary. Long host stalls can still delay service; the tool
records the maximum batch duration and batches over 100 us.

## Recorded runs

All replays start from an independent restore, without injected input, and run
for five guest seconds. The three policies each have three repetitions for
both scenes at both cold rates: **36 measured policy runs**. Rates and scenes
alternate, and CPU-heavy builds do not overlap measurements. A separate
500-ms city run checks the harness before measurement. JIT caches are rebuilt
for each restore; these are snapshot replays, not a steady-state JIT benchmark.

The city fixture is `target/heroes-states/city.pb.gz`, documented in the
[static audit](city-state-static-analysis.md). The intro fixture is
`target/startup-timing/intro-state/capture.pb.gz`, captured at guest time
3,000,003,578 ns during the globe animation. This experiment covers a five-second
intro segment, not the entire boot-to-menu interval.

Single fixed-133 controls reproduce all 31 city update images, all 128 intro
copy images and both PCM streams. City completion phases differ by at most
0.069 ms from the earlier instruction-stepped reference; intro phases differ
by at most one nanosecond. Both controls have zero DSP ACK misses and PCM gaps.
Single all-MAX controls also preserve both image sequences and PCM; the city
control has no late update resets. These controls validate the replay path,
not a statistically repeated host-performance baseline.

The city cadence criterion is completion before the next countdown callback,
as defined in the [work-timing audit](city-work-deadlines.md). It is not an
external graphics specification. The intro has no independently established
hard deadline, so identical images with shifted completion times do not receive
a timing pass.

### Earlier two-group results

Values span three runs. Work duration is the median of each run's median.

| City policy | Low rate | Updates per run | Late resets per run | Median update work | Worst release-to-start |
| --- | ---: | --- | --- | ---: | ---: |
| Stable hot/MAX, all others slow | 8 MHz | 31, 31, 31 | 0, 0, 0 | 6.908 ms | 2.361 ms |
| Stable hot/MAX, all others slow | 1 MHz | 28, 28, 28 | 23, 22, 19 | 14.851 ms | 25.558 ms |
| Growing PCs slow, all others MAX | 8 MHz | 31, 31, 31 | 0, 0, 0 | 6.171 ms | 1.660 ms |
| Growing PCs slow, all others MAX | 1 MHz | 29, 29, 29 | 20, 22, 15 | 9.060 ms | 16.462 ms |

All of these runs retain exactly the reference PCM. The city image sequence
matches at 8 MHz and has fewer completions at 1 MHz. In the intro, all four
variants retain 128 identical image-copy outputs, but worst completion shifts
against fixed 133 MHz reach 57–102 ms. Audio alone misses the city's regression.

The animation entry and completion PCs (`1F41D1`, `1F41E9`) occur only 31 times
in each five-second training trace. The service entry `21AC45` occurs 59 times.
These are stable but not hot under the chosen threshold: the first policy
throttles them, whereas the three-group policy marks them grey. The drawing loop
at `21DF08` remains hot. This shows why whole work episodes need separate checks
even when their innermost loops are classified correctly.

### Three-group results: grey fixed at 133 MHz

| Scene | Cold rate | Image completions per run | Late city resets per run | Identical image sequence and PCM? | Largest completion shift versus fixed 133 |
| --- | ---: | --- | --- | --- | ---: |
| City | 8 MHz | 31, 31, 31 | 0, 0, 0 | Both | +22.689 ms |
| City | 1 MHz | 29, 29, 29 | 21, 20, 17 | PCM only; fewer image completions | Ordinal comparison has unequal lengths |
| Intro | 8 MHz | 128, 128, 128 | No established deadline | Both | +59.076 ms |
| Intro | 1 MHz | 128, 128, 128 | No established deadline | Both | +80.319 ms |

City work takes a median 6.232 ms with cold/8 and 9.124 ms with cold/1, compared
with 14.851 ms for the earlier all-others-slow 1-MHz policy. Worst observed
release-to-start latency is 1.930 ms at cold/8 and 14.336 ms at cold/1. The
minimum margin to the next countdown callback is +1.472 ms at cold/8 and
−21.128 ms at cold/1. Long ordinal phase shifts can accumulate across otherwise
timely updates because completion/reset timing affects subsequent releases.

All 36 final policy runs produce 110,250 PCM frames with no gaps, overlaps,
DSP ACK misses or code-window invalidations. One growing-only city/1 run ends
inside an IRQ at the fixed window boundary; all three-group runs end outside
IRQ service. Exact PCM is therefore insufficient to accept the 1-MHz city
variants.

At cold/8, the three-group city median is 21,468,530 executed scheduler steps,
versus 166,250,001 at fixed 133 MHz: **87.1% fewer executed steps** in this
window. The median measured CPU-call time is 0.973 s; the complete instrumented
run takes about 3.18 host seconds for five guest seconds. These numbers are
specific to this diagnostic harness and do not establish a desktop CPU saving.
Grey occupies approximately 0.053% of guest time here; its rate is checked
against `4000 * grey_steps / 133`, within one nanosecond.

A preliminary variant that also limited grey by measured host cost is preserved
in `three-host-limited-trials.json` and the corresponding directories. Its grey
rate fell below 133 MHz, and one city cold/8 run had a late reset. It is a
different timing model from the fixed-grey results above. Together with the
small minimum margins, this is a reason to retain deadline monitoring and fast
recovery rather than treating three short successful trials as universal proof.

## Implication for a controller

Retain grey as the default for insufficient evidence. A cold label should also
carry a maximum permitted traversal time or service-latency bound. The recorded
city's empty dispatch path has 3,231 steps: at 1 MHz that is already about
12.9 ms before adding service or useful work. Executing the eventual hot block
at MAX does not recover the delay incurred before reaching it. The measured
release-to-start latency is consistent with this mechanism; the experiment
does not prove every cold instruction belongs exclusively to waiting.

Promoting rare work to grey is useful, but 1 MHz still fails this city cadence
criterion. Keep the lower rate conditional on measured progress and deadlines,
with a recovery path when the cold interval becomes too long. No such online
recovery controller is added by this experiment.

## Replay commands

These commands now generate the revised static IRQ-grey policy. Existing result
tables above retain the original IRQ-MAX policy; new runs need separate outputs.

Build with the requested native CPU optimization:

```sh
RUSTFLAGS='-C target-cpu=native' cargo build -p doslet-headless \
  --example state_speed_tiers --features profiling --release --locked --offline

python3 tools/speed_tier_profile.py \
  target/city-cpu-confirm/133 target/city-cpu-confirm/1000 \
  target/speed-tiers/city-three.json --policy three

target/release/examples/state_speed_tiers \
  target/heroes-states/city.pb.gz target/speed-tiers/city-three-8-r1 \
  target/speed-tiers/city-three.json tools/fixtures/heroes-city-deadlines.json \
  8 5000 max

python3 tools/compare_frame_probes.py \
  target/city-regulator/reference-133 target/speed-tiers/city-three-8-r1
```

Use cold rate `1` for the lower-rate test. For the intro, build the profile from
`target/intro-cpu-sweep/{133,266}`, restore the intro fixture, and use
`tools/fixtures/heroes-intro-probes.json`. Its reference is
`target/intro-timing/133`. Keep runs serial and alternate rate order for at least
three repetitions. `tools/city_deadlines.py` analyzes city probe outputs; do not
apply its city-specific addresses to intro replays.

Local artifacts under `target/speed-tiers/` include each run's `metrics.json`,
`probes.json`, `audio.s16le`, `final.png`, `frame-comparison.json`, and city
`deadlines.json`. Aggregate files are `trials.json` (growing-only slowdown),
`stable-hot-trials.json`, and `three-trials.json`. Profiles contain local game
code windows and remain outside tracked documentation.

## Code validation

PC/runtime library tests cover normal timing, external-clock bounds, deferred
IRQ delivery, unchanged CPU state during device advancement, DMA/PCM equality,
FM sample/timestamp equality and restoration of ordinary execution. Example
tests cover grey fallback after code changes and exact 133-MHz
grey accounting even when host execution is slower. Python tests exercise
classification policies, saved grey IRQ paths, IRET boundaries, and reject
mismatched RAM or incomplete training runs. Desktop tests cover bounded device
time after a host stall and policy toggles without losing CPU progress.

For the original replay, the native release build, targeted Clippy with warnings denied, formatting,
122 PC library tests, 109 runtime tests, the additional FM timing test, two
example tests, and seven Python profile/frame-comparison tests pass.

For the revised native adapter, six targeted release tests pass, including the
12-second local city restore with mouse motion, continued cold execution after
ten seconds, toggles and Reset. Seven profile-generator tests, native/headless
Clippy and formatting also pass. Before the static IRQ correction, the broader
desktop suite passed 116 tests with 20 ignored; it did not cover the live IRQ-scope
failure. The new city regression extends the former 500-ms smoke test.

## Optional synchronous learning

Start the game from its executable on the interpreter with the existing profile:

```sh
target/release/doslet --config games/HoMnM.toml \
  --speed-tier-profile target/tier-weighted-profiles/heroes-scenes.json \
  --hot-mhz 500 --grey-mhz 100 --cold-mhz 8 --no-dynamic-core \
  --learn-speed-tiers --gdb 127.0.0.1:5003 --mcp 127.0.0.1:5004
```

`--learn-speed-tiers` checks GREY once per host second using **actually executed**
instruction/microstep counts. Proven skipped iterations stay in the existing
Steps overlay but do not trigger profiling. Above 10%, with at least 10,000
executed steps in the window, it captures an independent working state and runs
one synchronous calculation. Fresh executable entry is captured before the first
live batch. The guest stops advancing until both replays finish and their labels
are installed. There is no snapshot queue. An already paused guest starts no
new work; disabling learning prevents subsequent calculations.
The capture shares immutable media;
RAM, device state, writable file overlays and handle positions remain independent.

Each job compares the same one-second interval of **guest time** at the GREY
frequency (capped at 333 MHz) and three times that frequency. Both diagnostic
copies disable JIT and idle/VGA acceleration. This can take substantially longer
than one host second. The owner immediately joins an 8-MiB-stack worker, which
checks its deadline between bounded chunks and discards a pair that faults,
stalls, misses an SB16 service deadline, exceeds its graph bounds or takes over
120 host seconds. Playback is paused and the host-time anchor is reset after
analysis, so the guest does not catch up the profiling pause. Paging and A20 code
aliases are not supported by the tracer.

For instructions seen at least 32 times in both replays, counts within 80–120%
are HOT candidates; growth of at least 50% is a COLD candidate. Other observations
remain GREY. The replay tracks actual interrupt frames and matching CS:IP/SS:SP returns.
Newly discovered instructions executed inside a hardware IRQ receive an IRQ label,
including shared callees. It does not propagate IRQ status through a merged
return-edge graph. For a mid-execution snapshot, the unobserved initial prefix is excluded until an observed
IRQ returns. DPMI IRET/RETF trampolines do not close an IRQ until its saved
caller context is reached. This is an empirical classification, not a proof that arbitrary
code can safely run at a different rate. Learning is monotonic: established HOT/COLD labels are never demoted or
replaced, even when a later short window disagrees. Only previously GREY or
unknown sites may gain a colour, including IRQ. Legacy `interrupt` graph guesses
and `scene-conflict` entries remain GREY initially but no longer block learning.
Only `code-conflict` tombstones remain locked GREY. Confirmed IRQ labels are
retained; existing HOT/COLD labels are not revoked by later IRQ observations.
Changed code still falls back to GREY through normal decode-time validation;
that does not erase the stored label for the original bytes.

At fresh executable entry, zero executed steps establish that the initial prefix
is outside an IRQ, so it is included. This matters for one-time DOS extender work.

The emulation owner accepts results only for the same session epoch, at a batch
boundary. Stable labels remain useful even if startup code has since been unloaded:
their stored bytes must match at CPU decode before they can affect execution.
Code changing within either replay or between the pair cannot receive a label.
It preserves cumulative counters and weighted
clock fractions; decoded/native caches are invalidated when labels change.
Learned labels are saved separately; the supplied JSON profile is not overwritten.
No-change results back off from 2 to 60 seconds; failed captures/replays defer
for 30 seconds. The live guest retains its normal idle/VGA acceleration.
MCP `status.speed_tiers.learning` reports job count, running state, newly added
HOT/COLD/IRQ labels (GREY bookkeeping is excluded), executed GREY percentage,
the latest capture duration in microseconds. For compatibility, `running` and
`pending_snapshots` remain present; they are false and zero when synchronous
maintenance returns.

### Persistent game profiles

Native CLI launches identify the game by SHA-256 of the executable contents.
Use `--learn-speed-tiers` alone to create or extend the game's cached profile;
no seed JSON is required. Optional `--hot-mhz`, `--grey-mhz` and `--cold-mhz`
override cached rates. A fresh profile defaults to 500/133/8 MHz.
Once a game has been launched with recording or `--speed-tier-profile`, subsequent launches
of the same executable automatically restore its labels and HOT/COLD/GREY rates.
Learning starts disabled, even if the saved profile records learning
as enabled. Opt in through the CPU panel or an explicit `--learn-speed-tiers`
launch. Renaming or moving the executable preserves its identity;
changing its contents selects a different profile. IRQ retains the GREY rate.
Use `--no-speed-tiers` to skip automatic activation. Explicit adaptive/analysis
strategies also suppress automatic activation.

Profiles are portable JSON files named `<sha256>.json`, stored under:

- Linux and BSD: `$XDG_CACHE_HOME/doslet/speed-tiers`, or
  `$HOME/.cache/doslet/speed-tiers` when XDG_CACHE_HOME is unset or relative.
- macOS: `$HOME/Library/Caches/doslet/speed-tiers`.
- Windows: `%LOCALAPPDATA%/doslet/cache/speed-tiers`.

Earlier versions used the data directory (`$XDG_DATA_HOME/doslet/speed-tiers`
on Linux); the first use moves that directory into the cache.

`--speed-tier-cache DIR` overrides this location. Without a configured executable,
an explicit base profile uses `<profile>.learned.json`, keyed by the base file's
content hash. The original profile remains unchanged.

Profiles record `layout`, the `(object, base)` pairs of the program's LE objects
at the time the 32-bit labels were recorded. When the desktop finds the objects
elsewhere, for example under another DPMI mode, labels inside them move with
their object. Only bytes of LE relocation fields may differ from the moved code,
whose bytes then replace the recorded ones; other labels are dropped. Profiles
without `layout` are placed by votes: the first four bytes of each label, where
they occur once without relocations in the image's code, name a base. Learned
Heroes labels taken without a DPMI host matched 3.5% of steps under DPMI Direct
before this and 95.5% after it, against 96.5% without the host.

Loaded overlays/modules are not separately fingerprinted. Each label retains
its address, execution mode and instruction-byte guard: changed code falls back
to GREY. This guards code replacement at known addresses; it does not establish
that identical code always performs the same role in different modules.

A single background writer saves after changes, coalesces updates received during
a write, and atomically replaces the cache file. Orderly shutdown joins the writer
and flushes the latest labels. Abrupt termination may lose an outstanding update.
Invalid cache files report an error and fall back to the explicit base profile,
or normal CPU execution when activating automatically. Write failures are reported
without an unbounded timer retry loop. MCP `status.speed_tiers.persistence` reports
restored labels, completed saves and pending updates. Concurrent processes using
the same cache file do not merge their updates.

Local validation covered content identity after renaming/moving the executable,
changed-EXE isolation, malformed files, save failures, updates during an active
write, CLI automatic activation and its disable switch. In a native Heroes run,
an orderly MCP exit followed by a launch without `--speed-tier-profile` restored
5,559 non-GREY labels, HOT 500 / GREY 100 / COLD 8 MHz and enabled learning.
The restored count includes the seed labels and labels added by live learning.

Historical validation of the earlier background learner (local native build,
`target-cpu=native`; this predates synchronous profiling):

- Synthetic tests cover paired-copy isolation, classification thresholds, IRQ
  actual hardware/software interrupt nesting, live-code checks, monotonic HOT/COLD labels and
  recolouring without loss of weighted clock fractions/counters.
- The first implementation allowed uncertain/conflicting windows to revoke
  existing colours and used conservative merged-graph IRQ reachability. A
  live city check found roughly 98% GREY after learning. That regression
  motivated the monotonic rule and actual interrupt-frame tracking above.
- GUI Reset retained the profile in memory and cancelled a pre-reset replay.
  At that stage, a process restart reloaded only the supplied JSON. The later
  persistence implementation described above also retains learned labels.
- Observed live SB16 guest deadline misses remained zero. Host audio underrun
  counters increased during concurrent build/replay work; smooth audible output
  has not been established by these counters. A quiet-host listening check is
  separate from deterministic guest timing.

Evidence logs are local: `target/learning-city.log`, `target/learning-live.log`,
`target/learning-app-tests.log` and `target/learning-resource-tests.log`.

The monotonic correction was checked again from the same local city snapshot:
all 2237 existing HOT/COLD labels remained unchanged. A paired 100/300-MHz
one-second replay produced 412 HOT, 488 COLD and 3296 GREY candidates in 10.63
host seconds. It added no colours to the existing combined profile for this
scene. Two-second weighted replays before/after kept GREY at 9.44% of logical
steps. This fixture is Vulcania; a separate live observation used another city,
so these percentages are not a measurement of that live scene. Evidence:
`target/learning-city-monotonic.log`, `target/learning-monotonic-tests.log`.

The corrected native interpreter presentation was restarted using
`city-v2.pb.gz`, the original combined profile and `--learn-speed-tiers`.
Across the first observed 28 guest seconds, MCP reported zero guest SB16
service misses and zero host audio underruns. The next completed background
pair added zero colours and did not revoke existing labels. This is runtime
counter evidence, not a listening assessment. Log:
`target/learning-monotonic-live.log`.


### Verified IRQ labels

The old graph-reachability `interrupt` reason could cover shared callees and their
ordinary continuations, yet prevented later learning. The live loader now treats
these old guesses as learnable GREY. A paired replay can promote them to HOT,
COLD, or IRQ using the existing interrupt-frame tracker. A fourth decoded label
and counter expose IRQ separately without splitting native blocks or adding live
interrupt-stack instrumentation. IRQ uses the GREY clock. Native trace reports
pack three 9-bit counts (HOT, COLD, IRQ) alongside steps and exit reason; a full
256-step block remains supported. Unknown GREY alone triggers diagnostic work.

From the same saved scene, one paired 100/300-MHz guest-second observation was
applied to the cold-first baseline, followed by two guest seconds at 500/100/8 MHz:

| Scene | GREY before | GREY after | IRQ after |
| --- | ---: | ---: | ---: |
| Menu | 27.14% | 0.23% | 24.78% |
| Puzzle | 29.57% | 0.22% | 26.70% |
| City | 9.37% | 0.45% | 8.32% |

These are logical step shares, including proven skipped iterations. Every replay
kept the established HOT/COLD labels and asserted zero new SB16 deadline misses.
This does not establish equal audio waveforms, live audible quality, or host CPU
savings. Logs: `target/irq-{menu,puzzle,city}-replay.log`. The three resulting
profiles merge without colour conflicts into local `heroes-verified-irq.json`.

Regression checks cover legacy IRQ/scene guesses becoming learnable, code-conflict
protection, confirmed IRQ retention, four-tier idle accounting, a full 256-step
native IRQ counter, and mixed-colour native/interpreter guest-state and time
agreement. Logs: `target/irq-core-tests.log`, `target/irq-app-tests.log`, and
`target/irq-native-differential.log`.
