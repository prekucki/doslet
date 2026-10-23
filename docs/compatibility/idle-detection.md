# Idle detection and waits

Doslet has several ways to reduce work while a guest waits for a timer, display
event or input. They have different guarantees. The native desktop **CPU → Idle
detection** card provides live switches for the general execution paths. Its
switches affect the current session; **Reset** restores launch defaults. A
switch does not change the CPU core or the guest clock mode.

| Method | When it applies | Default | UI | Guarantee |
| --- | --- | --- | --- | --- |
| Proven idle loops | Native CPU, including the interpreter | On unless launch policy disables it | CPU → Idle detection | Skips only a completed loop whose CPU, RAM and observed input state match the proof conditions. |
| VGA retrace polling | Native CPU at recognized VGA status-port loops | On | CPU → Idle detection | Batches whole iterations before the next device event; falls back when code or machine state changes. |
| Adaptive empty-input wait and learned countdown | Native CPU with `--adaptive-idle` | Off | CPU → Idle detection | Changes polling cadence after repeated empty mouse queries or validated countdown observations. It is a heuristic, not an identical-state proof. |
| v86 RAM polling loop | v86 with the host clock | On | CPU → Idle detection, with park count | Parks a recognized idempotent timer-polling pass. Time and devices advance; execution resumes for an IRQ or a changed value. |
| v86 VGA retrace trap | v86 with the host clock | On | CPU → Idle detection, with park count | After three repeated reads, traps the exact `IN AL,DX; TEST AL,08h; JE/JNE back` loop. Parks only while the last branch and current retrace status both say to repeat. Wakes toward the next status transition. |
| v86 MPU-401 status polling | v86 with the host clock | Off | CPU → Idle detection, with dense-read count | Experimental pacing for the exact transmit-busy loop at port `331h`; every instruction still executes. |
| v86 DOS keyboard polling | v86 with the host clock | On | CPU → Idle detection, with park count | Heuristic: after repeated empty INT 21h/AH=0Bh queries and 250 ms of unchanged presentation, yields the worker for at most 16 ms or until the next device event. Guest CPU state and time are not advanced by the park itself. |
| Heroes menu/event-list wait | Native CPU with `--experimental-heroes-menu-idle` | Off | CPU → Idle detection, when a Heroes executable is configured | Version-specific, bounded polling heuristic; input and device events wake it. |
| Heroes timer wait | Native CPU with `--experimental-heroes-timer-waits` | Off | Launch option | Batches selected CMP/JNE iterations before device deadlines. Requires the matching executable sites. |
| Idle observation | Native CPU with `--observe-idle` | Off | CPU → Idle detection | Diagnostic only; never authorizes a skipped instruction. |

The v86 detector recognizes the complete 32-bit instruction shape
`MOV EAX,[ticks]; MOV [ESP],EAX; MOV EAX,[ESP]; CMP EAX,[oldtime]; JE back`.
It scans 256 bytes behind and 64 bytes ahead of the current IP within its code
page, caches the physical loop head,
and makes v86 return at that instruction before checking the wait. The cached
code is checked again on each batch. Near this head v86 uses its interpreter
so a translated block cannot run past the boundary; elsewhere it keeps its JIT.
The detector reads both operand addresses from the guest code, so neither game
nor RAM address is hardcoded. At the backedge it checks the live registers, flags,
stack value and both RAM values. It also requires ordinary mapped RAM, A20,
interruptible protected-mode execution, no pending PIC interrupt and no paging.
If any check fails, v86 executes normally. A park retires zero CPU instructions;
the host-clock owner continues advancing devices and audio, but does not
recheck or execute the loop on each clock visit. The latched CPU wait ends when
the PIC has a deliverable IRQ or the host changes CPU/memory state. The native
worker sleeps to the next known PIT, keyboard or DSP IRQ deadline; input wakes
it earlier. On waking, the host clock advances devices in bounded slices
before the CPU runs. If no IRQ deadline is known, audio and presentation use a
fallback cadence. The first park is
logged with its PC; the switch and cumulative park counter are available in the
CPU panel. This recognizes one conservative loop shape; it is not a general
RAM-wait detector.

The native proof and v86 recognizer use different evidence. v86 does not provide
the native loop journal, and the native idle, VGA and adaptive switches are not
active under v86. Host-clock waits can change guest timing relative to CPU
instruction count. The v86 detector has a synthetic CPU-state regression test;
that test does not establish an improvement or compatibility in a real ROTT
session. Compare repeated real-game runs with the switch on and off before
claiming a speedup.

The v86 VGA trap is separate from RAM waits. After normal v86 execution
returns from repeated status reads, Doslet installs a boundary at the loop's
input instruction. On the taken backedge it verifies the exact instruction
sequence, flags and current side-effect-free VGA status before parking. A port
write or IRQ removes the trap. CIV's RAM-wait `Parks` counter can remain zero
while its VGA parks rise. MPU-401 pacing remains optional because its loop has
a changing CX counter. The periodic native log reports worker active time,
guest steps per second and wait counters. Worker active time is wall time
outside its command wait, not total process CPU usage.

The mode-13h retrace pulse is only about 63 microseconds wide. The native
worker sleeps until 250 microseconds before the predicted edge, subtracting
any lag behind the host clock, then checks without sleeping until the status
changes. This avoids missing a pulse and delaying visible title animation by
another frame.

The v86 DOS keyboard poll method addresses menus that repeatedly ask DOS whether
a key is available without changing the screen. It observes the presentation
revision every 32 queries and requires at least 64 empty queries before parking.
Input commands wake the worker. Unlike a proven idle loop, this method does not
prove that guest RAM or registers recur, so it is a timing heuristic. Turn it
off in the CPU panel if a game advances work between empty key queries without
updating the screen.

For implementation details and the existing native validation, see
[architecture](../architecture.md#chip-to-machine-boundary),
[v86 timing](v86-core.md#ownership-and-execution) and
[Heroes waits](heroes-waits.md).
