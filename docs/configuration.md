# Configure a machine

Native game profiles support `game.patches`, an ordered list of local RAM patch
manifests. See [memory patches](memory-patches.md) for configuration and lifecycle.

Open **Info → Settings…** in the desktop or browser workspace. Edit the draft,
select **Validate**, then **Apply & restart** to construct a new session.
Most hardware changes restart the guest. Unused sound hardware can be applied live
from the Audio panel, as described below. Closing the editor retains the draft;
**Revert** restores the active settings. **Restart** uses the active
configuration. Failed source preparation preserves the old session and its
reset settings.

The display fills the workspace. The toolbar has play/pause and restart icons;
hover over an icon to see its action. **Info** opens diagnostics and provides
access to **Audio…** and **Settings…**. The monitor icon opens **Display**, with
**Pixel perfect / Fit** scaling and **Nearest / Sharp Bilinear / HQ2x** filtering.
Set `video_filter = "sharp-bilinear"` under `[output]` in a native or browser
game profile. Supported values are `nearest`, `sharp-bilinear`, `hq2x`, and the
GPU filters `xbrz` and `ravu`; omitting the field selects the global default
(`nearest` without one). Without a hardware GPU only the simple filters run: a
browser that fell back to Canvas2D, or a native host whose only wgpu adapter is
a software renderer such as llvmpipe (`DOSLET_GPU_FILTERS=0` or `=1` overrides
the native check). HQ2x, xBRZ and RAVU-Zoom are then replaced by
`video_filter_fallback` (`nearest` or `sharp-bilinear`, default
`sharp-bilinear`), and 16:9 smart stretches the picture evenly. Pixel perfect never
filters: it only enlarges whole pixels. The display menu can change the filter
while the game is running.
`frame_limit` under `[output]` caps how many guest pictures per second the native
window shows, evenly spaced (10–240, or 0 for no additional cap). The live
**Frame rate** control also limits presentation; the lower rate wins. It saves
host CPU and GPU time for games that draw fewer frames than the display
refreshes, such as 35 for Rise of the Triad. Guest timing does not depend on it,
and browser builds ignore it.
The CPU indicator shows the target speed and recent activity; hover for its
breakdown or click for diagnostics. The video indicator reports register-derived
guest geometry and depth, such as `320×240×8 VGA`, using the current scanout
rather than the original BIOS mode or a generic Mode X name.
Clicking the audio spectrum opens **Audio**.
Settings use
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
| Sound | Off, AdLib (OPL2), Sound Blaster (8-bit DMA and OPL2), or SB16 (8/16-bit DMA and OPL3); independent MPU-401 UART at 330h |
| Input | BIOS compatibility or AT keyboard delivery; optional ISA gameport at 201h; mouse capture; controller identity/index, axes, buttons and dead zone |
| Resources | Resolved I/O ownership and sound resources; invalid assignments identify their configuration field |
| Output | Host audio, MIDI renderer/port and game/host MIDI dialects, independently of installed guest sound hardware |

Choose a game card with `[sound] card = "none"`, `"adlib"`, `"sb"` or `"sb16"`.
The old `sb16 = true|false` field remains accepted. Named MZ/extended programs
receive `BLASTER` for Sound Blaster or SB16, for example `A220 I7 D1 H5 T6 P330`
for SB16. IRQ and DMA follow the machine configuration; `P330` appears for SB16
only when MPU-401 is enabled. Disabling host audio does not hide guest hardware.

`output.midi_output` accepts `rust`, `apple` (native macOS system GM synth),
`moont` (requires CM-32L ROMs) or a MIDI port name/index. Apple output uses the
system sound bank and mixes with Sound Blaster PCM; it always receives GM.

The **Audio → Hardware** page reports whether the guest has accessed its sound
card and MPU-401. Changes that affect `BLASTER` require **Apply & restart**:
changing the Sound Blaster model or its IRQ/DMA resources, or toggling MPU-401
while SB16 is enabled. A guest may already have read `BLASTER` before
its first audio port access. Other audio changes can use **Apply** when the
affected device has never been accessed, preserving execution, RAM, guest time
and saves. After first access, including a probe, use **Apply & restart**.
AdLib has OPL2 without a DSP; Sound Blaster has OPL2 and 8-bit DMA; SB16 has
OPL3 and 8/16-bit DMA.
This history survives saved states. Older states lack that history and require
a restart for hardware changes.

Native host output opens on the first PCM configuration, OPL programming or MIDI
data. Before then, selecting a MIDI renderer or mixer retains the choice without
opening playback devices. MIDI synthesizers and the ROM-backed MT-32 emulator wait for MIDI
data; PCM/FM playback does not construct a MIDI synth. Automatic ROM discovery
still reads and validates candidate ROM files at startup. Restoring active MIDI
state initializes its renderer during replay. Emulated port controllers and
fixed queues remain prepared so first guest I/O and synthesis do not allocate
inside the machine execution batch. With system mixing, each source opens its
own host stream and allocates its playback buffers only when that source first
publishes samples. Unused sources do not generate placeholder silence. External
MIDI does not open a local MIDI stream; an external-MIDI-only game opens none of
the three system playback streams. Pause/reset and mixer telemetry do not
activate dormant streams. Stream setup errors are reported on first use.

Changing MIDI outputs cancels the previous receiver's queued messages and
silences its voices. The new receiver starts with a GM/MT-32 reset before the
current game's parameter and note replay. Restart clears guest MIDI state,
automatic dialect detection and the internal synth; pause/resume keeps that state.
Native external receivers are initialized even while paused. Browser receivers
without SysEx permission can reset channel controllers, programs and pitch bend,
but cannot reset device-wide SysEx parameters such as GM master volume.

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

`machine.dpmi` selects the DPMI host Doslet offers protected-mode programs:
`direct` (the default) dispatches eligible BIOS/DOS services from protected
mode, `reflect` runs them through the guest's real-mode handlers, and `off`
leaves DOS extenders such as DOS/4GW their own host. The Machine page offers the
same choice; `--dpmi` overrides the profile for one launch, and browser bundles
use the profile's value. See [DPMI host](compatibility/dpmi-host.md).

AT keyboard mode uses physical key locations and release events. BIOS mode
retains scan-code/ASCII injection. Text entered in application controls stays
out of the guest. Pause, focus/capture loss and disconnect release guest input;
held controls require a physical release before they can act again.

The **Mouse & keyboard** icon on the bottom bar opens the input panel. Mouse
capture, cursor visibility and BIOS/AT keyboard delivery change immediately,
without restarting or advancing the program. Keyboard delivery changes release
held keys and clear held BIOS modifiers while retaining lock flags, controller configuration,
PIC masks and guest state. These choices remain active after Reset. Input
settings are unavailable during machine preparation or an attached GDB session.

Choose **Auto**, **Capture**, or **No capture** in **Mouse & keyboard** or under
**Settings → Input → Mouse capture**. This host input setting takes effect
immediately, without restarting the guest. Changing it releases the current
capture and any held mouse buttons.

- **Auto** (default) follows guest reads of relative motion counters: capture is
  requested on a display click while relative input is active. After one guest
  second without relative reads, the lock is released and absolute motion resumes.
- **Capture** requests a lock on a display click and keeps relative motion active
  regardless of the guest's recent reads.
- **No capture** always uses absolute motion and never requests a lock.

Set `input.mouse_mode` to `"auto"`, `"capture"`, or `"no-capture"` in a version-2
profile. Legacy `input.capture = true/false` imports as Auto/No capture; do not
combine the legacy field with `mouse_mode`. Export writes `mouse_mode`.
**Ctrl+Esc** releases native capture; plain Escape reaches the guest. Pause,
focus loss, settings panels and session replacement release capture too. Browser
capture requires a display click to enter DOM Pointer Lock; Escape releases it.
`input.hide_cursor = true` hides the host cursor only over a running game using
absolute input. It defaults to false. The cursor remains visible in application
panels, while paused, and while relative input is waiting for capture.

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
| Persistent changes | Changes use a native redb save store or a browser IndexedDB namespace. |
| Legacy direct writeback | Native version-1 `rw` behavior: closed changed files replace files in the installation folder. |

Persistent changes preserve the installation as the lower layer. Native
persistence uses one [redb](https://www.redb.org/) database, `overlay.redb`, per
save store. A store named without a path component, such as `prince`, lives in
the per-user save directory listed under [host directories](#host-directories).
A `native-store` binding, or a value containing a path separator, names the
store directory explicitly. Browser persistence uses **IndexedDB**; SQL storage
is deferred. Each save store has a
base identity and one writer. Busy writers, mismatched installations, quota
failures and corrupt stores produce errors. Saves become acknowledged only after
the host transaction completes. **Info** distinguishes unsaved changes,
pending commits and storage waits. Browser acknowledgement does not prevent
browser storage eviction.

Standard writable mounts reserve a 1-MiB upper layer with 4-KiB changed chunks
and 64 new file records; DOS shares 96 handles across its filesystems. Each ISO
source has a 16-MiB cache of 128-KiB chunks and up to four reads in flight. Native
stores keep a bounded ledger of 4096 acknowledged mutations and the committed
snapshot, in database values just under 64 KiB. Copy-on-write retains the
previous revision until the next commit is durable, and redb grows the file in
power-of-two steps; expect a file of up to about four times the snapshot size.
IndexedDB keeps up to 65536 mutation records. Reaching a
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

Version-1 native directory stores (`manifest.bin` and `snapshot-*.bin`) are
imported into `overlay.redb` when first opened, including their mutation ledger.
The old files remain unchanged and are no longer updated. A store that is
already open by another Doslet process reports a busy writer.

## Host directories

Native builds keep per-user files in the platform's standard directories. On
Linux and BSD these follow the XDG Base Directory specification; relative
`XDG_*` values are ignored, as the specification requires.

| Contents | Linux / BSD default | macOS | Windows |
| --- | --- | --- | --- |
| Global host settings & custom presets | `$XDG_CONFIG_HOME/doslet/settings.toml` (`~/.config/doslet/settings.toml`) | `~/Library/Application Support/doslet/settings.toml` | `%APPDATA%\doslet\settings.toml` |
| Game launch profiles | `$XDG_CONFIG_HOME/doslet/profiles` (`~/.config/doslet/profiles`) | `~/Library/Application Support/doslet/profiles` | `%APPDATA%\doslet\profiles` |
| Named save stores (`overlay.redb` & `autosave.state.gz`) | `$XDG_DATA_HOME/doslet/saves/<name>` (`~/.local/share/doslet/saves/<name>`) | `~/Library/Application Support/doslet/saves/<name>` | `%LOCALAPPDATA%\doslet\saves\<name>` |
| Save export/import file | `$XDG_DATA_HOME/doslet/exports` | `~/Library/Application Support/doslet/exports` | `%LOCALAPPDATA%\doslet\exports` |
| Speed-tier labels and their **Save as** files | `$XDG_CACHE_HOME/doslet/speed-tiers` (`~/.cache/doslet/speed-tiers`) | `~/Library/Caches/doslet/speed-tiers` | `%LOCALAPPDATA%\doslet\cache\speed-tiers` |
| Dynamic-core profiles, one per executable | `$XDG_CACHE_HOME/doslet/core-profiles` | `~/Library/Caches/doslet/core-profiles` | `%LOCALAPPDATA%\doslet\cache\core-profiles` |
| Generated AOT procedure modules, one per live layout | `$XDG_CACHE_HOME/doslet/aot/<exe-sha256>/<layout>` | `~/Library/Caches/doslet/aot/<exe-sha256>/<layout>` | `%LOCALAPPDATA%\doslet\cache\aot\<exe-sha256>\<layout>` |
| Native code compiled from those modules | `$XDG_CACHE_HOME/doslet/aot-compiled` | `~/Library/Caches/doslet/aot-compiled` | `%LOCALAPPDATA%\doslet\cache\aot-compiled` |

These are defaults; edited paths, profile bindings and explicit CLI paths such
as `--config`, `--speed-tier-cache` and `--core-profile` keep their meaning.
Machine profiles are configuration and saves are data. Everything under the
cache directory can be regenerated, but deleting it also discards learned and
manually corrected speed-tier labels; save those elsewhere to keep them. The
first use moves an existing `$XDG_DATA_HOME/doslet/speed-tiers` directory into
the cache. The browser stores saves in IndexedDB instead.

Native game launches load and save a dynamic-core profile named by the SHA-256
of the executable; `--no-core-profile` disables this and `--core-profile FILE`
selects one file for every game. ZIP launches and the demo use no profile unless
one is given explicitly; they apply only to builds with the optional JIT.
`<layout>` names where the DOS extender loaded the program's LE objects, such
as `le-1e4000-265000-266000-268000-2697eb6b`: DOS4GW loads code at other
addresses with a DPMI host or another memory configuration. The desktop
compiles and installs the module of the live layout while the game runs unless
`--no-aot` is given. Compiled AOT files hold host machine code for this
Wasmtime version, configuration and CPU. Each records a SHA-256 of its contents;
a mismatch, or code Wasmtime rejects, is compiled again. Only one compilation per
module is kept.

## Global settings, hardware presets and game profiles

Global host defaults live in `settings.toml` (configured from **Settings** in the
library sidebar) and cover host audio mixer (`system` or `application`), master
volume, MIDI output port/synthesizer and formats, MT-32 ROM folder, the DPMI host
(`[machine] dpmi`), default SB16 IRQ/DMA (`[sound]`), keyboard, mouse and
gamepad input, scaling filter, frame limit, size (`fit` or `pixel-perfect`),
screen shape (`4:3`, `16:9`, `16:9-smart`) and full screen style (`borderless`
or `native`) under `[display]`, interface language (`auto`, `en`, `pl`), and the
default hardware preset (`386sx-16`, `386dx-33`, `486dx2-66`, `pentium-133`, or
custom `[presets.<id>]` entries).

A game profile overrides these one key at a time: every key it leaves out comes
from the global settings, or for the clock, speed mode, memory, x87 and sound
cards from its `preset` (the global default preset when it names none). Options
given on the command line come before both. The library's **Configure…** window
writes only the keys set to something other than **Default**, and keeps the
profile's comments and layout.

Game profiles under `profiles/` can select a hardware `preset` and declare
additional entrypoints (`[[game.actions]]`, such as `SETUP.EXE` or
`SETSOUND.EXE`) that share the same persistent `overlay.redb` save store on
drive `C:`.

This version-2 example uses a native installation, a hardware preset, a setup
action, and a separate persistent store:

```toml
version = 2
name = "My DOS game"
preset = "486dx2-66"

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
dpmi = "direct"

[sound]
sb16 = true
mpu401 = true
irq = 7
dma8 = 1
dma16 = 5

[input]
keyboard = "bios"
gameport = false
mouse_mode = "auto"
hide_cursor = false
pad_index = 0
deadzone_percent = 12
axis_x = 0
axis_y = 1
button_1 = 0
button_2 = 1

[output]
audio = true
video_filter = "xbrz"
video_filter_fallback = "nearest"
frame_limit = 35
game_midi = "gm"
host_midi = "gm"
midi_output = "rust"

[game]
executable = "installation/GAME.EXE"
arguments = []
working_directory = 'C:\'

[info]
cover = "cover.jpg"
released = 1994
publishers = ["Apogee Software, Ltd."]
developers = ["Apogee Software, Ltd."]
genres = ["Action"]
gameplay = ["Shooter"]
description = """
One or more paragraphs about the game.

A blank line starts a new paragraph."""

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

To queue startup input in both the native app and a web package, add
`startup_keys = "15"` under `[game]`. Characters are written in order to the
BIOS keyboard buffer when the game first waits for a key, and queued again
after a restart. Up to 15 printable ASCII characters are supported; `\n`,
`\t`, `\b` and Escape can also be expressed with TOML string escapes.
For example, `startup_keys = "A?\n"` sends `A`, `?`, then Enter.

The optional `[info]` table describes the game for launchers; emulation ignores
it. `cover` names a JPEG, PNG or WebP image (at most 16 MiB) relative to the
profile. `released` is a year from 1970 to 2100. `publishers`, `developers`,
`genres` and `gameplay` each take one label or up to 16 nonempty labels of at
most 200 characters. `description` is plain text of at most 4,000 characters;
blank lines separate paragraphs. The desktop [game library](usage.md#game-library)
shows all of these on its cards and game pages. `doslet-web` copies the cover
unchanged into the bundle and shows it with these details and the description
on the generated start page, whose first paragraph also becomes the page summary.

The `saves` location is relative to the profile and holds `overlay.redb`. Omit
the `game-saves` binding to keep that store in the per-user save directory
instead.

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
