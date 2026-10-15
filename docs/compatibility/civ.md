# Civilization DOS compatibility evidence

Test input: local CIV.EXE version 474.03. Game files and synthesizer ROMs are
external inputs and are never modified or distributed by Doslet.

## Recorded execution

A headless run using VGA, no sound, and keyboard input reached the Roman map
with a Settler in 4000 BC after 2 billion bounded execution steps. This does
not establish complete campaign, city-screen, or persistent-save compatibility.
Creating and reopening new files in the RAM overlay is covered by allocation
regression tests; reset discards the overlay.

A Roland startup run completed 150 million steps, processed 11 SysEx messages
and 23 note events, and rendered 581818 stereo frames at 32 kHz with nonzero
PCM. This verifies synthesis with the local CM-32L ROMs, not live speaker output.

Mouse startup (VGA / no sound / Mouse and Keyboard) completed 800 million steps.
A separate 150-million-step run injected movement and left-button transitions
without a fault. Synthetic tests verify callback parameters, RETF return,
register and stack restoration. Interactive native/browser clicking has not
been verified in a live window.

The local probe is `apps/doslet-headless/examples/inspect_civ.rs`. Its arguments
are game path, startup keys, execution budget, optional scheduled BIOS keys,
and optional CM-32L ROM directory. Mouse events can be supplied through
`DOSLET_CIV_POINTER_EVENTS=STEP:X:Y:BUTTON:DOWN,...`, with -1 for movement.

## Roland messages

CIV uses Roland DT1 messages with device 10h and model 16h, with valid Roland
checksums. Startup writes a display label at 20 00 00, system settings at
10 00 00, 42 patches starting at 05 00 00, and six custom timbres starting at
08 00 00: Bongos, Gourd, Explosion1, Explosion2, Explosion4, and SwordSwish.
The procedural synthesizer does not interpret these messages. The optional
native CM-32L engine does, using external Control v1.02 and PCM ROMs.

## Remaining audio scope

AdLib/Sound Blaster selection writes zero to 0B8Ah before probing OPL at 388h.
The local `ASOUND.CVL` contains this sequence. 0B8Ah is the Pro Audio Spectrum
audio-filter register, as documented in the
[86Box PAS implementation](https://github.com/86Box/86Box/blob/master/src/sound/snd_pas16.c).
Doslet has no PAS card: this one register reads FFh and ignores writes, without
aliasing it to OPL or accepting other unknown ports.

A 2026-10-16 headless run selected VGA, AdLib/Sound Blaster and keyboard input:

```sh
cargo run -p doslet-headless --release --locked --example inspect_civ -- games/civ/CIV.EXE 141 150000000
```

It completed 150 million steps with `Running`, no fault and mode-13h graphics,
producing 814079 nonzero FM stereo frames through the PCM queue. These frames
were drained without host playback; the reported `mixed_peak=0` reflects the
inactive host mixer in this probe, not silent OPL samples. This verifies startup
and guest FM generation, not audible playback, music fidelity or gameplay.

The local `ASOUND.CVL`, `ISOUND.CVL` and `RSOUND.CVL` also contain a routine
that reads RTC time through INT 1Ah/AH=02h, converts BCD hours/minutes/seconds
to binary, sets hundredths to zero, and calls DOS INT 21h/AH=2Dh. This is a
DOS/RTC synchronization sequence, not a request to change the host clock.
The probable purpose is restoring DOS time after sound-driver timer use;
that motivation is inferred from the driver context. Doslet implements this
service within the guest, with validation and matching AH=2Ch reads, rather
than reporting success without applying the requested time.

Roland audio
is enabled with `--mt32-rom-dir`, using selection 5 in the game. Browser ROM
selection and MT-32 synthesis integration remain pending.

Custom sound selection in this version invokes DOS INT 21h/4B03h to load
`Xsound.cvl` as an executable overlay. A traced run confirmed the filename;
the local installation has no such file and exits during startup. This is a
game-specific driver module. The original manual instead names XSOUND.EXE.
The 1994 MicroProse update supplied SOUND.EXE, CONFIG.SND, PSOUND.CVL (OPL3)
and GSOUND.CVL (General MIDI). Its reproduced README is available at
https://groups.google.com/g/comp.sys.ibm.pc.soundcard.games/c/xHgER2RCrmc.
The original manual is reproduced at
https://www.civfanatics.com/content/civ1/manual/civ1_man.htm.


## MIDI routing validation

A recorded 150-million-step CIV run started in GM mode, automatically upgraded
to MT-32, translated 11 valid SysEx messages and 23 notes, and rendered 581818
stereo frames at 32 kHz through Rust synth (peak 0.131792). No checksum errors
or unmapped notes occurred in this startup sample. This is an approximation,
not MT-32 synthesis, and it does not cover all custom game effects.
The probe enables this path with `DOSLET_CIV_MIDI_MODE=auto` or `mt32`.

PRINCE was also checked after the DOS mouse state services were added:
300 million UI execution steps reached 640x480 graphics without a CPU/DOS fault.
This is startup evidence, not protected-mode mouse callback validation.
INT 33h/0015h reports a 160-byte state buffer; 0016h/0017h save/restore the
selected driver state. Logitech CyberMan query 53C1h returns unsupported
(AX=0), per the Logitech SWIFT supplement:
https://www.bitsavers.org/pdf/logitech/cyberman/CyberMan_3D_SWIFT_Supplement_Ver_1.0_upd_199401.pdf.
