# Release readiness: native and browser CPU modes

Audit date: 2026-10-24. Branch: `feat/display-fullscreen-ravu`, based on
`0f6af3d`, including the integration changes committed with this report. Package version
remains `0.1.0`. This audit does not create a release tag or publish artifacts.

The automated Linux and Firefox checks pass. The branch is a candidate for an
**experimental release**; v86 game compatibility and browser performance still
have the limits listed below. Passing these checks does not certify every DOS
game, physical audio output, Windows or macOS.

## Changes covered

- Native v86 uses a trusted, embedded Wasmtime-precompiled core. External
  overrides accept raw WASM, not serialized native code.
- Browser packaging selects `simple`, `aot` or `v86`. v86 and Doslet devices
  share the machine Worker's WASM memory; audio runs in an AudioWorklet.
- `[game] mode` selects the backend in game profiles and the native library
  editor. Explicit CLI mode overrides the profile.
- The branch includes the configuration window, display scaling/fullscreen,
  RAVU, compact toolbar/status bar and sound-card/mixer changes.
- IRQ1 input wakes a halted v86 CPU even with IRQ0 masked. Disabled gameport
  reads return `FFh`; enabling the gameport still has a timing issue.

## Audit fixes

Formatting now checks workspace members and the standalone v86 tool separately,
preserving upstream-generated source formatting. CI installs the required
native headers and Clang, and builds both ordinary and v86 browser Workers.
The browser linker cache key includes SoftFloat object contents and linker
wrapper source, so changes cannot reuse stale linked WASM.

Twelve runtime v86 tests now use the embedded core and run automatically,
instead of remaining ignored pending a manual module build. Browser tests
now respect MPU transmit timing, exercise relative mouse use before capture,
and navigate the current display panel. Bundle tests check the selected mode,
actual translated execution and reset. Original fixture generation is checked
in; commercial game assets are not part of these tests.

## Verification results

| Check | Result |
| --- | --- |
| Workspace formatting and standalone v86-tool formatting | Pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass |
| Workspace tests, including doc tests | 1,998 passed, 0 failed, 106 ignored |
| Runtime v86 integration, debug and release | 15 passed in each profile |
| v86 host integration, release | 23 passed |
| Desktop without default features | Compiles |
| Frontend, Worker, AudioWorklet and libraries for `wasm32-unknown-unknown` | Compiles |
| Standalone v86 build tool | Compiles; contains no unit tests |
| Release desktop, headless and web-builder binaries | Built |
| Headless guest demo | Exits successfully; VGA output present |
| Browser production builds | Ordinary and v86 Trunk builds pass |
| Packaged `simple`, `aot`, `v86` sites in Firefox | Launch, pause and reset pass; AOT/v86 execution counters increase |
| Worker/AudioWorklet smoke | Pixels, MIDI, PCM DMA, blocked-window independence, pause/reset and pointer lock pass |
| Web MIDI device mock | Message framing, timestamps, disconnect/recovery, MT-32 translation and SysEx pass |
| Browser component acceptance | Storage, input/gameport mock and local HTTPS Range/ISO checks pass |
| Firefox WebGL renderer | xBRZ compiles and draws; Nearest stops shader draws; resize passes |
| Firefox Canvas2D renderer | GPU-filter rejection preserves the image; resize passes |

Debug workspace tests use `RUST_MIN_STACK=8388608`. Release v86 tests pass
without this override. No experimental LLVM flags were introduced. The
remaining ignored tests include local-game fixtures, benchmarks and physical
or platform-specific integration; their presence is not a pass result.

The initial sandbox run could not bind local test TCP sockets. The complete
suite was rerun with socket access and passed. Browser tests use disposable
headless Firefox profiles and mute the sink: sample assertions are not an
independent listening test. Mock MIDI and gamepad checks do not validate a
physical controller or synthesizer.

## Reproduce

Use the toolchain and dependencies in [development](development.md) and
[v86 setup](compatibility/v86-core.md). From the repository root:

```sh
cargo fmt --check
cargo fmt --manifest-path tools/v86-build/Cargo.toml --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUST_MIN_STACK=8388608 cargo test --workspace --locked
cargo test -p doslet-runtime --release --locked --features v86 --test v86
cargo test -p doslet-v86-host --release --locked --test core
cargo check -p doslet --no-default-features --locked
cargo check -p doslet -p doslet-web-worker -p doslet-web-audio -p doslet-time -p doslet-executable --target wasm32-unknown-unknown --locked
cargo test --manifest-path tools/v86-build/Cargo.toml --locked
cargo build -p doslet -p doslet-headless -p doslet-web --release --locked
cargo run -p doslet-headless --release --locked -- demo
```

Generate redistributable test inputs and build a v86-capable distribution:

```sh
python3 tests/web_release_fixture.py target/release-fixture
cargo run -p doslet-web --locked -- build target/release-fixture/game.toml --mode v86 --no-saves -o target/release-fixture-site
python3 tests/browser_smoke.py
python3 tests/browser_components.py
python3 tests/browser_xbrz.py
python3 tests/browser_xbrz.py --software
python3 tests/web_bundle_smoke.py target/release-fixture/game.toml --mode simple --web-dist web/dist
python3 tests/web_bundle_smoke.py target/release-fixture/game.toml --mode v86 --web-dist web/dist
python3 tests/web_bundle_smoke.py target/release-fixture/aot.toml --mode aot --aot-profile target/release-fixture/aot-profile.json --web-dist web/dist
```

Choose a new `-o` directory when repeating a build. `--web-dist` takes the raw
Trunk distribution, not an already packaged game site. The AOT fixture uses
flat protected-mode segments and an exact source-bound profile; unsupported
COM programs are correctly rejected by `--mode aot` without such entries.

Local evidence is in `target/release-audit-*.log`. Native binaries are under
`target/release/`; `target/release-audit-artifacts.json` records their sizes and
SHA-256 hashes alongside the final browser Worker/AudioWorklet. Synthetic sites
are in `target/release-ready-simple`, `target/release-ready-aot` and
`target/release-ready-v86`. These build outputs and logs are intentionally
untracked.

## Known limits before distribution

- **Enabled gameport with v86:** ROTT may override keyboard direction with
  a false joystick direction. Disable its joystick or the Doslet gameport;
  see the [known bug](backlog.md#gameport-timing-differs-between-cpu-backends).
- **Browser v86 performance:** successful translated execution is verified,
  but this audit does not establish playable ROTT/CIV/Quake performance or
  parity with native Wasmtime. Keep the backend marked experimental.
- **Chromium WebGL RAVU:** the configured simple filter is used to avoid the
  reported wgpu buffer-alignment panic. Firefox rendering was retested here;
  Chrome and WebGPU were not rerun in this audit.
- **v86 debugging/state:** GDB, instruction tracing, other CPU pacing policies
  and portable v86 state saving remain unsupported; see the
  [backend contract](compatibility/v86-core.md).
- **Physical/platform acceptance:** macOS/Windows, physical system-mixer
  playback and long real-game sessions need their own recorded runs. Existing
  reports of system-mixer interruption are not disproved by muted Worklet
  tests or by compilation.

Retain the upstream license notices when distributing binaries, including
v86/SoftFloat and the graphics filters. See [license details](../README.md#license).
