# Quake optimization plan

Status: deferred follow-up, saved on 2026-10-22.

## Goal and current evidence

Target 60 displayed FPS in the agreed Quake workload, while preserving guest
behavior. The target has not been reached or validated. Record the scene,
resolution, build and host configuration before comparing FPS.

The current implementation includes DJGPP/COFF discovery, bounded AOT regions,
experimental PC24 x87 lowering, procedure analysis before WASM generation,
per-entry admission statistics and synchronous tier learning. These features
do not establish an end-to-end performance improvement.

The most recent short native replay covers 40M scheduling steps, approximately
0.60 guest seconds. All-colors AOT took 0.543268 s versus 0.411155 s for the
interpreter: 32.13% additional host time. Filtering by procedure colors and
raising the ordinary work limit did not beat the interpreter in that window.
Removing instruction budgets/accounting in synthetic loops produced small,
overlapping timing differences. That experiment was removed; it is not the
next optimization priority. See [the measurements](x87-performance.md).

Tier labels describe work versus speed-sensitive waiting. HOT is not a ranking
of execution frequency. Labels currently control execution rates and AOT
admission/accounting; they do not yet drive compiler inlining or region layout.

## Ordered work

### 1. Locate the cost in the real replay

- [ ] Reproduce the baseline from the same saved guest state and executable.
- [ ] Use local `perf` to separate admission/source checks, CPU and FPU state
  transfer, Rust/WASM transitions, generated code and remaining interpretation.
- [ ] Report absolute host time and execution coverage alongside the breakdown.
  Keep instrumentation runs separate from uninstrumented timing runs.

Deliverable: a ranked cost report that identifies the first optimization to try.
Do not infer CPU-time shares from instruction coverage alone.

### 2. Record execution information for the compiler

- [ ] Add an opt-in execution profile recording block/entry frequency, edge
  frequency, instructions per entry and reasons for returning to the interpreter.
- [ ] Record an execution-weighted histogram of runs between x87, CALL/RET,
  INT/I/O, memory guards, budget boundaries and loop exits.
- [ ] Bind the profile to executable identity, code bytes and relocatable layout;
  reject stale evidence and keep manual tier labels protected.
- [ ] Rank regions by executed work and boundary cost, separately from tier color.

Deliverable: a repeatable profile of the Quake workload suitable for selecting
regions and call edges to optimize.

### 3. Optimize the measured dominant cost

Choose one change at a time from the evidence in steps 1 and 2:

- Larger regions or calls within WASM when frequent exits dominate. Consider
  inlining selected call edges and retaining small functions as dependencies.
- Smaller FPU transfers or persistent x87 locals when boundary conversion
  dominates. Procedure colors guide the analysis, but declared balanced exits
  alone do not prove intermediate refusals safe.
- Hoisted or reused memory/source proofs when guards dominate, preserving
  invalidation for self-modifying code and segment-relative operand slots.
- Additional x87 operations or graph shapes when unsupported code limits useful
  coverage. Preserve exact numerical behavior and interpreter fallback.

Deliverable: one measured improvement with a differential correctness check.
Keep bounded execution and guest hardware deadlines. An OS CPU limit does not
replace guest-time accounting.

### 4. Validate the improvement and measure FPS

- [ ] Repeat timing in rotating A/B order with compilation excluded.
- [ ] Compare complete guest state, RAM/VRAM, scanout, audio and timing/accounting
  outputs on the same workload. Explain any intentionally different diagnostics.
- [ ] Once AOT shows a reproducible gain, run the agreed timedemo and displayed
  FPS check. Record scene, resolution and frame count; do not convert a short
  CPU replay directly into a gameplay FPS claim.
- [ ] Measure native Wasmtime and production browser/WASM separately.

## Resume commands

Record or extend the game's tier labels; no seed JSON is required:

```sh
cargo run -p doslet --release --locked -- \
  --config games/Quake.toml --learn-speed-tiers \
  --hot-mhz 500 --grey-mhz 266 --cold-mhz 8
```

Learning pauses live execution and playback, computes one paired replay,
installs its labels and resumes without catching up paused host time. The
per-game cache also identifies executables inside ZIP archives. Later ordinary
launches restore labels with learning disabled.

Reproduce the short native color-selection comparison:

```sh
QUAKE_COMPARE_STEPS=40000000 QUAKE_COMPARE_ENTRY_FILTER=1 \
QUAKE_COMPARE_TIERS=1 QUAKE_COMPARE_PC24=1 QUAKE_COMPARE_COLOR_TIMING=1 \
RUST_MIN_STACK=33554432 cargo test -p doslet --release --locked --bin doslet \
  quake_aot_matches -- --ignored --nocapture --test-threads 1
```

The first command records tiers. The richer compiler execution profile in
step 2 remains to be implemented. Keep local game assets and profiling outputs
out of version control.
