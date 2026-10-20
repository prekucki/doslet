# Load memory patches

Native launch profiles (versions 1 and 2) can load an ordered list of patch
manifests. Paths are relative to the profile:

```toml
[game]
executable = "HoMnM/HEROES.EXE"
patches = ["../patches/heroes-absolute-town/patch.toml"]
```

The loader checks each manifest against the SHA-256 of the executable actually
selected, including a `--game` override. Missing files, malformed manifests,
incorrect hashes, overlapping writes and invalid RAM ranges prevent the new
session from starting. No executable or installation file is patched on disk.
The browser loader currently rejects profiles with local patch files.

## Manifest format

Each file describes one atomic RAM edit. This illustrative manifest changes two
bytes after a guest loader has installed a matching signature. Replace the hash,
addresses and bytes with values verified for your executable:

```toml
version = 1
name = "Example RAM edit"
executable_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
timeout_steps = 10000000

[[guards]]
address = 0x120000
expected = "55 89 e5"

[[writes]]
address = 0x120100
expected = "74 05"
replacement = "90 90"
```

Addresses are physical RAM addresses, not file offsets or segment:offset pairs.
Hex strings accept whitespace and either case. Each write requires equal lengths
for the expected and replacement bytes. For a larger payload, use `file` instead
of `replacement`; its path is relative to the manifest. `expected_zeroes = 1536`
can replace a long zero-filled `expected` string. Each pair of alternatives is
mutually exclusive. Unknown fields are errors.

Limits are 32 manifests, 128 write/guard spans per manifest, 64 KiB of combined
expected bytes per manifest, and 512 KiB per manifest file. Names contain 1–128
bytes. Payload files are limited to 64 KiB. Writes must fit ordinary RAM, exclude
video MMIO and not overlap another write in the plan.

## When edits take effect

The runtime checks pending patches before and after bounded execution batches,
at most 4096 guest steps apart. It waits until **all** guard bytes and original
write bytes match, then writes the complete patch without executing the guest
between writes. It waits if the current PC is inside a replacement sequence
(execution at its first byte is allowed). High-memory patches wait for A20 to
be enabled; they never edit an aliased low-memory address.

This permits a DOS extender to load the actual game code after the initial MZ
stub starts. Patch authors must select signatures that identify the loaded code
and ensure the guest will not subsequently overwrite the patched region. The
runtime does not locate code or reserve guest memory; the desktop relocates
manifests that name their LE objects, as below.

### Relocatable patches

A DOS extender loads protected-mode code at other linear addresses with a DPMI
host or another memory configuration: Heroes' code object starts at 0x19e000
with `--experimental-dpmi off` and at 0x1e8000 with `reflect` or `direct`. A
patch written for one layout never matches the other, so its guards would wait
until `timeout_steps` faults the session. List the bases of the program's LE
objects the addresses were taken at, object 1 first, and mark the 32-bit fields
of each replacement that hold addresses:

```toml
objects = [0x19e000, 0x240000]

[[writes]]
address = 0x213fab
expected = "be 0b 00 00 00"
replacement = "e9 50 d8 ed ff"
rel32 = [1]          # displacement of the JMP, measured from the field's end

[[writes]]
address = 0x0f1800
expected_zeroes = 1536
file = "mouse.bin"
rel32 = [24, 62]     # jumps into the game's code
abs32 = [516]        # an address of the game's data
```

When the desktop finds the live layout of the program's objects, it moves each
pending patch: guard and write addresses inside an object move with it, while
addresses outside every object, such as BIOS RAM, stay. An `abs32` field must
hold an address inside an object; a `rel32` field keeps its target, moving it
only if the target lies in an object. Applied patches are left as they are.
Fields that overlap, exceed the replacement or appear without `objects` are
errors. Guard and expected bytes are compared as written, so choose signatures
without absolute addresses. Without `objects`, addresses are fixed, which suits
real-mode programs.

Patches apply once, in list order. `timeout_steps` is a positive absolute guest
step deadline since launch, including time spent waiting for earlier patches.
If the expected state is not found before that deadline, execution faults with
the one-based patch index. The failed patch writes nothing; earlier completed
patches remain applied. Restarting with an empty list starts an unpatched guest.
Removing a manifest from the profile does not undo an already running session.

The runtime invalidates decoded/native code through normal RAM write generations
and clears idle-loop proofs after editing memory. It performs no file access or
allocation during patch checks. Completed plans do not scan memory again.
`Session::applied_patch_count()` reports the completed prefix of
`Session::memory_patches()`.

Restart reloads the configured manifests. Portable state version 3 embeds the
validated plan, its deadline and completed count. Restoring a state retains the
payload's live RAM data and does not reapply completed writes or reread host
patch files. Earlier version-1 and version-2 states remain readable.

## Heroes town mouse patch

The supplied [manifest](../patches/heroes-absolute-town/patch.toml) targets the
specific `HEROES.EXE` hash recorded there. It redirects two mouse poll routines
to an original payload in unused Doslet BIOS RAM at `0xF1800..0xF1E00`.
The manifest requires that entire region to be zero before use.

The payload uses absolute coordinates only while the town screen is active.
The adventure map and other screens retain the original relative polling.
Transitions synchronize the driver cursor and drain relative motion once.
The game-specific addresses and screen selection live in the patch, not in
the generic loader or CPU. They were taken with DOS4GW's own host, so the
manifest lists those object bases and marks the payload's seven jumps and
calls into the game and its five data addresses. With `--experimental-dpmi
direct` DOS4GW loads the code object 0x4a000 higher and the data object
0x4b000 higher. Before relocation existed, the guard never matched there and
the session faulted when `timeout_steps` expired after 500 million steps. Now
the patch applies in the off, reflect and direct modes alike.

The payload was exercised live through GDB for town positions, return to the
map and re-entry into town. This establishes those transitions for this build;
it does not establish every game screen or a full playthrough. See the assembly
[source](../patches/heroes-absolute-town/mouse.S) for the calling convention and
entry points. Reproduce its binary with GNU binutils from the repository root:

```sh
as --32 -o target/heroes-mouse-patch.o patches/heroes-absolute-town/mouse.S
ld -m elf_i386 -Ttext 0xf1800 --oformat binary -o target/heroes-mouse-patch.bin target/heroes-mouse-patch.o
cmp target/heroes-mouse-patch.bin patches/heroes-absolute-town/mouse.bin
```
