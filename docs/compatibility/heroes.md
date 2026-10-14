# HEROES.EXE startup evidence

Local input: `games/HoMnM/HEROES.EXE`. Its files remain unchanged.

The initial run stopped on `D9 C9` at `0180:0023772E` (FXCH ST(1)).
After register exchange was added, the next instruction was FDIV ST(0), ST(1)
at `0180:00237730`. This is a Pentium FDIV defect probe: the program computes
4195835 / 3145727, multiplies the quotient back, subtracts it from the original
numerator and compares the residual. The probe requires rounded arithmetic;
the previous exact-quotient-only implementation stopped explicitly.

The x87 unit now retains a 64-bit finite significand, applies guest precision
and rounding control, and implements the probe's FXCH, FDIV, FMULP, FSUBP and
memory FCOMP forms. A native x87 comparison agreed on significands, signs,
exponents, precision exceptions and C1 for 360 cases. The reference vectors
are retained as an unsafe-free regression fixture. A separate regression
checks the complete probe and its zero residual at extended precision.

The recorded run reached INT 2Fh/AX=1500h before CD-ROM support existed, so that
session reported zero installed drives. It exited normally with status 1 after
printing `Unable to access CD Drive.` at 25045764 steps. Runtime ISO mounting
and basic MSCDEX sector services have since been added, but this game has not
been rerun with the new support. Directory lookup and DOS file access through
the CD drive remain necessary; the recorded result does not establish menu or
gameplay compatibility.

Reproduction:

```sh
CARGO_TARGET_DIR=target/civ-probe cargo run -p doslet-headless --release --locked \
  --example inspect_civ -- games/HoMnM/HEROES.EXE '' 100000000
```
