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

AdLib/Sound Blaster selection reproduces a write to port 0B8Ah at startup,
followed by OPL probing at 388h. OPL synthesis and the unidentified auxiliary
port are unsupported; do not treat this path as working audio. Roland audio
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
