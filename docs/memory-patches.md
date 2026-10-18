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
and ensure the guest will not subsequently overwrite the patched region. This
mechanism does not locate code, relocate payloads or reserve guest memory.

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
the generic loader or CPU.

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
