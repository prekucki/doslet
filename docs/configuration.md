# Configure a machine

Open **Info → Settings…** in the desktop or browser workspace. Edit the draft,
select **Validate**, then **Apply & restart** to construct a new session.
Changing hardware restarts the guest. Closing the editor retains the draft;
**Revert** restores the active settings. **Restart** uses the active
configuration. Failed source preparation preserves the old session and its
reset settings.

The display fills the workspace. The toolbar has play/pause and restart icons;
hover over an icon to see its action. **Info** opens diagnostics and provides
access to **Audio…** and **Settings…**. **Pixel perfect / Fit** selects the display
scale, and the adjacent selector switches **Nearest / Sharp Bilinear / HQ2x**
filtering. Settings use
horizontal tabs with a fixed action bar. **Close** or Escape returns to the
display; a successful Apply also closes settings. On narrow windows, additional
tabs scroll horizontally. The restart icon remains available in the toolbar.

The native application prepares folders, images and HTTPS sources on a separate
thread during Apply. The workspace remains responsive while preparation runs.
The previous guest pauses during that work and resumes if preparation fails.
Browser preparation runs through the emulation Worker and host adapters.

## Hardware and input

| Page | Available settings |
| --- | --- |
| Machine | Current i386 interpreter, configurable nominal CPU clock (33 MHz by default), 4/8/16/32 MiB RAM, optional x87 subset, VGA/VBE with 512 KiB VRAM |
| Sound | Optional SB16 at 220h, IRQ 5/7, 8-bit DMA 0/1/3, 16-bit DMA 5/6/7; independent MPU-401 UART at 330h |
| Input | BIOS compatibility or AT keyboard delivery; optional ISA gameport at 201h; mouse capture; controller identity/index, axes, buttons and dead zone |
| Resources | Resolved I/O ownership and sound resources; invalid assignments identify their configuration field |
| Output | Host audio, MIDI renderer/port and game/host MIDI dialects, independently of installed guest sound hardware |

`output.midi_output` accepts `rust`, `apple` (native macOS system GM synth),
`moont` (requires CM-32L ROMs) or a MIDI port name/index. Apple output uses the
system sound bank and mixes with Sound Blaster PCM; it always receives GM.

Only the current CPU, timing and video implementations are selectable. The
interpreter and x87 implement documented subsets, rather than complete historical
processors. See [the specification](SPEC.md) for their implemented instructions.

`machine.cpu_mhz` sets the nominal CPU clock from 1 to 1000 MHz (integer),
defaulting to 33. The Machine page offers common values; TOML accepts the full
range. Apply restarts the machine with the selected clock. Higher values allow
more guest CPU work per emulated second while PIT, video and audio retain their
own rates. This functional timing model is not a cycle-accurate historical CPU.
Legacy `machine.clock_hz` is accepted in whole MHz; if both fields are present,
they must agree. Export writes `cpu_mhz`.

AT keyboard mode uses physical key locations and release events. BIOS mode
retains scan-code/ASCII injection. Text entered in application controls stays
out of the guest. Pause, focus/capture loss and disconnect release guest input;
held controls require a physical release before they can act again.

Native mouse capture follows guest reads: relative motion counters keep relative
input active for one emulated second after the latest read, blocking hover movement
until capture. Without recent relative reads, absolute input works without locking
and an existing lock is released. `input.capture = false` disables capture; leave
it enabled for relative-input games. `input.hide_cursor = true` hides the host
cursor over a running game using absolute input. The cursor remains visible in
application panels, while paused, and while relative input is waiting for capture.
Esc releases an active capture. `hide_cursor` defaults to false. Browser input
retains explicit capture selection: `capture = false` selects absolute movement.

Native controllers use gilrs; browser controllers use the Gamepad API. The Input
page lists available controller identities. A stable ID is optional; leave it
empty to use the explicit controller index. Native UUIDs and browser Gamepad IDs
are host identities and may require rebinding after moving a profile. Native
ambiguous UUIDs require explicit index selection. The gameport exposes two axes
and two buttons. The default mapping uses axes 0/1, buttons 0/1 and a 12% dead
zone. These adapters and electrical gameport behavior have automated coverage;
that coverage does not establish compatibility with a particular physical pad
or DOS game.

## Drives and saves

C: uses a native installation folder or a browser-selected ZIP tree. Browser
**Open… → Choose ZIP…** opens game files and the executable selector.
The Drives page accepts a local ISO or HTTPS ISO as an immutable D: CD-ROM.
Select a browser ISO with **Choose ISO…**. A native local ISO uses the path
field. ISO file access and MSCDEX sectors share one media/cache owner.

| Write policy | Result |
| --- | --- |
| Read only | Guest modifications are denied. |
| Temporary changes | Changes live in a separate RAM overlay until discarded. |
| Persistent changes | Changes use an explicit native save directory or browser IndexedDB namespace. |
| Legacy direct writeback | Native version-1 `rw` behavior: closed changed files replace files in the installation folder. |

Persistent changes preserve the installation as the lower layer. Browser
persistence uses **IndexedDB**; SQL storage is deferred. Each save store has a
base identity and one writer. Busy writers, mismatched installations, quota
failures and corrupt stores produce errors. Saves become acknowledged only after
the host transaction completes. **Info** distinguishes unsaved changes,
pending commits and storage waits. Browser acknowledgement does not prevent
browser storage eviction.

Standard writable mounts reserve a 1-MiB upper layer with 4-KiB changed chunks
and 64 new file records; DOS shares 96 handles across its filesystems. Each ISO
source has a 16-MiB cache of 128-KiB chunks and up to four reads in flight. Native
stores keep a bounded ledger of 4096 acknowledged mutations and the latest two
published snapshots; IndexedDB keeps up to 65536 mutation records. Reaching a
limit produces a visible quota error. Export into a new store to continue after
ledger exhaustion. Store payloads contain only the upper layer and its metadata.

Browser persistent storage is requested from the Window after applying a
persistent mount. The UI reports whether the browser granted durable storage;
denial retains the IndexedDB store with best-effort retention. It does not change
the selected policy to a temporary RAM store.

Use **Export saves** to keep a portable snapshot independently of automatic
persistence. **Import saves** validates that the snapshot belongs to the mounted
installation. Native save exchange uses the file path shown on the Drives page;
browser exchange uses a download/file picker. Pending commits must settle before
restart. To replace a guest with unacknowledged temporary changes, export them or
explicitly select **Discard unsaved game changes on restart** when shown.

HTTPS streaming requires an HTTPS URL, single-range `206` responses, a valid
`Content-Range`, a known length and a strong ETag. Every read pins that identity
with `If-Match`. A full `200` response, a changed validator, malformed range or
encoded representation is rejected. Browser servers also need CORS permission
for `Range` and `If-Match` and must expose `Content-Range`, `ETag` and
`Content-Encoding`. Local ISO selection remains available when a server cannot
provide this contract. Transient transport attempts are finite: at most three,
with 250-ms/1-s delays, a 10-s limit per attempt and 35-s overall limit.

## Profiles

The Profile page imports version-1 or version-2 TOML into the draft and exports
version 2. Native **Import file…** resolves native paths relative to the profile.
Browser import retains named host bindings and path text for editing; selecting
local resources establishes the browser bindings. Profiles describe settings;
they do not embed game files, ISO contents, ROMs or saves.

This version-2 example uses a native installation and separate persistent store:

```toml
version = 2
name = "My DOS game"

[machine]
platform = "pc-at"
cpu = "i386"
engine = "interpreter"
timing = "functional"
cpu_mhz = 33
memory_mib = 16
x87 = true
video = "vga-vbe"
vram_kib = 512

[sound]
sb16 = true
mpu401 = true
irq = 7
dma8 = 1
dma16 = 5

[input]
keyboard = "bios"
gameport = false
capture = true
hide_cursor = false
pad_index = 0
deadzone_percent = 12
axis_x = 0
axis_y = 1
button_1 = 0
button_2 = 1

[output]
audio = true
game_midi = "gm"
host_midi = "gm"
midi_output = "rust"

[game]
executable = "installation/GAME.EXE"
arguments = []
working_directory = 'C:\'

[[drives]]
letter = "C"
kind = "directory"
source = "game-files"
policy = "persistent"
store = "game-saves"

[[bindings]]
id = "game-files"
kind = "directory"
location = "installation"

[[bindings]]
id = "game-saves"
kind = "native-store"
location = "saves"
```

Set the executable and bindings to your own files, then launch with:

```sh
cargo run -p doslet --release --locked -- --config machine.toml
```

For a browser C: tree, use drive kind `zip` and a ZIP binding. For browser saves,
use binding kind `indexeddb` and a namespace in `location`. Local CD-ROM mounts
use kind `iso`; remote ones use `https-iso`. Both require policy `read-only` and
a letter from D through Z. Source binding kinds match their drive kinds.

Version-1 native defaults and precedence remain: built-in defaults, profile,
then explicit CLI flags. `--drive-mode ro|overlay|rw` retains its original
meaning. `--memory-mib`, `--no-x87`, `--no-sb16` and `--no-mpu401` override the
corresponding hardware settings. CLI paths stay relative to the shell; paths
loaded from TOML stay relative to that file. Absent MIDI CLI modes preserve
profile modes. See [version-1 launch profiles](launch-profiles.md) for legacy
fields and command-tail limits.

Hardware, configuration and storage tests are separate from game acceptance.
Compilation or a synthetic save test does not establish a playable PRINCE
session, physical controller behavior, audible output or durable browser storage
on a particular browser/device.
