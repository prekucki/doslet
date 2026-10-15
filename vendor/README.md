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
