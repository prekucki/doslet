# Portable guest states

The `save-state` feature captures CPU, RAM, VRAM, modeled device registers and
queues, device-clock fractions, DOS memory allocation, mouse/input state, open
file positions and the writable filesystem overlay. Version 2 stores immutable
files and CD media as typed resource references, not embedded disk contents.
References contain a kind (directory file or ISO 9660), DOS drive/path, byte
length and FNV-1a checksum. The checksum detects accidental content mismatch;
it is not cryptographic authentication. Host paths remain in the launch profile.
Restoration requires matching assets, which may live at different host paths.
Keep game fixtures under ignored `target/`; they contain game data and must not
be committed or redistributed.

The format is Protobuf, implemented with `prost` and checked-in Rust message
definitions. **Cargo builds do not require or run `protoc`.**
The [schema](../crates/foundation/doslet-state/state.proto) documents the wire
fields. Arrays have explicit lengths; integers use Protobuf widths, with checked
conversions to guest register widths and host indices. RAM/VRAM use `bytes`.
The x87 state preserves raw encodings and its internal extended-precision state.
OPL state includes oscillators, envelopes and pending writes; replaying its
register writes alone would not restore the same sound.

The envelope identifies the file format and machine model. Unsupported versions
are rejected. Version 1 (embedded media) and version 2 (external media) remain
readable; new captures use version 3. Version 3 also retains pending memory
patch plans, their deadlines and the completed prefix, so restoration does not
reapply completed patches. Older readers reject version 3 instead of silently
dropping pending edits. Very old state formats have no importer. Future compatible changes
must retain field numbers; removed fields must be reserved. A breaking change
must increment the format or model version instead of guessing how to restore it.
Lazy VGA time is encoded as the effective raster phase and pixel fraction, so
different execution batch boundaries do not produce different clock states.

Saving allocates explicitly outside execution. Loading creates a new owner and
validates the state before returning it; failure leaves an existing session
untouched. Files larger than 1 GiB are rejected. Native code, decode caches and
process-local code identities are rebuilt. The RAM write instrumentation counter
also starts empty: it counts implementation writes and differs between JIT and
interpreter paths even when guest RAM is identical. Analyzer evidence and heuristic proofs
start empty; saved guest time and effective CPU MHz are retained. These are guest
snapshots, not snapshots of a GUI, host audio sink or external MIDI synthesizer.

`Session::save_state()` writes resource references. Use
`Session::load_state_with_media(bytes, configured_session)` to resolve them;
`Session::load_state(bytes)` also accepts states without external resources and
older embedded-media Protobuf captures. Restored handles and writable overlays
belong to the new owner; immutable file/ISO objects are shared with the supplied
session and are never mutated. Missing resources, wrong types/drives and content
mismatches reject restoration before replacing the live guest.
The API supports local synchronous media and RAM overlays. It explicitly rejects
faulted sessions, pending host storage operations, persistent-store leases and
asynchronous/remote CD sources. A desktop profile using the asynchronous storage
adapter must not be silently treated as a complete portable capture. Native
`--learn-speed-tiers` and `--load-state` preload configured local ISO assets to
make independent replays possible; they do not enable remote-resource captures.

## Resume in the native window

```sh
target/release/doslet --config games/HoMnM.toml --load-state target/heroes-states/city.pb.gz
```

`--load-state` accepts raw Protobuf or a `.gz` file. With `--config`, the frontend
preloads local assets and resolves the saved references. When a game executable
is configured, Reset restarts that game from the beginning; the snapshot only
selects the initial position. Without an executable, Reset reloads the snapshot
using the available resources. A version-1 embedded-media capture can still
start without `--config`. Changes remain in the restored in-memory filesystem; they are
not written back to the original installation. Hardware/CPU launch overrides
are rejected when they would conflict with the saved machine.

The native adapter renews the host input epoch, releases saved host keys and
drains already-produced PCM/MIDI before resuming. Device and DMA state are
retained. The runtime `Session::load_state` API still preserves the captured
epoch and queues for deterministic replay. Loading uses a setup thread with an
8-MiB stack and transfers the single new Session owner to the desktop worker;
the decoder thread never executes the guest.

An optional [static speed-tier profile](compatibility/speed-tier-replays.md#try-the-static-profile-in-the-game)
can select hot/MAX, cold/8 MHz and grey/133 MHz execution for the restored scene.

## Heroes headless fixtures

Build the local fixture tool with the native target optimizations:

```sh
RUSTFLAGS='-C target-cpu=native' cargo build -p doslet-headless \
  --example heroes_states --release --locked
mkdir -p target/heroes-states
target/release/examples/heroes_states capture-menu \
  games/HoMnM/HEROES.EXE games/HoMnM/homm1.gog target/heroes-states/menu.pb.gz
target/release/examples/heroes_states --config games/HoMnM.toml play target/heroes-states/menu.pb.gz \
  docs/compatibility/fixtures/heroes-map.json target/heroes-states/map.pb.gz
target/release/examples/heroes_states --config games/HoMnM.toml play target/heroes-states/map.pb.gz \
  docs/compatibility/fixtures/heroes-city.json target/heroes-states/city.pb.gz
target/release/examples/heroes_states --config games/HoMnM.toml play target/heroes-states/map.pb.gz \
  docs/compatibility/fixtures/heroes-puzzle.json target/heroes-states/puzzle.pb.gz
target/release/examples/heroes_states --config games/HoMnM.toml play target/heroes-states/map.pb.gz \
  docs/compatibility/fixtures/heroes-scroll.json target/heroes-states/scroll.pb.gz
```

Each file is a gzip-compressed Protobuf state with an adjacent PNG and JSON
summary. Before publishing a capture, the tool compares the original owner with
an independently restored owner after one million further steps: PCM and the
complete serialized state must match. The saved file still contains the earlier
capture point. Inspect the PNG before accepting the scene label: relative mouse motion
can scroll the map, and a successful input sequence alone does not prove that the
desired screen was reached. `play` accepts guest-time waits, relative movement,
clicks and BIOS keys from a JSON script; it never patches game memory.

```sh
target/release/examples/heroes_states verify target/heroes-states/city.pb.gz
target/release/examples/heroes_states bench target/heroes-states/city.pb.gz 30 3
target/release/examples/heroes_states bench target/heroes-states/scroll.pb.gz 30 3 scroll
```

`verify` first warms one owner for 250 guest milliseconds, then checks an immediate
serialization round trip and matching complete serialized states, timestamped
PCM frames and MIDI bytes after one million further steps in two independent
owners. `bench` loads a fresh state for each fixed/adaptive trial, alternates order
and writes `.bench.json` results. Each trial first runs five guest seconds of
warmup, starting with fresh JIT caches and controller learning. Timed execution
excludes loading/warmup and includes any further synchronous JIT compilation.
Results include actual guest duration, normalized host cost, PCM production,
DMA deadline counters and the MHz timeline. The `scroll` workload briefly touches
alternating map edges each guest second and returns the pointer to the center.
It requires the centered pointer from `heroes-scroll.json`; it does not clamp
the pointer against a corner, which would scroll away from the discovered area.
Inspect its final PNG to check terrain coverage. Run trials without concurrent
builds or other benchmarks. It is a
headless guest execution comparison, not a desktop CPU or audible-playback test.
Guest SB16 DMA deadline counters do not establish host playback continuity.

If a continuation comparison fails, set `DOSLET_STATE_DEBUG_DIR=target` to
write the two uncompressed Protobuf states for diagnosis. These files also
contain game data. Successful verification compares PCM timestamps and sample
durations as well as sample values; it compares MIDI output byte for byte.

## Recorded local validation (2026-10-17)

The native tool created and visually checked menu, TUTORIAL world map, Vulcania
city, puzzle and centered-pointer scrolling fixtures. Each capture passed the
one-million-step original/restored comparison. The files are about 539 MB before
compression and 400 MB compressed because they embed the game files and CD.
No battle fixture has been captured; these scenes do not establish battle
compatibility.

Runtime tests cover nonaligned CPU/device timing, RAM, input, active OPL audio
and an open writable file. A VGA regression test checks equivalent lazy clock
phases. Workspace tests, Clippy and the `wasm32-unknown-unknown` save-state build
passed. Actual replay was native; a cross-architecture replay was not measured.

## Background replay copies

`Session::capture_working_state()` copies CPU/RAM/devices and mutable filesystem
state at a batch boundary. It shares immutable media through `Arc`, without
encoding or copying CD contents. `WorkingState::restore()` constructs an
independent guest on the profiling thread. It has no live input, output sink,
storage lease, native code or learned execution policy. Writable overlays and
handle positions are copied, not shared. This in-process representation is
separate from the portable Protobuf file format.
Working copies also retain an enabled experimental DPMI host, including its
allocator, registered callbacks and active return frames. Speed-tier learning
can replay these copies even though portable DPMI save files remain unsupported.

Headless `heroes_states`, `state_graph_trace`, `state_speed_tiers` and
`bench_weighted_tiers` accept `--config FILE` to resolve version-2 references.
For example, convert a local embedded-media fixture without overwriting it:

```sh
target/release/examples/heroes_states convert \
  target/heroes-states/city.pb.gz target/heroes-states/city-v2.pb.gz
target/release/examples/heroes_states --config games/HoMnM.toml verify \
  target/heroes-states/city-v2.pb.gz
```

A local Heroes city conversion reduced the uncompressed Protobuf payload from
539,410,274 to 18,610,054 bytes. Restoration with `games/HoMnM.toml` matched the
complete state and PCM after 1,000,000 additional steps. Tests also reject changed
file/ISO content and verify shared immutable media pointers, independent RAM,
file overlays and CD handle positions. Logs: `target/learning-convert.log` and
`target/learning-v2-verify.log`. Compression size and host setup time are separate
from these payload measurements.
