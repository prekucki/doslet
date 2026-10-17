# PRINCE music: static inventory and synth selection

Inspected 2026-10-14. Inputs are the ignored, read-only local installation.
This analysis reads MIDI structure, sound configuration and printable executable
strings. It also decodes the previously recorded `target/prince-midi.log`;
that trace is separate execution evidence, not a fresh listening comparison.
No game assets or MIDI payloads are included in this document or distributed.

## Reproduce

```sh
cargo run -p doslet-headless --example inspect_music --locked -- PRINCE
cargo run -p doslet-headless --example inspect_music --locked -- PRINCE target/prince-midi.log
cargo test -p doslet-headless --example inspect_music --locked
```

The read-only example accepts synchronous SMF formats 0/1 with PPQN timing. It
merges track events, integrates tempo changes and reports programs, channels,
controllers, bank-select values, note counts, pitch bends and SysEx identifiers.
It also scans printable signatures in EXE and `.386` files; see
[the original toolchain evidence](prince-toolchain.md). It reports peak
simultaneously held channel/key pairs, excluding sustain,
release tails, layered samples and duplicate voices. This is not a synthesizer
voice-count measurement. Executable string offsets are file offsets, not code
addresses or proof of control flow. Three synthetic tests cover tempo-track
merging, running status, SysEx classification and malformed input.

## Observations

- All 25 `.MID` files in this installation are SMF format 1, with PPQN 120 or
  240. None contains a SysEx event. Music comes from authored sequences; this
  inventory found no procedural composition mechanism. It cannot rule out
  event transformations or additional generation in executable code.
- `JVSOUND.CFG` selects `midi=mpu401` at port 330h, with Sound Blaster wave output.
  It contains no `mpusynth` value. `PRINCE.EXE` and `SETSOUND.EXE` contain that
  key and explicit General MIDI / MT-32 / Sound Canvas choices. The meaning of
  an omitted value has not been recovered from code.
- The executable contains adjacent filename pairs such as `GM_LOGO.MID` /
  `MT_LOGO.MID` and `MVMNT1WM.MID` / `MVMNT1MT.MID`, with repeated filenames for
  some other pairs. These are evidence of alternate arrangements, not enough
  to infer every selection condition or safely classify every song by suffix.
- The recorded 300M-step guest stream uses the `MT_LOGO.MID` arrangement:
  channels 2/3/program 77 have 15/11 note-ons, channel 4/program 58 has 151, and
  channel 9/program 5 has 2. These exactly match its 179-note-on file inventory.
  The following `PAVANA.MID` contributes 90 more note-ons before the trace ends.
  The observed programs are forwarded unchanged, with no SysEx in that trace.
- `PAVANA.MID` is 37.895 seconds under its stored tempo map, with 255 note-ons
  on five melodic channels and at most nine held keys. Its programs are 73, 60,
  57 and 43 (zero-based). Channel 6 also selects bank MSB 0 / LSB 74. The trace
  preserves those bank messages; support for that bank depends on the synth.
- Across these files, the largest held-key peak is 19 (`MVMNT1MT.MID`). Used
  controllers include modulation, volume, pan and reverb send. This is modest
  note/event density; it does not measure the CPU cost of actual sample layers,
  effects, interpolation, synth scheduling or PCM output buffering.

## Instrument numbering matters

The MIDI protocol itself does not assign the same timbre to a program number
on every device. [General MIDI](https://midi.org/general-midi) standardizes a
sound set; MT-32 has a different preset map. For example:

| Zero-based program | General MIDI interpretation | MT-32 preset interpretation |
| --- | --- | --- |
| 5 | Electric Piano 2 | Electric Piano 3 |
| 58 | Tuba | Harp 2 |
| 60 | French Horn | Guitar 2 |
| 77 | Shakuhachi | Pan Pipes |

See the [Roland MT-32 manual](https://cdn.roland.com/assets/media/pdf/MT-32_OM.pdf)
for the original preset map. Several inventory values and the selected logo
arrangement make MT-32 compatibility worth investigating. This does not prove
that every song is MT-32, that all configuration branches work, or which device
was the composer's reference. A generic GM SoundFont can play the notes while
choosing a different instrument. Changing sample quality alone cannot resolve
the program-map mismatch. Do not automatically remap the whole soundtrack by
filename suffix; first verify the sound library's `mpusynth` selection and any
per-song conversion.

## Practical choices

These are compatibility-based recommendations, not a measured listening ranking
for this installation. No SoundFonts were downloaded or compared here.

| Playback mode | Starting choice | Reason and limit |
| --- | --- | --- |
| GM through TiMidity | FluidR3 GM | Established complete GM bank; a useful baseline after verifying GM selection. It does not reproduce the MT-32 preset map. |
| GM/GS through FluidSynth | GeneralUser GS 2.0.3 | Preferred candidate for its compact bank and careful instrument programming; the author recommends a compliant synth, including FluidSynth 2.3 or later. |
| GeneralUser through an older/less complete SF2 player | GeneralUser GS SoftSynth 1.44 | The author's compatibility fallback; do not assume modern 2.x modulators render correctly in an unverified TiMidity build. |
| A different GM interpretation | Arachno | A complete GM/GS bank with a different sample palette; try only after establishing the program map. |
| Authentic MT-32 playback | Munt with the required ROM images | Device emulation, rather than a GM SoundFont. Appropriate when the game actually selects the MT-32 arrangement/driver behavior. |

Primary references: [FluidSynth's known-working banks](https://www.fluidsynth.org/wiki/SoundFont/),
[GeneralUser author and compatibility guidance](https://schristiancollins.com/generaluser.php),
[Arachno author](https://www.arachnosoft.com/main/soundfont.php),
and [Munt project](https://github.com/munt/munt).

Host buffering can affect native MIDI continuity independently of the guest's
program map. Compare buffering settings on the target system and assess audio
by listening; counters alone do not establish instrument fidelity.
