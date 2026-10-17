// Original 16-bit guest fixture. Poll the MPU write-ready bit before each byte,
// just as a UART driver must; host draining cannot replace this handshake.
fn append_midi_byte(program: &mut Vec<u8>, byte: u8) {
    program.extend_from_slice(&[
        0xBA, 0x31, 3, // MOV DX,331h.
        0xEC, 0xA8, 0x40, 0x75, 0xFB, // IN AL,DX; TEST AL,40h; JNZ poll.
        0x4A, 0xB0, byte, 0xEE, // DEC DX; MOV AL,byte; OUT DX,AL.
    ]);
}
