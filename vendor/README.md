# Local iced pointer capture patch

These are registry sources of `iced_runtime` 0.14.0 and `iced_winit` 0.14.1,
licensed under [MIT](LICENSE-iced). Cargo patches select the local packages;
all other iced packages and winit remain locked registry dependencies.
Upstream: <https://github.com/iced-rs/iced>.

iced 0.14 exposes cursor appearance but no cursor-grab operation or relative
device-motion stream. Doslet needs both for DOS games that use INT 33h motion
counters. The local changes add `window::capture_mouse`/`release_mouse` and a
bounded task stream, route winit device motion and mouse buttons to that stream,
and restore the cursor on Esc, focus loss, close or stream disconnection.
Capture tries Locked, then Confined for platforms such as X11. A full event
channel releases capture instead of losing a button release. Capture streams
end with Released, even if the channel was full. Iced's normal absolute pointer
events are suppressed during capture so widgets cannot receive guest clicks.

Two existing deprecated `try_next` calls use the equivalent `try_recv` API to
keep local builds free of deprecation warnings. No renderer or guest-machine
logic is changed. Browser capture uses DOM Pointer Lock in Doslet's bridge.

## Nuked OPL3 portable state

`nuked-opl3/` contains the registry source of `nuked-opl3` 0.1.0,
licensed under [LGPL-2.1-or-later](nuked-opl3/LICENSE).
Upstream: <https://github.com/tgies/nuked-opl3-rs>.
The local patch adds the optional `save-state` feature and checked-in Protobuf
bindings in `src/core_saved.rs` and `src/lib_saved.rs`. It exposes complete chip
state through `doslet_state::State`; no synthesis algorithm was changed.
Cargo builds use `prost` derives and do not invoke `protoc`. Keep the upstream
license files with this copy. Verify active FM continuation when updating it.

## v86 CPU/JIT

`v86/` contains the pinned upstream Rust CPU, WASM JIT, instruction generators
and SoftFloat implementation. Native Cargo builds compile a separate WASM
module and embed a Wasmtime-precompiled artifact. Browser builds link the CPU
into the machine Worker's WASM memory. `tools/v86-build` also exports the
standalone module and optionally regenerates instruction tables without Node.
See its [provenance and local
changes](v86/README.md) and [build/validation instructions](../docs/compatibility/v86-core.md).
