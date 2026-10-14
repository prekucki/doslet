# Dependency review

Review date: 2026-10-14. The current dependencies are broadly reasonable for the
emulator/frontend split. The main maintenance issues are the old ZIP branch,
two unmaintained UI dependencies, and an older native audio backend. No known
vulnerability advisories matched the scanned graph. This is a dependency review
and RustSec scan, not a complete security audit of every crate or native library.

## Snapshot and method

- 26 workspace packages and 16 distinct direct external dependency names.
- 432 resolved external package/version pairs across all platforms and
  development/build dependency resolution.
- The desktop application's Linux normal-dependency graph contains 224 external
  package/version pairs; its WASM graph contains 130. These counts exclude
  build/dev edges and count repeated graph appearances once. They are not binary
  size or compile-time measurements.
- External sources in this snapshot are crates.io; no Git dependencies or
  alternate registries were found. Cargo-deny source checks passed.
- Cargo.lock SHA-256:
  `89915970016be7b28543d28a8d39e87d8af21a5160fb1c5538f14e6587364d99`.
- Tool: cargo-deny 0.20.2. RustSec database revision:
  `edb58f5974b83eb8302332821cb6d389777aa351`.

Cargo metadata, normal dependency trees, reverse inclusion trees, license
inventory, selected source/build scripts, and upstream documentation were
reviewed. The RustSec/source checks ran for the full workspace graph and
separately for `x86_64-unknown-linux-gnu` and `wasm32-unknown-unknown`.
This covers the enabled feature graph, not every unused optional feature.

## RustSec findings

| Dependency | Finding | Inclusion and practical scope |
| --- | --- | --- |
| `ttf-parser 0.25.1` | Unmaintained, RUSTSEC-2026-0192 | iced → cosmic-text/fontdb; also winit → sctk-adwaita → ab_glyph/owned_ttf_parser. Present in Linux and WASM font handling. |
| `paste 1.0.15` | Unmaintained, RUSTSEC-2024-0436 | iced → wgpu → wgpu-hal → metal. Found in the all-platform graph through Apple's Metal backend; not in the checked Linux/WASM graphs. |

These are maintenance advisories, not reported vulnerabilities. The checks exit
with status 1 because this review's strict default policy treats unmaintained
crates as errors. Full graph: two advisory errors; Linux/WASM: one each. Source
checks report zero errors. No vulnerability advisory matched in these runs.
See the primary [ttf-parser advisory](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)
and [paste advisory](https://rustsec.org/advisories/RUSTSEC-2024-0436.html).

Updating a direct dependency cannot be assumed to remove these crates. iced and
its font/GPU dependencies must adopt replacements upstream, or a narrowly
reviewed patch must be tested. Do not silently suppress the maintenance findings
or replace iced as part of this review.

## Direct dependencies

Versions are the resolved Cargo.lock versions. The assessments below are
engineering judgments based on the stated evidence.

| Dependency | Assessment and action |
| --- | --- |
| `iced 0.14.0` | Fits the shared native/WASM UI requirement and is the current documented release. Most of the graph belongs to UI/rendering/font/platform support. Keep it; track the two maintenance findings and measure before changing features. [Upstream package](https://docs.rs/crate/iced/0.14.0) |
| `clap 4.6.7` | Current documented release; appropriate for explicit CLI parsing and validation. Native-only. Keep. [Upstream package](https://docs.rs/crate/clap/4.6.7) |
| `toml_edit 0.25.17+spec-1.1.0` | Current documented release in the same project as `toml`. It already existed through proc-macro-crate. Native parser enables only `parse` with defaults disabled. Appropriate; a typed `toml` + Serde schema could reduce manual validation code as profiles grow. [Project](https://github.com/toml-rs/toml), [API](https://docs.rs/toml_edit/0.25.17+spec-1.1.0/toml_edit/) |
| `zip 2.4.2` | Highest update priority. Upstream's security policy supports only the latest release, documented as 9.0.3 at review time. Plan a controlled migration, retaining bounded decoding and archive-path tests. [Security policy](https://github.com/zip-rs/zip2/blob/master/SECURITY.md), [current package](https://docs.rs/crate/zip/9.0.3) |
| `cpal 0.15.3` | Established RustAudio backend, but this resolved version dates to 2024-03-05; the current documented release is 0.18.2. Plan a separate upgrade with real device/buffer/pause/resume tests. OS support and API changes need review; compilation alone cannot validate audio behavior. [Upstream package](https://docs.rs/crate/cpal/0.18.2) |
| `midir 0.11.1` | Current documented release; fits host MIDI access. Native-only, with platform FFI. Keep the current sending-thread and reconnect tests; validate actual ports on the intended OS. [Upstream package](https://docs.rs/crate/midir/0.11.1) |
| `moont 1.0.0` | Specialized CM-32L emulation backend based on Munt. Exact version pinned, default features disabled, no enabled optional features in metadata. Keep the faithful user-ROM route, but prioritize focused review/fuzzing of ROM/SysEx handling and binary distribution licensing. The author's Munt-matching claim is not an independent audit. [Author README](https://docs.rs/crate/moont/1.0.0/source/README.md) |
| `sha1 0.10.7` | RustCrypto implementation used for identifying supported ROM contents. This is not signature verification or authentication. No reason to replace it solely because SHA-1 is unsuitable for new cryptographic security protocols. [Upstream package](https://docs.rs/crate/sha1/0.10.7) |
| `wasm-bindgen 0.2.129`, `js-sys 0.3.106`, `web-sys 0.3.106`, `wasm-bindgen-futures 0.4.79` | Coherent browser-binding family; CI pins the matching wasm-bindgen CLI. Keep versions/tooling aligned. web-sys features are explicitly selected. [Binding package](https://docs.rs/crate/wasm-bindgen/0.2.129) |
| `web-time 1.1.0` | Small host-clock compatibility adapter. Appropriate at the frontend boundary; no reason found to replace it. |
| `log 0.4.34` | Standard logging facade, used on the browser path. Keep. |
| `console_error_panic_hook 0.1.7` | Browser panic diagnostics. Its age alone does not establish abandonment; no advisory matched it. Keep. |
| `allocation-counter 0.8.1` | Development-only allocation regression instrumentation. Not a production execution dependency. Keep meaningful allocation tests. [Upstream package](https://docs.rs/crate/allocation-counter/0.8.1) |

### ZIP exposure

The reviewed browser archive adapter rejects symlinks, duplicate/unsafe DOS
paths, excessive entry counts, declared expansion sizes and mismatched actual
sizes. It reads bounded file data into a virtual drive instead of calling
`ZipArchive::extract` on the host filesystem. The Worker shares this adapter.
The desktop's current native path uses mounted host directories.

The extraction vulnerability
[RUSTSEC-2025-0168](https://rustsec.org/advisories/RUSTSEC-2025-0168.html)
was patched in versions >=2.3.0; our 2.4.2 is outside its affected range.
The recommendation to update comes from upstream support policy and maintenance
distance, not a claim that this advisory affects Doslet.

### moont and unsafe code

The selected source contains unsafe byte-to-raw-structure casts in ROM and SysEx
parsing. Their existence alone does not prove unsoundness. Some reviewed ROM
parsers check lengths and metadata bounds, and our wrapper verifies supported
ROM digests before constructing the engine. The workspace's `unsafe_code =
"forbid"` applies to our crates; it does not certify dependencies as unsafe-free.
Allocation tests and MIDI/ROM functional tests also do not prove memory safety.

`bundle-rom` is not enabled. The build script's ROM-loading/generation branch is
conditional on that feature. Metadata shows no optional moont features enabled;
its optional tracing dependency is inactive. Keep that invariant when adding
browser ROM selection. moont/sha1 currently appear in the desktop package's
WASM dependency graph even though browser ROM selection is pending; native-only
gating could reduce build work, but linked-size savings have not been measured.

## Licenses and distribution

Every package in the cargo-deny inventory has a recognized license. Most
external packages offer MIT/Apache-2.0 or another permissive license. The
notable direct exception is `moont` under `LGPL-2.1-or-later`; the workspace is
GPL-2.0-or-later. A license inventory is not proof that binary redistribution
requirements are satisfied. Review source/notices/relinking provisions for the
chosen distribution method before publishing binaries. See the
[license text](https://spdx.org/licenses/LGPL-2.1-or-later.html).
Game assets and Roland ROMs are separate from crate licensing and remain
user-provided; this review does not grant redistribution rights for them.

## Architecture, build cost and process

The CPU/DOS/PC runtime normal graphs contain only local emulation crates. Large
third-party graphs are concentrated in application presentation and OS adapters.
The desktop and Worker disable ZIP default features; iced disables defaults and
selects platform/rendering features. This is a good boundary and should remain.

Several dependency families have multiple resolved versions, especially platform
bindings, font libraries, hashbrown/rustix and error helpers. This is not itself
a vulnerability. Do not force incompatible versions together or remove supported
X11/Wayland backends without a measured need. Build scripts and native FFI are
part of the trust boundary; RustSec does not audit system ALSA/drivers or prove
that build scripts are harmless.

CI currently checks formatting, Clippy, tests, native builds and WASM/Trunk
builds with a lockfile and pinned tooling. It has no automatic advisory/source
scan. Recommended order:

1. Plan and validate the ZIP upgrade.
2. Add a pinned Rust-only cargo-deny advisory/source check in CI, with explicit,
   documented treatment of the two upstream maintenance advisories. Use plain
   shell/Cargo steps; no Node-based action.
3. Track iced/font/GPU migrations that remove ttf-parser/paste.
4. Upgrade CPAL separately with real audio-device evidence.
5. Review moont parsing and distribution obligations; preserve the exact pin,
   no-bundled-ROM invariant and runtime tests.

No dependency upgrades or CI policy changes were made during this review.
The preceding TOML launch implementation adds toml_edit as a direct native
dependency while reusing the already resolved version.

## Reproduction

```sh
cargo metadata --format-version 1 --locked --offline > /tmp/doslet-dependencies.json
cargo tree --workspace -i ttf-parser --target all --locked
cargo tree --workspace -i paste --target all --locked
cargo tree -p doslet --target x86_64-unknown-linux-gnu --edges normal --locked
cargo tree -p doslet --target wasm32-unknown-unknown --edges normal --locked
```

For the security scan, the temporary review policy was:

```toml
[advisories]
db-path = "/tmp/doslet-rustsec"
[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

Run cargo-deny 0.20.2 with this policy; fresh database fetching requires network
access. Local mise has this tool installed; select `cargo:cargo-deny@0.20.2`
explicitly if the unconfigured shim cannot run:

```sh
mise exec cargo:cargo-deny@0.20.2 -- cargo deny --locked \
  --metadata-path /tmp/doslet-dependencies.json \
  --config /tmp/doslet-deps-deny.toml check advisories sources
```

Repeat with one `--target x86_64-unknown-linux-gnu` or
`--target wasm32-unknown-unknown` option before `check` for platform-specific
results. Do not interpret exit status 1 here as evidence of a vulnerability:
inspect whether the diagnostic is `unmaintained` or `vulnerability`.
