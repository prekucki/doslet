# PRINCE: original DOS toolchain evidence

Inspected 2026-10-14 from the ignored, read-only local installation. This covers
the original DOS binaries being run in Doslet; modern re-releases have separate
builds. No original source, project files or development logs are available here.
String locations below are hexadecimal file offsets, not guest code addresses.

Reproduce the read-only inventory:

```sh
cargo run -p doslet-headless --example inspect_music --locked -- PRINCE
```

The example inventories music and scans printable signatures in `.EXE` and
`.386` files. Absence of a matching signature does not exclude a library.

| Component | Local evidence | Conclusion and limit |
| --- | --- | --- |
| Watcom C/C++32 runtime | `PRINCE.EXE` at 00000371: Watcom C runtime notice; at 000A9475: Watcom C/C++32 runtime notice. `SETSOUND.EXE` has corresponding notices at 00000371 and 0001104D. | Confirmed linked Watcom runtime; strong evidence for the Watcom C/C++ toolchain. Compiler release, build flags and the division between C/C++/assembly remain unknown. |
| C++ exception support | `PRINCE.EXE` at 000D8F23 and following: exception-specification, throw/unwind/destructor and terminate diagnostics. | Confirmed C++ runtime support is linked. This does not prove every game module was written in C++. |
| DOS/4GW 1.97, Rational Systems | `DOS4GW.EXE` at 0003EB92: version 1.97, alongside DOS/4GW identity/copyright notices. `PRINCE.EXE` at 000D9094: Rational DOS/4G identity. The game has an LE payload and actual startup loads the extender. | Confirmed DOS protected-mode runtime in this installation; it supports the game's 32-bit x86 execution. It is not a graphics/game engine. |
| Sound Drive / JVS sound configuration | Both the game and `SETSOUND.EXE` contain Sound Drive compatibility messages. The game has `jvsound.cfg` references at 000D62C4/000D62D2; installation metadata specifies `SETSOUND.EXE` and `JVSOUND.CFG`. | Strong evidence for the named Sound Drive audio layer and its configuration/setup tools. Exact SDK vendor, release and whether all routines are external or game-local have not been established. Do not expand the JVS initials to an unverified person's name. |
| SVGA/VESA graphics modules | `AT640480.386`, `S3640480.386`, `VS640480.386` and other card/mode modules. The executable contains VESA and VESA480 selections and a graphics-mode error. | Confirmed modular 640×480 SVGA/VESA support. The library/SDK author is unidentified; filenames do not prove Fastgraph, Genus or another package. |
| Separate auxiliary runtime | `PMM.EXE` at 0000077A includes Doug Huffman's 1991–1994 DOS-extender notice. | A different extender family from the main game's DOS/4GW. FlashTek X-32/DOSX is a plausible identification; the exact binary variant and compiler have not been independently identified. |

The [Digital Mars DOSX developer discussion](https://forum.dlang.org/thread/9gf81j%2420bq%241%40digitaldaemon.com)
identifies Doug Huffman as the DOSX extender author; the
[Digital Mars tool guide](https://www.digitalmars.com/ctg/ctg.html) describes
X32 support for 32-bit DOS. These contextual references do not establish the
compiler used for the local auxiliary executable.

`DOS4GW.EXE` itself also contains Microsoft runtime notices. Those are evidence
about the extender binary, not proof that the game was compiled with Microsoft
Visual C++. Keep component provenance separate.

## Asset tools remain unknown

The game includes PCX images, bitmap/font/binary data, standard MIDI sequences,
WAV effects, OP3 FM banks and an SBK sample-bank reference. These establish
formats and playback inputs, not the applications used to author them. There
is insufficient evidence to name Deluxe Paint, Photoshop, Cakewalk, Cubase,
a debugger, an IDE or an assembler release.

The practical confirmed stack is therefore a Watcom-linked 32-bit DOS program
using DOS/4GW, an identifiable Sound Drive audio layer, and graphics modules
whose SDK provenance still needs investigation.

The [graphics-driver analysis](prince-svga.md) records the shared private ABI,
actual VS module loading/execution, and the separate VGA polling loops.
