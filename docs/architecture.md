# Component architecture

Doslet separates architectural CPU behavior, chip behavior, machine wiring and
application policy. Libraries live in nested domains, with independent manifests
and tests. No emulation library depends on iced or accesses a host filesystem.

The [machine design](design/configurable-machines.md) and its
[input and overlay extension](design/input-and-write-overlays.md) record the
design decisions. The initial implementation now includes device registration,
version-2 profiles, the shared machine editor, positional ISO sources, input
routing and transactional save stores. SQL remains a future host adapter.
See [configuration](configuration.md) for the implemented settings and limits.

TOML launch profiles use a shared native/browser `toml_edit` codec.
The frontend merges explicit CLI options, resolves host paths and transfers
mounted file ownership during setup. `Session::configure_launch` accepts DOS
command-tail and working-directory byte slices before execution; the CPU and
chip graph does not depend on TOML. The desktop retains launch settings for Reset.

The Rust `doslet-web` application packages a TOML launch profile and local media
as a static website. Its `serve` subcommand builds before starting a localhost
preview. Optional offline register-code AOT uses the existing CPU WASM compiler;
the browser machine Worker retains one Session owner, validates live code and
device grants, and calls the interpreter for unsupported work. Generated modules
import the runtime's memory but touch only a fixed register/dispatch scratch;
polls copy nothing. The runtime verifies each LE object's live load delta.
An embedded `doslet.procedures` module can instead use the same bounded
memory-procedure compiler and metadata as native AOT. The browser Worker imports
its own linear memory into that module, owns RAM, VRAM and code generations in
stable allocations there, and passes only a small call frame at procedure
boundaries. Native and browser dispatch share source-byte guards, work and IRQ
admission, and interpreter fallback. Procedure bundles carry post-relocation
absolute addresses, so preparation must use the browser's actual loader layout.
Tier-aware register grants use the existing instruction labels and scheduler:
every bound block must have one live tier, transitions end the grant, and the
runtime charges that tier's counters before advancing devices. Other foreign
engines retain their existing refusal of labels unless they explicitly provide
live classification and matching tier accounting. Memory procedures provide
it: a call needs one live tier for all source instructions, or exact per-tier
counts for a canonical copy. The browser always enables it, and native speed
tiers enable it with their labels. The uniform tier is kept per procedure for
the CPU's label generation while the guarded source bytes match.
On the desktop, `apps/common/le_layout.rs` finds where a DOS extender loaded a
running LE program's objects from relocation-free code anchors. Procedure
modules are cached per layout and installed while the game runs; learned
speed-tier labels and relocatable patches move with their objects.

`doslet-storage` defines bounded positional reads, source identities, request
epochs and atomic commit messages. `doslet-iso9660` parses either an owned
immutable image or a cached positional provider. Its DOS file and MSCDEX paths
share one cache. `doslet-vfs` owns the directory tree, changed 4-KiB chunks and
96-handle table. The DOS facade retains the existing mount API. Native directory
and ZIP mounts enter at the filesystem layer; they do not pretend to be sectors.

The native host worker performs file/HTTPS reads and durable save publication.
A browser storage Worker owns IndexedDB transactions and Web Locks; the machine
Worker performs Fetch/Blob reads and owns the cache. Host completions carry an
epoch, generation, request ID and slot. The runtime retains an explicit service
continuation across a miss or commit. It stages reads before guest writes and
copies at most 4 KiB per visit. Guest clocks freeze during storage waits; host
presentation remains responsive. A failed write remains dirty and exportable.

`doslet-config` separates portable machine intent from host bindings. Setup
resolves available components and compiles independent port read/write maps,
page routing, IRQ grants and DMA grants. Concrete endpoint enums preserve static
dispatch in execution. A candidate machine is installed only after validation
and host preparation; Reset uses the active recipe. Hardware options include
4/8/16/32-MiB RAM, x87, SB16, MPU-401, BIOS/AT input and the optional gameport.

`doslet-input` owns bounded contributor-aware key, button and axis routes.
Native gilrs and browser Gamepad adapters normalize host input; disconnection,
focus loss, pause and overflow release affected contributors. The independent
AT keyboard crate implements scan sets 1/2 and guest-time typematic. Its IRQ1
shim updates the BIOS key ring only when executed through the guest IVT.
The gameport implements guest-time charge deadlines at port 201h.

The native `doslet-emulation` thread creates and exclusively owns the Session,
execution cache and storage adapter. With a host audio device, `doslet-audio`
owns MIDI synthesis and PCM resampling/mixing. Emulation processes at most 256 queued
commands between bounded execution turns. Each turn has a 400000-step budget,
10000-step chunks and an 8-ms host-work check between chunks. Once per turn
it polls/submits a bounded JIT job; Cranelift runs on a separate `doslet-jit`
thread. Neither compiler completion nor a full queue blocks emulation. The worker continues without
sleeping when behind the host clock, and waits briefly when caught up or blocked
on audio/storage. Its guest clock remains independent of UI refresh.

The iced state owns presentation snapshots and sends commands through a
256-entry nonblocking queue. Overflow pauses execution and releases held input.
A single snapshot slot replaces obsolete frames while preserving pending host
effects; execution needs no shared machine mutex or guest-memory clone. RGBA
conversion occurs on the worker at most once per 16 ms and only on request.
Coalesced notifications wake iced; acknowledging a running snapshot requests
the next one. A stalled UI stops presentation work without stopping
emulation. Paused sessions request no periodic UI updates. Editor command
sequence numbers prevent older snapshots from overwriting newer local edits.
Shutdown joins the owner after saving its dynamic-core profile.
Pending iced effects are collected in a flat list; empty tasks are discarded.
Snapshot replacement appends those lists and batches them only when the UI
consumes a packet. Paused execution and a stalled UI therefore do not build a
recursive chain of empty task streams for the main thread to poll or drop.

The opt-in native speed-tier adapter installs static code labels into the CPU's
execution caches. Native blocks retain their control-flow scope and return counts
of completed HOT/COLD/GREY/IRQ work. The machine converts those counts to time with
configurable rational weights; device horizons still bound execution. Host clocks
stay in the native adapter, which bounds MAX work and services devices even when
its instruction cost is zero. Existing proven idle/VGA batching accounts the labels
of skipped iterations. Unknown/changed code uses GREY; confirmed IRQ labels have
separate counters at the same clock rate. Legacy graph-based IRQ guesses remain
learnable GREY until checked by a replay. The
older headless replay planner is diagnostic-only. See the
[experiment and live controls](compatibility/speed-tier-replays.md).
Native portable-state loading prepares one owner on a bounded setup stack before
handing it to the emulation worker. Saved states refer to immutable disk assets
by type, DOS drive/path, length and checksum; restoration binds matching assets
from the launch profile. Mutable RAM, devices, overlays and handles have one
owner each. See [portable states](save-states.md).

Optional native tier learning samples actual executed counters once per host
second, excluding skipped iterations. GREY above 10% schedules one independent
state copy for paired one-second guest-time replays at the GREY rate (capped at
333 MHz) and three times that rate. Only the background copy disables idle/VGA
acceleration and traces instructions. Immutable media are shared; guest RAM and
mutable devices/filesystem state are copied explicitly at a batch boundary.
Diagnostic copies retain private file overlays and handle positions but omit
persistent-store leases and host writeback. Guest writes and later reads operate
in that copy's RAM; profiling never publishes those changes. Capture waits for
an in-flight host storage operation to settle, while an idle persistent lease
no longer blocks learning. Portable state-file restrictions are unchanged.
Fresh entry is captured before the first live batch; at most four further
high-GREY snapshots wait for the single worker. The worker yields periodically,
has a 120-second host deadline, and cancels on session replacement or policy disable.
Pausing the guest drains already captured replays and applies their results;
no new snapshots are captured while paused. Progress changes wake presentation
without advancing guest execution. An attached, stopped debugger retains its
existing freeze behavior. Each queued job takes the latest settled instruction labels when it starts.
After its first, lower-throughput replay, it skips the second replay if every
observed instruction has matching code bytes and an established or manual label.
Unknown/GREY sites, changed code and empty traces keep the paired check. This
checks the observed window, not every possible future game path. Results require
the same session epoch. Stored instruction-byte guards validate
labels on decode, so evidence for unloaded startup code can survive until restart. Interrupt frames are tracked only in the diagnostic
copy; new IRQ instructions receive a distinct IRQ label. Learning adds HOT/COLD/IRQ to GREY
sites and never revokes an established colour. Installation invalidates decoded/native labels without resetting clock fractions
or execution counters; instruction blocks remain intact.

Native CLI startup selects persisted tier labels by the executable's SHA-256,
then validates instruction bytes through the existing decode path. The host adapter
owns persistence: one background writer serializes a bounded copy of the label map,
atomically replaces its JSON file, and flushes outstanding changes on orderly
shutdown. It also stores rates and the learning setting. Disk I/O and hashing are
outside the instruction execution path; CPU libraries do not depend on storage.

## Dependency graph

Arrows mean "depends on".

```mermaid
graph TD
  UI[iced application] --> Runtime[doslet-runtime]
  UI --> Synth[doslet-midi-synth]
  UI --> Routing[doslet-midi-routing]
  Worker[browser Worker] --> Routing
  Routing --> Translator[doslet-mt32-gm]
  Routing --> Synth
  UI --> Moont[doslet-mt32]
  UI -. macOS .-> Apple[doslet-apple-midi / DLSMusicDevice]
  CLI[headless application] --> Runtime
  CLI --> Synth
  CLI --> Executable[doslet-executable]
  Runtime --> PC[doslet-pc]
  Runtime --> DOS[doslet-dos]
  Runtime --> ISO[doslet-iso9660]
  Runtime --> VFS[doslet-vfs]
  Runtime --> Input[doslet-input]
  Runtime --> Storage[doslet-storage]
  UI --> Config[doslet-config]
  PC --> Config
  DOS --> VFS
  ISO --> Storage
  Runtime --> VGA[doslet-vga]
  Runtime --> CPU[doslet-x86]
  DOS --> CPU
  DOS --> Bus[doslet-bus]
  DOS --> Executable[doslet-executable]
  PC --> CPU
  PC --> Bus
  PC --> VGA
  PC --> PIC[doslet-i8259]
  PC --> PIT[doslet-i8254]
  PC --> DMA[doslet-i8237]
  PC --> Time[doslet-time]
  PC --> MPU[doslet-mpu401]
  PC --> DSP[doslet-sb-dsp]
  PC --> Mixer[doslet-sb-mixer]
  PC --> OPL[doslet-opl3 / Nuked OPL3]
  PC --> KBC[doslet-i8042]
  KBC --> Keyboard[doslet-at-keyboard]
  PC --> Gameport[doslet-gameport]
  PC --> CMOS[doslet-mc146818]
  CPU --> Bus
  CPU --> X87[doslet-x87]
  CPU -. native dynamic-core feature .-> JIT[doslet-x86-jit / Cranelift]
  Time[doslet-time: independent clock primitives]
```

## Ownership and borrowing

The native `wasm-procedures` feature, enabled by default in the desktop app,
gives PC RAM one concrete
backing selector. Its ordinary variant owns boxed RAM and page generations;
its WASM variant owns one fixed Wasmtime linear memory containing RAM,
generations and physical VRAM. VGA retains its device controls and borrows the
selected backing through explicit memory APIs; its old allocation is released
after setup migration. Bus
reads, device writes, decoder guards, state serialization and generated
procedures borrow slices from the selected owner. Selection uses an enum rather
than a trait object in memory hot paths. Enabling an offline module migrates
existing RAM and generations once during setup and invalidates old owner
proofs; procedure calls exchange only their small CPU frame and descriptor.
Ordinary interpreter batches borrow RAM, generations and physical VRAM once
from this owner, alongside the VGA controller. A concrete slice-backed bus
admits both RAM and VGA operands until the bounded batch ends. VGA byte
operations retain their normal plane/latch/revision semantics and access
counts. The borrow ends before generated code or port I/O runs. This avoids
Wasmtime memory lookups per interpreted access and per-pixel batch exits;
instrumented accesses and boundary-crossing operands retain the full PC bus.
The CPU still depends on generic bus/procedure contracts, while PC wiring
selects the optional native owner. This feature adds no Wasmtime dependency to
ordinary or browser builds. See the native ROTT test in
[the WASM core notes](compatibility/wasm-core.md).

`Session` owns one boxed `PcMachine`, allocated during setup to bound host stack
usage. Its CPU and `PcBus` are separate fields, so the
CPU can execute with `&mut bus` without interior mutability. The bus owns RAM and
devices. VGA owns its aperture and palette. `IndexedFrame<'a>` borrows those
arrays; Rust prevents mutating VGA while that frame remains borrowed.

`PcBus::ram_view` returns a slice only for ranges wholly in ordinary RAM. A range
crossing the VGA aperture is rejected. Word/dword bus operations borrow one RAM
range when its A20 mapping is contiguous and excludes MMIO. Crossings, address
wrap and out-of-range accesses retain ordered byte operations, including partial
writes before a bus error. Loading copies file bytes into guest
memory once; ordinary execution does not make intermediate memory buffers.

The executable inspector borrows the input file. Its load-module and relocation
table slices point into that file's original buffer. The DOS MZ loader consumes
those views and a caller-supplied ordinary-RAM process range, validates the
layout, writes through `MemoryBus` and initializes process registers. It has no
PC device or filesystem dependency. The runtime selects the available PC RAM
range; the host CLI owns file acquisition and output.

Native RAM patch manifests are parsed and checked against the selected executable
hash in the host setup adapter. The runtime owns the bounded byte-edit plan and
checks pending signatures between execution batches, allowing a guest DOS
extender to install code before patching. All spans must match before any write;
ordinary RAM write generations invalidate decode/JIT entries. No host paths,
file access or game-specific addresses enter the CPU or machine components.
Portable states retain the plan and completed prefix. See
[memory patches](memory-patches.md) for bounds and lifecycle.

`ReadOnlyDrive` owns preloaded file buffers transferred by the host during setup.
DOS handles are fixed-size records referring to those buffers; reads borrow the
file slice and write through the bus. The DOS memory arena uses 1024 fixed block
records and publishes MCB metadata into guest RAM. A DOS overlay load borrows
the mounted executable, copies its module once and relocates in place without
creating another machine or process.

The browser Worker platform adapter keeps the selected ZIP as immutable compressed
input and mounts the chosen executable's directory during setup. ZIP decoding
and path validation belong to the application, not DOS or chip crates. A new
session owns decompressed file buffers; only the compressed source is shared
between pending selection and the running game's reset source. Reset reimports
the original ZIP and creates a fresh RAM overlay, rather than cloning RAM or
mutated file contents. The local file picker uses browser APIs through Rust
WASM bindings and does not upload input.

The drive owns a fixed current-directory name; relative path resolution uses
stack storage. Changing directories or opening a relative path does not call
the host filesystem or allocate during execution.

The MPU-401 crate owns UART state and a fixed output queue independently of
the CPU, machine and audio host. PC ports 330h/331h route to it. The runtime can
drain MIDI into caller-provided storage. Each accepted data byte holds status
bit 6 busy for 320 µs of guest time (31,250 bit/s, including start and stop bits).
The PC supplies its clock on port accesses; host draining cannot shorten that
deadline. A full output queue also asserts busy. Writes while busy fail explicitly.
The deadline survives reset and save/restore along with accepted bytes; older
states without the deadline restore ready. Accepted bytes retain frontend batch
resolution rather than separate timestamps at the end of each serial byte.
The independent no_std MIDI synthesizer
receives those wire bytes and renders into supplied stereo buffers. Its timeline
maps guest nanoseconds to sample frames; MIDI timestamps currently have bounded
batch resolution. Native CPAL owns a reserved host audio queue and shares only
those frames with the output callback. The machine remains owned by the
emulation thread.

Native sample output uses eight preallocated, recycled packets between emulation
and `doslet-audio`. Each holds up to 4096 timed PCM frames and 16384 translated
MIDI bytes. Emulation reserves a packet before advancing a CPU chunk; a full pool
or at least 80 ms of unrendered guest time defers the next chunk. That time bound
can be crossed by the final accepted chunk. Packets preserve the existing order:
receive timed PCM, render to the committed guest timestamp, then apply that
batch's MIDI. Synthesis never advances beyond committed guest time. Audio refill
pacing subtracts pending synthesis time from its target to avoid counting it twice.
The worker renders at most 512 frames before checking control requests. A full
CPAL queue makes it wait, without holding a machine or synthesis lock.

Rust, Apple DLS and CM-32L synthesizers are created, restored, used and destroyed
on the audio owner. Configuration uses a separate control channel and acknowledges
only after successful construction/replay, cancellation of older packets and
clearing the old CPAL frames. Failed preparation preserves the previous output.
Pause gates playback while retaining queued sound and synth state; reset and
shutdown can interrupt a full paused output queue. Worker failures pause the PC
and reach the existing notice/stderr path. The device-free path uses the same
renderer synchronously. OPL/DSP register behavior, DMA, IRQs and source PCM
generation remain with the machine. Thread priorities and the CPAL frame-queue
mutex are unchanged; synthesis occurs outside that mutex and outside the callback.

Linux can select `--audio-backend pulse` while CPAL remains the default. The host
`doslet-pulse` crate uses the Rust `pulseaudio` protocol client, with no libpulse
or libpipewire bindings. The existing synthesis owner and reserved frame queue
feed one 48-kHz stereo f32 stream. A `doslet-pulse` control thread handles stream
creation, corking, reset acknowledgement and bounded recovery; the library's
reactor consumes queued PCM. No machine state crosses into either thread.
Reset increments a queue generation, rejects callbacks from older generations,
and deletes/recreates the server stream before acknowledging the clear. This
also discards bytes already queued in the client reactor. Pause retains samples.
Control requests have a two-second deadline; reset acknowledgement has a
three-second deadline. A failed reset stops the worker and reports the error.

The server buffer requests a 30-ms target and 100-ms maximum; these are separate
from the local queue and do not guarantee end-to-end device latency. Spectrum
and consumed-frame counters measure submission, ahead of audible playback.
Underrun telemetry samples server intervals every 100 ms and may miss shorter
gaps. Recovery allows three attempts per output lifetime, including connections
that succeed briefly. `auto` can then pause and replace the output with CPAL.
Linux and macOS `--audio-mixer system` selects three independent outputs (Rust PulseAudio protocol streams on Linux; CoreAudio `DefaultOutput` AudioUnits on macOS): Sound Blaster
PCM at the DSP rate, FM at 48 kHz, and locally rendered MIDI at 48 kHz.
Application mixing remains the default. Optional bounded host-only source taps
preserve timestamped PCM and FM before their legacy mix; these queues are not
serialized. The legacy mixed path remains available. The synthesis worker
copies PCM samples without interpolation, converts i16 to stereo float, and
fills guest-time gaps with silence. FM and MIDI keep existing engine conversions.
A rate boundary ends a playback segment; the control thread drains and recreates
the server stream. Pause and reset can interrupt this process. Recreation can
introduce a gap. Queue capacity and refill pacing account for all three outputs.
Pause, reset and error checks cover all outputs. Each has a finite reconnection
budget; exhausted recovery pauses the group, with application/CPAL fallback only
under `auto`. No per-sample synchronization is required. This remains experimental
without a long-run drift or audible-quality guarantee.
UI changes prepare the replacement before dropping the old output and leave the
session paused. Spectrum bands report the maximum level across sources;
consumed frames track MIDI, while underrun sample counts are summed across rates.

The native desktop and headless applications share the host `doslet-debugger`
crate and its `gdbstub` TCP adapter. The desktop polls its nonblocking protocol
state machine on the existing emulation owner, so partial packets and stopped
guests do not block UI commands or presentation. Host storage completions remain
queued while GDB is stopped and are applied on resume. Its generic
`tick_mode<M: Execution>` selects ordinary batches or instruction-level GDB
execution outside the CPU loop. The owned desktop target and borrowed headless
target share one generic implementation, without cloning or locking a machine.
Transport-independent `Session::debug_*`
methods remain no_std: RAM access validates complete ordinary-memory spans and
uses the existing page-generation/write-log path; stops do not release input or
advance guest clocks. The adapter exposes a bus-linear PC alongside monitor
output for architectural EIP and hidden segment state. It checks virtual address
breakpoints before each bounded interpreter step and disables JIT, idle proofs
and VGA polling batching during the connection. Desktop detach restores the
previous execution settings and resets pacing; headless retains its diagnostic
settings. Its fixed hotspot table is allocated only when sampling is enabled;
normal execution does not allocate debugger storage. Host TCP,
protocol formatting and paused diagnostic output stay outside emulation crates.
See [the GDB workflow](debugging.md) for mixed-mode disassembly and limitations.

The output callback and browser AudioWorklet feed the final stereo PCM into a
fixed-buffer 1024-point FFT in `doslet-pcm`. Sixteen frequency levels with release
decay reach the status bar as small snapshots. Analysis allocates no memory after
setup, includes underrun silence and does not affect guest hardware timing.
In the browser, `doslet-web-worker` exclusively owns the machine and storage.
The window's iced state receives diagnostics and RGBA presentation buffers; it
cannot advance or borrow the machine. Two transferable video buffers are reused;
conversion writes into Worker-owned host storage and iced explicitly copies into
its own image. The Worker allocates no guest buffers during ordinary execution.

`doslet-web-audio` contains only the synth, fixed timed MIDI queue and fixed
scratch buffers. AudioWorklet instantiates a precompiled WASM module at setup.
Its process callback calls Rust and writes from fixed channel views without
allocating sample buffers, growing memory or waiting for another thread. Queue
timestamps and committed-time watermarks prevent playback ahead of emulation;
underflow inserts host silence while retaining guest time. Telemetry is handled
outside process() on a direct Worker port. Sixteen transferable MIDI buffers
bound in-flight traffic; acknowledgements recycle buffers independently of the
window. Fresh connection pools and epochs isolate output replacements. No
SharedArrayBuffer or cross-origin isolation headers are required.

User gestures create/resume audio; context suspension does not stall the guest.
Pause preserves synth state and Reset clears it. The browser glue registers
the processor and connects APIs; synthesis remains Rust. A small TextDecoder
fallback accommodates wasm-bindgen error strings in the worklet global scope.
The Trunk Rust post-build hook builds both extra WASM modules and bindings,
including wasm-opt -O3 in release builds. There is no JS build system.

Native macOS can select `apple`, backed by `doslet-apple-midi`. This host adapter
owns one DLSMusicDevice Audio Unit and renders at most 512 stereo frames per call.
It uses the system GM bank, receives the existing GM/MT-32 routing output and
shares the guest-timed PCM mixer and CPAL queue with the Rust synth. Fixed planar
scratch buffers are interleaved into caller storage; no machine state is shared.
Apple API allocation behavior is outside the emulator's allocation guarantee.
Snapshot replay services the Audio Unit between messages and discards setup PCM
without advancing guest time. Pause preserves the unit and sends silence to the
audio device; Reset replaces the unit. The private FFI module owns creation,
configuration, render and disposal;
the crate denies unsafe code elsewhere. No Audio Unit enters the portable core
or WASM dependency graph.

Host MIDI is an explicit alternate adapter: midir natively, Web MIDI in browsers.
The no_std wire decoder reconstructs complete messages and keeps basic replay
state outside sample synthesis. Native port output bypasses sample generation.
A dedicated native thread owns the midir connection. A preallocated bounded
queue carries complete messages and guest timestamps; a condition variable wakes
the sender at the next deadline or a pause/reset/stop request. The emulation thread executes
up to 80 ms ahead and retains one mutable Session owner. The queue mutex never
covers host IO. Short message storage is inline; bounded SysEx payload ownership
is transferred explicitly. Cancellation removes pending messages and requests
silence before replay, with epochs preventing stale completion statistics.
Output failures latch once and cancel future notes. The inspector reports
dispatch lateness, which does not measure downstream PCM underflow.
Browser host ports receive the same timestamped Worker stream. clear()-capable
ports can schedule through Web MIDI; other ports use a bounded cancelable host
timer queue. Neither adapter assumes an OS synthesizer exists. Audio errors
pause/report once and an output replacement can recover the existing Session.
Host API allocations and presentation copies remain explicit; the machine is
not shared with either audio API. The procedural synth is not
sample-based GM or MT-32 emulation. Optional native `doslet-mt32` uses the
`no_std` moont CM-32L engine with externally supplied, fingerprint-validated
Control v1.02 and PCM ROMs. Its timeline renders bounded blocks and explicitly
resamples 32-kHz output to the host rate. ROM loading and SysEx ownership occur
outside the audio callback. Browser ROM selection remains pending.
Output-switch snapshots use a separate setup path that services moont between
complete MIDI messages and discards the warm-up PCM. This avoids submitting a
snapshot larger than the engine's 4096-event live queue in one burst. The guest
clock and output-frame position do not advance during restoration. Native
synth/dialect switches reuse an already open CPAL stream and clear its old PCM
only after the replacement synth is ready.

The independent `no_std` `doslet-mt32-gm` crate translates the MT-32 MIDI
dialect to GM channel messages without ROM data. It owns a fixed ~23-KiB model
of the SysEx-addressed parameter memory (patch/timbre/rhythm temp, patch and
timbre memory, system area, display) and per-part held-note mappings. Built-in
presets map by number through Doslet's own table; guest-uploaded timbres are
classified by name, then by envelope, waveform and register. Parts 1–8 play on
GM channels 1–8 and rhythm on 10. Translation borrows each framed message,
emits through a caller closure and allocates nothing. It is an approximation,
not LA synthesis. Native and browser applications use `doslet-midi-routing` to
frame messages, maintain original and translated channel state, and select
translation according to the game/output dialects. Roland model-16h SysEx
upgrades GM game mode to MT-32 before a GM output receives it. Other unsupported
SysEx retains the output's explicit error behavior. The procedural synth honors
RPN 0; replay restores its semitone/cents range and current RPN selector.
An MT-32 device reset also sends GM All Sound Off and Reset All Controllers on
the translated channels, clearing sustained voices and restoring expression
before the fresh instrument setup. PCM regression tests cover a muted channel
and a note released under sustain; the native Apple adapter has a separate
expression-reset rendering check.
Written-byte masks in the MT-32 model track DT1 and program/controller side
effects. Raw replay sends a device reset followed by bounded DT1 packets only
for known written ranges, then pitch, expression, sustain and active notes.
It does not copy factory timbre bytes from ROM. Snapshots fit a separate 256-KiB
transport buffer (including fragmented written ranges) and preserve the router's partial wire message across switches.
Native moont uses its GmDevice wrapper in GM mode and its Device in MT-32 mode.
GmDevice approximates GM with CM-32L instruments and eight melodic parts; it is
not a full GM sound bank. Output switches restart voices rather than preserve
voice envelopes and reverb tails.

The session owns the selected DOS mouse-driver state. Host adapters supply
logical pointer coordinates, relative movement and button transitions. The
native iced backend is locally patched to expose winit cursor capture and a
bounded relative-motion task stream (see [vendor notes](../vendor/README.md)).
The browser uses DOM Pointer Lock and Worker messages. The session retains
fractional movement and keeps raw motion counters independent of clamped
logical coordinates. Guest callbacks execute as
far calls through an interrupt wrapper that preserves registers and FLAGS;
protected-mode mouse callbacks remain unsupported. Pointer events do not mutate
VRAM: presentation borrows cursor masks and applies AND/XOR while converting
the indexed frame to RGBA. Text presentation also borrows guest memory and uses
explicit BIOS glyph tables. No guest RAM or VRAM clone is required.

The SB16 DSP and CT1745 mixer have independent `no_std` crates and fixed state.
PC ports 220h-based DSP/mixer accesses route to them. The DSP owns command
parameters and two response bytes; the mixer owns its configuration registers.
Mixer register 83h retains interrupt enables: byte/word DMA bits gate delivery
without clearing DSP pending status, and reenabling a pending source raises its
IRQ. ROTT's Wave Blaster mask 0Bh leaves both DMA sources enabled; the independent
MPU-401 UART does not currently generate IRQs. Older save states without the
enable-register marker retain their previous unmasked DMA behavior. Unsupported
mixer accesses report the selected register as well as the data port.
The DSP consumes explicit byte/word DMA units and returns stereo PCM/block IRQ
events. A preallocated machine queue owns produced frames until the host drains
them. Full hardware timing and applying digital-PCM mixer gain remain pending.
DSP diagnostics expose the last configured DMA format until reset, even after
a transfer ends. Both frontends use this with the guest sample rate in the
spectrum tooltip; browser metadata travels in the Worker state snapshot.
The independent `doslet-opl3` adapter owns a Nuked OPL3 chip, two register
latches and integer guest-time timers. With SB16 enabled, the machine routes
388h–38Bh, 220h–223h and 228h–229h to this one chip. Timer-only probes do not
start sample production. Synthesis writes start a guest-clock 48-kHz FM stream;
the chip's resampler converts its native output. The machine sums FM and held
DMA samples into a single timestamp-ordered PCM queue, with saturating addition.
The CT1745 master/FM levels apply approximate 2-dB attenuation steps to FM;
master, DAC, FM and CD faders reset to unity. The PC applies the same 1.5x
OPL output calibration as DOSBox Staging after guest attenuation, with saturation
at the PCM boundary.
Mixer gain for digital PCM remains pending. Native and browser output consume the
existing PCM transport. Executed CPU accesses to these OPL ports charge an
additional 1 microsecond per byte transaction on the shared device timeline,
a functional approximation of ISA access time independent of CPU MHz and
execution tiers. This preserves timer-based detection loops at high CPU rates;
observed and ordinary execution use the same timing. Direct bus setup accesses
do not consume CPU time, and external-clock experiments supply their own time.
Register writes are immediate; chip busy/write-delay timing and OPL interrupt
delivery remain unimplemented.

The published `nuked-opl3` dependency requires `std`. This is an explicit
exception for the FM integration, including the transitive PC/runtime graph;
the remaining emulation libraries retain their `no_std` source boundaries.
State still has one mutable owner and synthesis allocates nothing after setup.
See the [`no_std` backlog item](backlog.md).

The local `nuked-opl3` 0.1.0 patch adds optional Protobuf state conversion for
the complete synthesis core, without changing synthesis. This captures active
oscillators, envelopes and the resampler rather than replaying registers.
Component `save-state` features depend on `doslet-state`; they allocate only
during explicit capture/restore and add no execution hooks. See
[portable guest states](save-states.md) and the [vendor notes](../vendor/README.md).

The absent AWE/EMU8000 probe banks are explicit machine wiring, rather than an
emulated synth. Neither DSP nor mixer depends on the CPU or host audio.
The DSP counts service windows between consecutive auto-init DMA blocks and
records a miss if the previous block's matching IRQ has not been acknowledged
when the next block completes. Transfer replacement/reset cancels the open
window; completed counters survive DSP reset. This fixed-size chip diagnostic
has no host clock or CPU dependency. Runtime adaptation observes new misses
between bounded batches and restores its user ceiling; MCP and native Details
report this limited IRQ-service scope separately from host PCM underruns.

## Chip-to-machine boundary

CPU segment translation leaves the A20 signal to machine wiring. `PcBus` owns
the gate and applies it before RAM/MMIO routing. Fast port 92h, keyboard-controller
output-port events and BIOS services update that signal; the i8042 receives
the resulting line value through an explicit setter. The default machine owns
16 MiB of RAM. Its CMOS memory-size configuration and BIOS query values match
that storage. Device queues and core execution remain fixed/preallocated;
CMOS clock ticking and complete device-event scheduling are still pending.

The CPU also owns a fixed 4096-entry decoded-instruction cache for selected
16-bit and 32-bit forms. The cache includes word overrides, byte/word/dword
shifts and rotates, register/memory INC/DEC, SHLD/SHRD and indirect near JMP. `MemoryBus` optionally supplies side-effect-free code views
and `CodeCheck` values, resolved once when an instruction is decoded. A check
records the RAM owner and mapping generation as one code-space token, plus the
physical 4-KiB pages touched by the instruction and their write generations; a
hit compares that token and both generations without translating the address
again. Every RAM write and borrowed mutable view advances the relevant
generations, so DOS services and wide CPU writes share the same invalidation
path. The PC retains 32 KiB of page generations beside its 16-MiB RAM.
Setup-only atomic owner identities distinguish replacement buses; they contain
no guest state. Checked counters never wrap; exhaustion falls back to
interpretation. A20 changes advance the mapping generation in the token.

Cache entries contain operations, immediates and addressing expressions rather
than resolved data addresses. Register-dependent addresses and segment checks
remain execution work. Entries are keyed by linear address and CS default
width, so they survive compatible CS context changes. A change resolves the
selector, hidden-cache, PE-mode and execute checks once, and hits validate the
entire code span's offset range and its generation token. Decoder rejections are
cached against the same check so the original interpreter runs without repeated
decode attempts. Each `Registers` value carries a setup-time
lineage and a CS reload version that are not guest state; a step whose lineage,
version, selector and PE mode match the last resolution skips the full context
comparison, and exhausted counters restore it. Short/noncontiguous code views and unsupported
forms use the original byte-fetching interpreter, preserving partial fault IP
progress. One cached instruction still consumes exactly one bounded step; a
cached REP string performs one iteration per step, and IRQ delivery retains its
existing path. The optional native core below uses a separate block cache.

`Cpu::interpret_batch` runs cached instructions without the native core between
the machine's IRQ check and its next hardware deadline, at most 256 steps, so
the PC advances clocks and counters once per slice instead of once per
instruction. Segment windows are resolved once per slice. Each instruction
validates all of its batch-memory operands, stack slots and branch targets before
its first effect and then accesses those linear addresses directly; a failed
check ends the slice before that instruction. Batch memory is plain RAM or a span
entirely inside the VGA aperture under a bank that lies in VRAM, which keeps
banked frame-buffer copies such as REP MOVSD in the slice.

Slices fetch from a separate fixed cache of 4096 basic blocks, each holding up
to 8 decoded instructions through the first control transfer and stopping
before port I/O or an unsupported form. A block is validated once per entry
against the CS window over its whole span and one code check; after each
instruction the slice continues only while IP reaches the next decoded
instruction and the check still holds, so taken branches, repeated strings and
stores into the block's pages return to lookup. A block that cannot start is
remembered as a rejection. The native core's slices use the same blocks and try
a native block before each candidate instruction.

Block entries use a 16-byte prepared instruction: a dense handler code, byte
length, native eligibility bit, register operands, immediate and one packed
16/32-bit effective-address expression. Decode selects const-generic handlers
for MOV and ALU/CMP/TEST across register/register, register/immediate,
register/memory, memory/register and memory/immediate forms; register/memory
INC/DEC; LEA; register PUSH/POP; relative near CALL/JMP/Jcc, RET/RET imm16;
byte/word/dword shifts/rotates with immediate/CL counts; and the decoded
MOVZX/MOVSX forms. Unencodable forms are not added to the decoder.
Widths and effective-address sizes are compile-time handler parameters. The
handler code indexes a compiler-owned constant function table monomorphized
for the concrete bus, without trait-object bus calls. Native eligibility is
computed by the existing predicate during preparation; native decoders still
reject word operands and non-32-bit branches.

Rare forms retain `try_batched` and decoded `Instruction` in a separate fixed
cold array, indexed by block slot and position. Only those steps copy the full
IR. Both arrays are allocated at CPU setup. On x86-64, a hot block occupies
168 bytes instead of 328; the separate cold array costs 1152 KiB, increasing
total block storage by 512 KiB while reducing the frequently accessed table
from 1312 to 672 KiB. No execution-time allocation or guest-buffer copy is
introduced. ALU, increment, shift and sign/zero-extension calculations are
shared with `execute_cached`; ordinary bus reads/writes retain page generations
and idle-proof tracking. See [the Linux measurement report](compatibility/interpreter-handlers.md)
for validation, workload boundaries and host-specific results.

The CPU owns hidden segment caches, descriptor-table state and optional x87
state. Its decoder selects 16/32-bit addressing and validates cache permissions
before emitting linear bus addresses. The independent `doslet-x87` crate owns
eight raw extended register encodings, retained 64-bit finite significands,
physical tags, TOP, control/status and instruction/data pointers. It depends on
neither a PC device nor the frontend. The 80387 state is enabled by the PC model.

`doslet-x86::x87_decode` classifies all D8–DF/ModR/M pairs without accessing
guest state or memory. Every documented 387 encoding selects an interpreter
handler; P6/SSE3 and reserved encodings remain separate diagnostics. The adapter
supplies addressing/mode/operand size and performs bus transfers. All numerical
operations and environment encoding remain in `doslet-x87`.

The FPU uses fixed-width integer arithmetic across the extended exponent range.
Raw real80 transfers and context images preserve guest encodings; binary64 is a
diagnostic/output view. Shared arithmetic rules handle memory/register specials.
Root, remainder, logarithm, exponential and trigonometric kernels have bounded
loops and fixed local arrays, without a host math dependency or execution
allocation. FPREM partial reductions return C2 to the guest; trig range failures
also preserve the guest operand and set C2. FNSTENV/FNSAVE side effects occur
after the CPU adapter completes memory writes.
The [coverage map](compatibility/x87.md) separates opcode support from numerical
conformance. The [audit](compatibility/x87-audit.md) records reference fixtures
and the remaining stack/exception-delivery and nonfinite-transcendental limits.

VGA owns configuration register banks and the color/mono CRTC decode. The PC
routes their ports; VGA tests cover readback and write protection independently
of CPU execution. Raster/planar-memory effects remain future work.
Its preallocated 512-KiB VRAM supports a 64-KiB banked aperture and borrowed
640×480 indexed scanout. The runtime's VBE BIOS adapter advertises the supported
mode/window and installs a guest far-call bank switch shim. Mode changes and
bank selection retain one VRAM owner; conversion writes into caller storage.

The PIC and PIT have no bus or CPU dependency. Their methods operate on explicit
port values and clock ticks. The PC adapter chooses their I/O addresses and
connects PIT output periods to the PIC request latch. The machine delivers
eligible interrupts at CPU boundaries with the CPU's STI/SS inhibition state.
Its functional timeline advances four configured nominal CPU clocks per bounded
step (33 MHz by default, configurable from 1 to 1000 MHz). Setup precomputes
whole nanoseconds and the fractional remainder; single steps use addition and
carry without division. Batches preserve the same fraction. PIT, selected raster status and DSP
rates retain fractional clock conversion.
The runtime handles cooperative INT 2Fh/1680h yields by returning AL=00h and
ending the current bounded batch after the BIOS shim returns. It exposes the
yield to native and Worker schedulers so they return to their event loops
before executing another batch. The CPU and hardware clock have no host sleep
dependency; a yield does not fast-forward guest time or pause the session.
Interrupt-enabled HLT can combine those same logical steps until the first
PIT/DSP deadline, the remaining budget, or 1 ms of guest time. IRQ eligibility
still takes precedence. VGA accumulates bounded guest nanoseconds and resolves
raster phase when status is read or at its 1-ms flush bound. The PC composition
recognizes confirmed unprefixed `IN AL,DX; AND AL,08h; JZ/JNZ -5` and
`IN AL,DX; TEST AL,AH; JZ/JNZ -5` status waits, with AH=08h for TEST.
The TEST path executes its final iteration normally so unmasked AL bits retain
their exact raster-time value. A CPU-independent bus notification on actual VGA status I/O gates discovery,
so unrelated CPU transfers require no code inspection. VGA exposes an exact
next-retrace deadline with retained pixel fractions;
PC combines it with PIT/DSP deadlines and skips whole three-unit iterations
strictly before an event. Code/segment/port/state checks protect the fast path;
the CPU library has no VGA dependency. IF-disabled waits preserve pending PIC
requests. TF, interrupt shadows, paging, changed code and one-step execution fall
back. A runtime switch disables recognition for reference measurements. Optional
profiling counters are absent from ordinary builds.
An additional opt-in PC path accepts three caller-selected CMP-dword/JNE sites
and a DS-relative operand. The native Heroes experiment supplies its known
addresses outside the chip/CPU libraries. Exact code, mapped ordinary RAM,
positive data, invariant flags and IRQ/debug state are checked per batch.
Whole two-step iterations advance strictly before PIT/DSP events with the same
1-ms bound. The path remains disabled by default and does not patch guest code.
No chip reads a host clock or depends on UI refresh. Precise CPU/bus timing and
complete arbitration/cascading remain future work.

The separate, opt-in `--experimental-heroes-menu-idle` policy changes polling
cadence rather than proving instruction equivalence. Native setup supplies the
menu and event-list addresses; runtime recognition checks flat segments and
CALL/backedge bytes. Menu checks require an empty stack event, the known callback
and a positive animation countdown. Event-list checks require an empty stack
event, the selected context, CL=0 and a finished handler iterator. Held mouse
buttons and a pointer within eight coordinate units of an edge exclude the
event-list candidate, preserving stationary edge scrolling.

Two empty observations start 125-us waits. Every six further empty observations
doubles the limit, up to 1 ms. Changed display revision resets confidence and
reserves 2 ms of full-speed guest execution. Host input immediately cancels a
wait and reserves 100 ms at the configured MHz. Invalid candidates reset the
limit; normal hardware wakeups preserve confidence. This is a bounded polling
heuristic, not a general proof of inactivity. The original strict idle detector
remains separate. An optional two-head scheduler hint brings execution back to
the runtime on arrival at either selected IP. Interpreter
and JIT use const-generic specializations; ordinary execution contains no
block-head comparison. This scheduling hint is not an instruction breakpoint.
Each hardware advance stops at the next PIT, keyboard, DSP or OPL event. An
eligible IRQ, input-state change, RAM write, changed CPU state or the time limit
ends the wait. Debug/single-step execution disables it. Idle scheduling units
count toward the existing finite budget; they are not retired instructions.
The desktop exposes a live toggle, accumulated idle time and current wait limit.
MCP status also exposes the last recognized head and wake counters. The alias
`--experimental-heroes-adaptive-idle` enables the same policy. For this experiment
only, host PCM pacing uses low/high watermarks based on observed callback size,
with a 30-ms minimum high target and an 80-ms lead cap. This reserves audio
before the queue empties while guest interrupt timing remains device-driven.
It does not establish identical game state or glitch-free audible playback.

The separate `--observe-idle` diagnostic is address-independent and never
authorizes skipped execution. At `Session::advance` boundaries, no more often than
20,011 scheduling units, it samples CS/base/mode/IP into 64 fixed entries. A
250-ms guest-time window reports recurrence, input-query service calls, RAM
write/device-access deltas, host input and sampled display revisions. Four
visits to one site among at least eight samples, input queries, and no observed
input or display activity produce an **unverified polling candidate**. Queries
need not have returned empty. RAM/device effects are reported as unverified,
not treated as harmless; IRQ audio work may contribute to these counters.
Sampling can miss work or alias execution, and does not prove a complete loop.
The observer never authorizes sleeping. In addition to PC samples it runs
bounded reference-execution traces: at most 512 single steps every two million
scheduling units, with fixed storage for 32 aging conditions. The tracer supports
absolute byte/word/dword CMP with an 8-bit immediate followed by JZ/JNZ, and
IN AL,DX followed by AND/TEST and JZ/JNZ. It records actual branch outcomes and
AL returned by the guest IN; it never performs an extra device read. RAM values
are inspected through the side-effect-free debugger view, with segmentation
validation and paging rejection. A changed watched value records the step PC,
IRQ-pending state and device-access activity at that boundary. This does not
prove which instruction, IRQ handler or device wrote it, or identify a timer or
audio buffer automatically. Unsupported forms remain unclassified.

During trace bursts, existing strict proofs are abandoned and the optional
Heroes heuristic is cancelled; those paths resume between bursts. Reference
stepping affects profiling and can change heuristic polling cadence if both
experiments are enabled. Disable Heroes adaptive idle for reference comparisons.
No unconditional instruction tracing is added to the CPU. Existing strict proofs
and opt-in Heroes recipes remain independent. Disabled observation adds runtime guards and service/input guards; no CPU
instruction hook exists.
The desktop Details toggle and MCP status expose the most recent window.

The separate opt-in `--adaptive-idle` policy uses this tracer to learn a RAM
countdown guard: CMP against zero, JNE forward by at most 64 bytes to RET. The
first structural matches install up to two independent batch observation points.
Eight observations,
including both decreasing and increasing values, are required before waiting.
Only zero-based 32-bit code can supply these candidates. Each point retains its
own evidence: an adjacent static guard cannot overwrite a countdown while it
is learning. Occupied points can only be replaced after a one-second absence.
16-bit DOS/driver predicates cannot replace them.
Validated arrivals at that point refresh evidence independently of trace bursts;
sparse tracing must not expire an actively visited guard. A gap longer than one
guest second or a MHz change resets confidence and requires new counter changes.
It yields batches at the learned guard and revalidates the code, positive counter,
flat 32-bit segments, recent evidence at the current MHz and input queries.
Actual input reserves the shared 100-ms grace before waiting. Pointer position,
screen edges and display revisions do not gate this policy: they do not measure
CPU demand. The fixed Heroes event-list recipe retains its separate edge guard.
The next known service horizon minus max(100 us, observed recurrence peak)
bounds each wait to 25–250 us. Device events still run and eligible IRQs wake the
CPU through the existing cooperative path. Observation off disables learning
and clears its batch yield point. Details and MCP expose the arrival count and
last guard eligibility reason. Storage is fixed and execution allocates none.
This is a heuristic: neither the counter writer nor absence of work in the
caller is proven. It remains disabled by default and replaces the fixed Heroes
recipe when enabled. The diagnostic provisional budget remains separate.

The same opt-in policy also budgets repeated empty INT 33h/000Bh relative-motion
queries with no pressed buttons. It measures guest CPU time between queries,
subtracting its own cooperative waits. After eight intervals, the largest work
interval in a bounded 32-sample window sets the reserve. A query may request
up to `250 us - work`, further bounded by the next known service horizon minus
`max(100 us, 2 * work)`. Less than 25 us of spare time means ordinary execution.
Thus expensive work reduces waiting without interpreting screen contents or
mouse coordinates. Host input and MHz changes reset confidence. A pending wait
expires after 1 ms and starts only when maskable interrupts are accepted;
device events and IRQ wake behavior use the existing cooperative path.
This deliberately changes polling cadence and does not prove absence of useful
work between queries. It is experimental, not a general CPU-demand oracle.
Details/MCP report empty-poll waits and the observed work peak separately from
learned countdown checks. Disabled adaptive mode does not collect this budget.

Synchronization diagnostics also report a provisional time/step budget. Repeated
visits to the same condition within one trace burst measure a recurrence cost;
untraced gaps are excluded. At least four samples, at most 250 ms old and at the
current MHz, are required. The largest observed cost is retained until the
condition entry expires or its MHz changes. The estimator subtracts that cost
and a reserve of max(100 us, 25% of cost) from the next known service horizon,
clamping at zero. Four nominal CPU clocks per scheduling unit convert time to
steps; these are not measured historical processor cycles.

Known horizons include pending PIC requests, PIT channel-zero pulses, AT
keyboard events and the earliest DSP block completion assuming unstalled DMA.
Individual OPL/DSP sample-generation events are not CPU service deadlines.
For an observed mask 08h on the active VGA status port, the condition is linked
to the next retrace transition, which further bounds the horizon. This
is deliberately partial: host output queue slack, other hardware sources and
the causal connection between a condition and an IRQ are not established. A
recurrence may be an empty poll rather than useful work. Therefore the displayed
budget is diagnostic only and never permits sleeping or changes configured MHz.


Live MHz changes replace only the CPU step-clock ratio, retaining its fractional
phase rounded to the new denominator. Peripheral fractions and accumulated time
remain intact. The owner updates the active/reset recipe between batches; the
frontend does not replace the Session or restart the audio stream.

Two independent 8237 instances own address/count/mode/mask state. The machine
owns page registers and byte/word address wiring, including 64/128-KiB wrapping.
Audio DMA reads ordinary physical RAM independently of the CPU A20 line and
commits one unit after the memory access. No intermediate RAM copy is made.

The optional VFS write overlay owns reserved 4-KiB chunks and bounded file
metadata. Read/write opens retain the original input owner; merged reads borrow
base and upper storage. Repeated writes replace existing upper chunks. Truncate
tracks the visible base prefix so extending a shortened file returns zeros.
Whiteouts and renamed entries survive snapshot recovery. A compact extent mode
remains for callers requesting legacy overlay budgets below 4 KiB. The default
library drive stays read-only.

This makes chip tests independent of CPU instruction coverage. Future cards can
compose existing chips without introducing cycles: the Sound Blaster card may
depend on OPL, while the PC machine connects it to DMA and interrupts.

## Native dynamic core

The required static EXE-to-WASM path reconstructs executable images and
relocations offline, analyzes statically recoverable control flow, and emits one
large program module. Profiles guide optimization; code coverage comes from the
executable. Static entry/relocation tables replace runtime translated-block cache
management. The module shares runtime memory through an explicit owner contract
and calls WASM runtime functions directly. Host budgets are soft, with accounting
at block ends, checks hoisted before proven finite narrow loops, and infrequent
bounded checkpoints for unknown-duration loops. Actual completed work, including
overshoot, is returned in counted mode. An optional uncounted HOT code-generation
policy omits instruction/statistics counters and instruction-budget charges in
HOT regions, while retaining independent bounded runtime service points and
device-time progression. Reports distinguish charged work from unavailable total
instruction counts. The counted static slice has an experimental shared
Rust WASM runtime, code guards and device-horizon admission. Opt-in byte/word/dword
MOV and ALU lowering reads RAM/VGA and writes ordinary RAM directly in shared
WASM memory. Register/immediate PUSH and register POP also use direct RAM with
live SS.B stack addressing. Other stack operations, memory shifts, finite-loop hoisting and production
frontend selection remain incomplete. See
[WASM core](compatibility/wasm-core.md).

The offline preparation already has two independent system components.
`doslet-executable` borrows the source file and validates supported LE metadata
and direct internal fixup records without allocation. `doslet-aot` reconstructs
immutable object bytes, distinguishes file-backed spans from implicit BSS and
canonicalizes symbolic relocations with source provenance. Its static CFG starts
at explicit entries, follows recoverable transfers and preserves independently
reachable overlapping instruction streams. Unknown exits remain explicit.
An explicit bound traversal follows relocated near CALL/JMP/Jcc targets under
a full flat32 source CS. It can reach file-backed data32 objects while retaining
that CS; a destination object's data selector does not replace the executing
code context. Unreachable fixups and pointer values never become roots. Other
code contexts remain explicit stopped paths in this mode; the original unbound
code16 analysis remains available. A bound graph borrows its exact immutable
binding plan, and lowering rejects another plan even for the same source image.
Explicit offline binding plans validate load extents and selector views before
encoding supported relocations. Straight-line grouping preserves joins and
overlapping instruction identities under a fixed region-length bound.
Allocations and decode work have limits. Neither component chooses runtime selectors,
loads machine RAM or introduces executable-format dependencies into the CPU.

The optional system-layer `doslet-aot/wasm` adapter lowers original, explicitly
relocated instruction windows to the CPU-only `doslet-x86-wasm::static_program`
IR. That separate compiler emits direct tail calls between bounded regions,
passing registers/flags without frame flushes on known edges. Initial entry,
near returns and runtime callbacks use a static address dispatcher. Imported memory holds a
72-byte boundary frame; counted budgets are soft and HOT can be uncounted.
Independent structural checkpoints retain runtime control. The initial slice
handles register aliases, LEA, shifts/rotates, direct branches and optional MOV/ALU
memory operands; unsupported instructions exit before effects. Native acceptance adapters cover Wasmtime and direct WASM-to-WASM
callbacks sharing one imported memory. The application-level `StaticSession`
owns the Rust DOS/PC runtime and admits whole counted regions against exact
relocated source bytes, the live CS descriptor and event-free device grants.
Admission only reserves work; callbacks and final returns commit its actual
effects and device time. The direct shared-memory acceptance wrapper runs this
runtime under Wasmtime without a native callback trampoline.

An optional 384-byte descriptor follows the unchanged 72-byte CPU frame. It
exposes borrowed RAM, page generations, protected source-page markers, segment
windows and actual VGA backing bytes. Direct accesses check the full operand
span before effects; the PC excludes low-memory accounting and the VGA aperture
from the ordinary RAM span and requires A20 enabled. One internal WASM admission
helper checks immutable code guards and reserves the device grant without a
runtime import. RAM stores update distinct touched page generations immediately.
VGA reads implement linear banking or planar/chain4 selection and read-compare
against backing bytes, retaining the resulting latches in the descriptor.
If a source-page generation changes but every expected source byte still
matches, the owner renews the proof and retries the same static entry once.
A subsequent memory refusal executes normally through the interpreter;
changed source bytes never authorize a retry. Both the previous and refreshed
hardware grants must cover the complete region.
At service/final boundaries the owner commits reservations, RAM-write counters,
VGA latches and device-access counters before advancing device time. No borrowed
Rust reference survives entry to generated execution; no guest RAM/VRAM is copied.
By default, VGA writes and unsupported memory instructions enter the ordinary
interpreter. The optional `memory_write_helper` compiler mode requires inline
admission and adds a direct WASM-to-WASM memory-store import. An isolated MOV
store that fails its direct RAM proof can use that helper after segment/source
and grant validation. The helper first validates the complete bus span, commits
staged direct effects and accumulates reservations, then performs the ordinary
bus store and refreshes the descriptor. Live GPRs/flags remain in generated code;
neither instruction interpretation nor device-time accounting occurs in the
helper. A later callback/final return reconciles all accumulated work. Declined
helpers have no effects; generated code removes only the current reservation
before ordinary fallback. The runtime accepts byte/word/dword stores wholly in
RAM or VGA, including compiled source pages whose generations the bus updates.
Mixed RAM/VGA spans retain ordinary fallback. This is a memory-only slow path,
not a software TLB or larger-region optimization.
Direct memory ALU supports register destinations, memory CMP/TEST, and ordinary
RAM read-modify-write arithmetic, logic, INC/DEC/NEG/NOT. Each memory instruction
has a separate counted region; complete operand validation precedes its effects.
MOVSX/MOVZX extend byte/word sources into word/dword register aliases, including
guarded RAM/VGA sources whose access width is independent of the destination.
CBW/CWDE reuse pure register sign extension. These instructions preserve flags.
Two- and three-operand IMUL use captured word/dword operands and a signed 64-bit
product, replacing only CF/OF. Guarded RAM/VGA IMUL uses the operand width and
the same isolated counted region as other memory reads.
One-operand MUL/IMUL retain the full byte/word/dword product in AX, DX:AX or
EDX:EAX. Both inputs are captured before writing either destination, preserving
aliased source registers; guarded RAM/VGA forms use the same read contract.
Register/immediate PUSH and register POP support word/dword operands with an
independent 16/32-bit stack pointer, supplied explicitly by the owner in the
descriptor. Missing stack capability declines before effects. SS permissions,
complete RAM spans and write generation checks precede stack-pointer changes;
PUSH SP/ESP captures its old value and POP SP/ESP assigns after increment.
Bound fullflat32 direct rel32 CALL uses one guarded PUSH of the linear return
EIP followed by an internal subdivision edge. It charges one guest instruction;
the continuation is a separate static entry. Far, indirect and unresolved rel16
calls retain ordinary fallback.
Bound near RET/RET imm16 pops a word/dword EIP using live SS.B and transfers
through immutable linear aliases of existing regions. Known targets stay inside
the module; unknown targets delegate after completing RET. Source, stack or
hardware admission failures delegate before effects. The compiler preserves
soft-budget exits and structural service quotas across returns. Alias tables
create no new code and retain each target's existing source/context admission.
Large address tables use bounded internal dispatcher helpers within the same
module. Each helper handles at most 8192 sorted entries; a balanced range tree
selects it without a runtime call. This keeps individual function bodies below
the engine's parsing limit while retaining original region function IDs and
known-target service/admission checks.
Register shifts and rotates preserve byte/word aliases and masked counts;
word SHLD/SHRD with CL remains a fallback because the count cannot be proved
statically. Immediate masked word counts above 16 also retain the reference
CPU's explicit unsupported boundary. LEA computes only an offset, with independent operand/address widths
and no segment checks, memory access or flag changes. Optional offline partitioning isolates immediate operands and likely
mutable source instructions, retaining exact byte guards for every region.

`doslet-aot::waits` discovers bounded straight-line memory/compare loops as
candidates. An optional policy selects all candidates or only complete verified
COLD bodies, then emits runtime stubs at their existing CFG entries. The runtime
executes those slices through the interpreter, immediately starting the ordinary
CPU/RAM recurrence proof. Only proven passes may be omitted, strictly before
hardware events; same-value writes are allowed. Temporary proofs end before
returning to generated code. This saves host work without selecting a lower CPU
rate. HOT/COLD labels alone never authorize skipping. The independent adaptive
and speed-tier policies remain available.

The separate experimental `doslet-x86-wasm` compiler emits one WebAssembly module
from observed speed-tiers JSON or DSJ1 code profiles. It uses the shared CPU
decoder and a runtime callback interface, with internal dispatch between guarded
register blocks. Its CPU test adapters run natively and in a browser; production
selection remains pending. A diagnostic Session adapter uses the PC's device
horizon and ordinary IRQ/service fallback for bounded ROTT comparisons.
The optional native `doslet-wasm-aot` adapter precompiles those modules through
Wasmtime/Cranelift with the Speed preset, backtracking register allocation and
compiler inlining. It measures warmed execution separately from generation and
loading. Its private unsafe loader accepts only the same engine's freshly
compiled artifact; no public API accepts serialized native code for loading.
The standalone `doslet-wasm-game-probe` application also compiles the complete
interpreter/runtime to WASM for comparisons under that engine. Its private unsafe
FFI boundary transfers owned setup buffers and opaque owner handles to a trusted
synchronous diagnostic driver. One probe owns each Session; the shared Session
adapter and emulation libraries retain safe interfaces. The translated module
imports the probe's memory and scratch base, as in the browser worker.
See [WASM core](compatibility/wasm-core.md).
It does not replace the execution or adaptive scheduling strategies below.

The headless frontend enables `dynamic-core` by default on x86-64 and AArch64.
The desktop app makes it opt-in (`--features dynamic-core`, then `--dynamic-core`)
and enables `wasm-procedures` by default instead.
`doslet-x86` remains a `no_std` CPU library with static bus dispatch. Its optional
`doslet-x86-jit` dependency is a host compiler adapter using `std` and Cranelift;
it is excluded from WASM and from builds without the feature. The adapter has
no PC, DOS or filesystem dependency and never owns guest RAM. Its small unsafe boundary
converts finalized compiler output to private function pointers, invokes them
with borrowed general registers, EFLAGS and EIP, optionally borrowing a bounded
RAM span for a trace, and frees executable storage on drop. The rest
of the emulation workspace retains `unsafe_code = forbid`; the native WASM AOT
loader and macOS Audio Unit host adapter have their own private boundaries
described above.

There are two distinct representations. Address-independent prepared blocks
contain decoded register IR or a bounded RAM/control-flow trace and at most 240
expected code bytes. Execution cache
entries bind those blocks to CS state, offset, privilege and a bus `CodeCheck`.
The execution cache has 1024 slots allocated at CPU setup. An independent pool
retains compiled IR by code fingerprint, with full byte comparison before reuse;
execution-cache collisions and address changes do not discard native code.
Pool insertion happens only during explicit preparation. Code discovery performs no
allocation. A hot block becomes eligible after 16 observations; compilation is
explicit host maintenance. `Session::prepare_dynamic` remains a synchronous
API for headless tools and deterministic compiler tests, capped at eight blocks.
The desktop uses `take_dynamic_compilation` and `finish_dynamic_compilation`
instead: one job of up to two blocks is in flight through bounded single-slot
channels. The job exclusively owns one compiler bank and address-independent IR;
it contains no guest registers, RAM or device state. Cranelift construction,
compilation and retired-bank reclamation run on `doslet-jit`. While a bank is
away, its handles use interpreter fallback; other banks remain executable.
Publication checks the originating CPU identity and exact prepared bytes before
binding handles; execution still checks current code generations and segments.
Reset/replacement discards old results. Closing the channels lets the compiler
finish and reclaim its job without blocking shutdown. `Session::advance` never
calls the compiler. Executable storage is split into at most 16 independently
owned Cranelift modules, each holding at most 256 functions (4096 resident
functions total). A trace and its optional register fallback reserve their two
slots in the same module before either is emitted. Native entry and preparation
mark a module recently used. When no module has space, preparation retires the
least recently used module, removes its execution, template and pool handles,
and frees its executable storage before installing replacement code. Hot
modules remain usable; evicted blocks can become eligible again without a
session restart. `DynamicStats::code_cache_evictions` counts retired modules.
Imported templates are eagerly attempted once; eviction alone does not eagerly
recompile cold profile entries. Genuine compiler failures still latch once and
stop further compilation; existing blocks and interpreter fallback remain usable.

The first native subset consists of 2–16 straight-line dword register MOV,
ALU/CMP/TEST, INC/DEC, XCHG and NEG/NOT instructions in protected default-32-bit
code. A terminal short/near relative Jcc/JMP whose target is exactly the block's
start can form a native loop; its branch counts toward the 16-instruction limit.
It shares the existing instruction decoder with the interpreter. Native
code reads/writes eight general registers, EFLAGS and EIP. The register-only
compiler retains its original ABI. I/O, unsupported control transfers, segment
changes, x87, REP and other widths terminate a
block and execute through the interpreter. Whole-block CS checks protect fetch
limits. TF/RF/VM, paging, halted state and interrupt shadows prevent native
execution. Ordinary single stepping stays on the interpreter.

For straight blocks, the compiler tracks arithmetic flag liveness backwards:
only flags needed by a later ADC/SBB or at block exit are materialized. INC/DEC
preserve carry across these chains. Native loops conservatively compute full
arithmetic flags and carry them with the registers through an SSA backedge.
The generated function checks the hardware-bounded remaining step budget before
each whole iteration and returns the actual completed-step count. It writes
architectural state only on return: EIP points to the loop start after a taken
backedge, or after the branch when the condition is false. A budget remainder
smaller than one iteration uses the existing execution path. No guest memory
can change while this register-only loop holds its borrow, so code validation
occurs once on entry, rather than on every iteration.
Generated code stores only changed registers and works directly
on borrowed CPU state, without copying a ten-word temporary at every call.

A separate RAM trace compiler accepts up to 16 reachable instructions
inside one 240-byte code span. It follows relative branches and supports dword
MOV/ALU memory sources with 32-bit base/index/displacement addressing. LEA,
memory-left CMP/TEST, byte MOV/CMP/TEST and byte MOV stores extend the register
scope; AH-style aliases preserve all other bits of their parent dword. Dword
memory XCHG resolves its address and value from the old registers, guards the
load and store, then exchanges operands and updates page generations. In the
single-owner machine the exclusive RAM borrow prevents hardware activity from
interleaving with the exchange. PUSH/POP32,
near direct/indirect CALL and RET/RET imm16 use guarded ordinary-RAM stack
accesses. SS must be full flat with a 32-bit stack; CALL/RET additionally require
full flat executable CS, making their dynamic targets valid offsets. PUSH ESP
uses its old value, POP ESP overrides the incremented pointer, and indirect CALL
reads its target before writing the return address. Every failed guard exits
before the current instruction's register or memory effects. Direct calls follow
known targets and retain return sites during discovery; runtime CALL/RET targets
matching a prepared instruction transfer in SSA, otherwise they return to dispatch.
Discovery uses fixed arrays and rejects overlapping instruction starts. Other
stores, word overrides, far calls/returns and unsupported successors are interpreter exits.
The full contiguous span, including gaps and all branch bytes, is checked for
code generations and CS bounds; external exits receive conservative CS checks.
Every used data segment must be flat, fully readable and cover all 32-bit
offsets; store segments must also be writable. Positive validation is cached
against used selectors and full hidden descriptors within the execution-cache
key (CS, entry IP, privilege and mode). A separately compiled register prefix
remains usable when the larger RAM trace cannot run; it is attempted only after
zero trace progress and fits the same resolved hardware budget. Both functions
occupy slots in the same bounded code bank. Replacing the prepared trace clears the
result. Changed DS/SS state cannot reuse stale assumptions.

`MemoryBus::native_ram` optionally returns `NativeRam` with a borrowed immutable
backing slice, linear base and one excluded interval. Its default declines native
reads. The PC exposes `500h` through the configured RAM end only while A20 is enabled: IVT/BDA stay
outside the span and the VGA aperture is explicitly excluded. Backing bytes in
that interval are never CPU-visible through native access. Each native access
checks the complete byte/dword against the view, exclusion and 32-bit overflow
before forming a host pointer; unaligned dwords use little-endian order. Failed guards return before the load, preserving
prior progress, full flags and EIP. The pointer is never retained between calls.
No device or mapping can change during the immutable borrow.

`MemoryBus::native_ram_mut` optionally supplies `NativeRamMut` with disjoint
exclusive borrows of the RAM backing span and the full physical 4-KiB
page-generation table, with the same mandatory excluded-interval guard. The PC requires A20 enabled and an available code space. Borrowing does
not itself invalidate pages. Generated stores check complete byte/dword bounds,
32-bit overflow, generation-table bounds and non-exhausted counters, and reject
the pages occupied by the active trace (including gaps). A successful store
updates each touched page generation once before further execution. Other
compiled blocks therefore fail their next generation check; self-modifying
stores leave native execution before changing active code. The interpreter
handles the store and invalidation. No pointer survives the exclusive borrow.
The read-only compiler API rejects traces containing stores. The read-only CPU
API may execute their register prefix, but never their stores. Writable execution
requires an exclusive bus borrow.

Trace nodes carry registers, flags and remaining budget in SSA, preserving
state across internal branches/backedges. The first version computes full flags
and checks the remaining hardware-bounded budget per instruction. Every exit
returns the actual completed-step count, including completed native stores. `ram_trace_steps` and `ram_trace_entries` identify this
subset of total native execution. The resident-function limit is shared with
the register-only compiler.

Optional exit diagnostics must be enabled before native compilation. The CPU
allocates 8,192 aggregation slots during setup, then records CS, entry/exit IP,
linear addresses, reason, count and completed steps without execution-time
allocation. Refused entries are separate from actual native returns, including
zero-step guard failures. Profiled traces use a private reason-bearing return
ABI; ordinary generated functions remain uninstrumented. The diagnostic runner
checks guest state against interpretation and exports metadata CSV separately
from timing benchmarks. See [exit measurements](compatibility/native-core.md#measured-native-exit-reasons).

The PC delivers pending eligible IRQs before entering a slice of at most 256
steps, bounded by the caller's budget and strictly before the next PIT/DSP/AT-keyboard
deadline and the 1-ms hardware advance limit. A slice combines native blocks
with decoded ordinary-RAM operations, near branches and stack accesses. The
bus must explicitly guarantee the complete operand spans are writable ordinary
RAM via `MemoryBus::is_plain_ram`; segment, stack and branch bounds are checked
before execution. Faultable accesses, MMIO, I/O, mode/IF changes, REP and other
unsupported forms stop the slice before their effects. The interpreter handles
that boundary instruction with current hardware time and interrupt state.
The PC updates clocks and scheduling counters once per slice. Native execution
counters also accumulate locally, while `Session::advance` commits its total
step count once per host batch. VGA polling candidates retain their existing
per-instruction observation. This reduces interrupt checks without delaying IRQ
delivery. Native blocks that do not fit fall back without native side effects.
Every entry checks page, mapping and owner generations. After a generation change,
identical bytes may rebind existing code; changed bytes require new IR and
compilation. RAM writes inside a slice still invalidate code before the next
instruction/block lookup. No large RAM/VRAM buffer is cloned.

Dynamic-core profile preparation (a per-executable cache file by default, or
`--core-profile FILE`) stores at most 256 observed hot blocks
in a versioned file of at most 62,726 bytes. It records linear address hints and
exact post-relocation code bytes, never host code or pointers. Imports are bounded,
fully decoded and compiled again. Runtime binding decodes the complete current
block and searches the prepared pool by fingerprint plus exact bytes and length;
a historical shorter register prefix cannot pin a newer RAM trace. Address hints
remain format metadata. Byte comparison and the current CS/code checks
are mandatory before a first native entry. Invalid profiles report a warning and
leave ordinary execution available. Profiles currently use observed addresses,
not executable object identities. Identical register code can bind at a new
address, while changed relocation bytes lose a hit and safely rediscover code.
LE/LX static object traversal and relocation-aware
binding remain future work. Profiles may contain proprietary guest code and stay
local, outside the original game directory.

## Service interception

An optional [DPMI host](compatibility/dpmi-host.md) belongs to the runtime session.
It supplies its own descriptors, gates, bounded callback frames and guest-page
allocator. The CPU only exposes integer context save/restore and explicit mode
setup; it has no dependency on DPMI or BIOS. Eligible unchanged service shims can
be dispatched in protected mode while preserving the reflected path's memory
effects and logical time. The PC supplies one bounded timing step for a replaced
IRET, charged to the protected dispatcher label when speed tiers are active.
Native desktop launches select direct dispatch by default, including profiling
copies; `--dpmi off` selects the original guest extender. Bus-observed
analyzer steps and debugger attachment retain reflection for instruction tracing.

The CPU implements ordinary INT and IRET stack/vector behavior. The runtime
installs BIOS/DOS shims in guest memory and checks arrival at those addresses.
Replacing an IVT entry runs the guest's replacement handler. This avoids teaching
the CPU about INT 10h video modes or INT 21h DOS functions.

The shim region is writable RAM in this bootstrap, not a complete BIOS ROM model.
The runtime also checks its IRET byte before intercepting the service.
Interception is restricted to real mode; protected guest handlers execute through
the CPU's descriptor and interrupt-gate path. Complete ROM protection, exception
behavior and service ABI coverage remain milestones.

## Presentation boundary

The frontend requests a frame only after a bounded execution batch. A revision
number prevents rebuilding an unchanged frame. The initial iced image handle
requires owned pixels, so presentation allocates an RGBA image and moves that
buffer into iced. Handle clones share the underlying image; they do not clone
the emulated machine.

Desktop and browser xBRZ use an iced shader primitive with a WGSL port of
[libretro's xBRZ freescale shader](https://github.com/libretro/slang-shaders/blob/master/edge-smoothing/xbrz/shaders/xbrz-freescale.slang).
Its GPU texture stays at guest resolution, is uploaded only when the presentation
handle changes, and is reused until the video dimensions change. Corner detection
and blending run in the fragment shader at the physical viewport size. The
shader clamps source-edge reads and handles the target's sRGB encoding. The
shader retains upstream Hyllian/Zenju attribution and license notices (MIT and
GPLv3). Browser builds use the same shader through wgpu's WebGPU/WebGL backend.
When iced falls back to Canvas2D, selecting xBRZ reports that GPU rendering is
unavailable and retains the current filter. The check uses iced's existing canvas
context rather than a separate GPU capability probe.

A future shader widget can own reusable GPU textures and accept indexed pixels
plus a palette, reducing CPU conversion and repeated host allocations. GPU upload
still has a transfer cost. That work belongs to presentation and must not change
the chip's borrowed scanout interface.

## Extension rules

- Add a chip crate when there is actual state and behavior to implement. Avoid
  empty crates for every item on the roadmap.
- Keep chip-specific internals under `registers`, `memory`, `timing`, `scanout`
  or equivalent modules as their implementations grow.
- Share bus/time contracts; do not create a catch-all common library.
- Use monomorphized bus access and concrete chip types in execution paths.
- Put filesystem, audio-device, window and Worker adapters in platform code.
- Add another CPU engine behind the architectural-state boundary. Keep a simple
  interpreter as the correctness reference; benchmark before specializing.
- Reserve large buffers and event capacity at setup. Expose overflow explicitly.

## Synchronization analysis boundary

`doslet-x86::analysis` exposes bounded decoded effects and control-flow records
from the interpreter decoder. It owns no PC devices and only uses side-effect-free
`MemoryBus` code views and page-generation checks. Each instruction is a small
basic-block subdivision with explicit successor edges. The first supported
context is flat 32-bit CS/DS/SS without paging; other contexts remain incomplete.

`doslet-bus::observation::Observed<B>` statically wraps an ordinary bus for traced
CPU steps. It forwards transaction widths and records returned reads and successful
writes in 64 fixed slots. Failed partial transactions and capacity exhaustion
mark the trace incomplete. Ordinary execution instantiates the machine step with
observation disabled; it does not call an observer per instruction. The PC layer
records actual IRQ entry and distinguishes hardware RAM changes outside the CPU
transaction trace. Runtime services remain outside that wrapper and explicitly
invalidate instruction-trace episodes when their writes are unobserved.

VGA boundary accounting does not require tracing the work between polls. I/O
ends ordinary native/register batches, so the runtime observes the actual
IN/test/branch sequence and records confirmed waiting. It retains the existing
strict VGA batching proof through `try_vga_wait_batch`; it introduces no new
skipping semantics. The difference between total guest elapsed time and confirmed
waiting is a conservative work upper bound, including IRQs, runtime services and
unsupported code. Sparse static coverage is reported independently rather than
used to erase this continuous clock measurement or classify unknown work as idle.
An IRQ suspends an in-flight port condition until the actual IRET returns to
the saved CS/IP/SP, with a 20-ms bound. Interrupt time remains work. An abandoned
condition invalidates its incomplete episode, preventing adjacent presentation
periods from being joined accidentally.

The runtime owns a fixed 64-node graph, four predicate records, an eight-entry
observed IRQ stack and bounded episode statistics. Code-generation, context,
trace-gap and clock changes invalidate the relevant evidence. An unknown initial
interrupt context is never called foreground ownership. The runtime's separate
opt-in controller changes effective CPU throughput between batches, preserving
configured MHz as its fallback ceiling. No confidence score authorizes loop
skipping. MCP serializes named fields in the host adapter; no JSON allocation or
host clock enters emulation execution. See [development](development.md#bounded-synchronization-analyzer)
for the admitted predicates, limits and validation boundary.

Returning guards have two separate diagnostic records. Their caller is linked
through the actual flat stack and decoded direct CALL, rather than guessed from
a returning function's recurrence cost. A bounded IRQ-only observation path
captures their RAM writer independently of sparse foreground discovery. Static
effect gaps do not erase actual bus-write attribution within that observed
handler; nested interrupts, trace overflow, unobserved writes and budget
exhaustion do. These guard records do not authorize throughput reduction.

A diagnostic caller probe follows one dynamically observed returning iteration,
including bounded transitions through 16-bit code. It compares CPU, input,
ordinary RAM write results and modeled CMOS register state at the same instruction
boundary. Its fixed effect table and byte ledger live in the session; only small
summary statistics enter frontend snapshots. The opt-in bus wrapper captures old
RAM values through ordinary-memory views, never extra device reads. Mapping gaps,
write/trace overflow and unobserved effects remain explicit. This dynamic inventory
does not extend the admitted static graph or the strict skipping contract, and
does not itself qualify a controller budget.

An unchanged caller inventory may install an exact instruction boundary for
continuous recurrence measurement. The PC owns a separate bounded RAM journal
and IRQ/device-access marks; the runtime owns CPU/input snapshots and checks
fresh countdown-source evidence. Only restored CMOS shutdown-status scratch
writes are admitted device effects. Every interval executes normally; only after
complete state comparison can its elapsed time enter continuous wait accounting.
The returning guard's zero/nonzero arrivals can delimit complete work/wait
episodes; source-only samples still cannot qualify a budget. Unmodeled services,
interrupts, mapping changes and journal overflow
exclude the interval. Native store views are unavailable while the journal is
valid, so writes cannot bypass its old-byte capture. This observation path does
not extend the strict idle-skipping contract. While that journal is valid, the
PC selects decoded batches at the scheduling boundary, avoiding repeated failed
native-store attempts. Overflow and journal completion restore normal JIT use.

When no returning-guard anchor with fresh source evidence exists, bounded scouts may install an
executed, taken direct backedge as a recurrence boundary, including conditional
branches. A fallthrough does not discover a boundary. This requires
an existing, fresh, stable VGA cadence model; the backedge does not supply a new
deadline. A frequently visited guard without observed writers cannot monopolize
discovery; a later caller inventory does not replace an active backedge.
The same complete state journal checks every execution, including
indirect calls. Eight consecutive unchanged intervals qualify the boundary;
only individually verified later intervals contribute waiting to complete VGA
episodes. Changing or interrupted executions remain work. An interval exceeding
5 ms loses its journal but retains the boundary, so animation work does not erase
the next frame's measurement. A boundary absent for 250 ms, changed code/context,
or missing presentation source ends observation.
Source candidates are checked at complete arrivals before classifying waiting;
code validity and journal/absence time limits are checked between arrivals too.
Eight rejected/cancelled intervals abandon an unqualified probe. A qualified
boundary tolerates bursts of changing handlers for at most 250 ms without
another unchanged interval, without crediting those changes as waiting. An
eight-entry cooldown table prevents immediately retrying an abandoned probe for
250 ms. Losing a qualified
anchor invalidates the associated work estimates. Storage remains fixed-capacity
and this extension does not skip execution or recognize game addresses.
