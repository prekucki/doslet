# Working on Doslet

## User constraints

- Reads and edits in this repository and Cargo commands have standing user
  approval. Ask before other tool operations unless already approved; stay
  within the approved scope.
- Do not use Node.js, npm, npx, Node-based auxiliary tools or Node-based CI
  actions. This includes environment checks and test automation.
- Use Cargo for native builds and the Rust Trunk tool for browser builds. The
  explicit mise tool identifier is `cargo:trunk`, not the ambiguous `trunk`.
- Keep the local `PRINCE` game directory/symlink ignored. Read its data only for
  authorized compatibility work; do not modify or redistribute its files.

## Architecture

- Read `docs/SPEC.md` and `docs/architecture.md` before extending components.
- Keep chips in independent crates under their hardware domain. CPU libraries
  depend on bus contracts, not PC devices, DOS or a frontend.
- Preserve `no_std` for emulation libraries and static bus dispatch in hot paths.
- Give each machine one mutable owner. Borrow guest buffers; avoid RAM/VRAM
  clones, per-step allocations and interior-mutability wrappers in execution.
- Keep large buffer ownership and presentation conversion explicit. Small
  register values and typed events may be copied by value.
- Run bounded execution batches. REP instructions must yield between bounded
  operations. Host refresh rate must not define guest hardware timing.
- Report unsupported behavior explicitly. Keep documented implemented scope
  aligned with tests and do not claim PRINCE runs from synthetic demo evidence.

## Validation

Use the commands in `README.md`. The allocation regression test measures the
guest demo and conversion after setup. Game compatibility needs a recorded real
run; compilation, header inspection and a synthetic benchmark are separate
evidence. Keep `Cargo.lock` checked in for application reproducibility.
