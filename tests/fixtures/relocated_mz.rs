// Original, redistributable real-mode DOS program; no commercial game data.
// MOV AX, relocated data segment; MOV DS, AX; MOV DX, 0; print; exit with 7.
pub const fn relocated_mz() -> [u8; 160] {
    let mut file = [0; 160];
    file[0] = b'M';
    file[1] = b'Z';
    file[2] = 160; // One partial 512-byte page.
    file[4] = 1;
    file[6] = 1; // One relocation.
    file[8] = 2; // 32-byte header.
    file[10] = 16; // 256 extra bytes, including the stack.
    file[12] = 16;
    file[14] = 8; // SS = load segment + 8.
    file[17] = 1; // SP = 256.
    file[24] = 28; // Relocation table offset.
    file[28] = 1; // Word operand at module offset 1, segment 0.
    let code = [
        0xB8, 2, 0, 0x8E, 0xD8, 0xBA, 0, 0, 0xB4, 9, 0xCD, 0x21, 0xB8, 7, 0x4C, 0xCD, 0x21,
    ];
    let mut index = 0;
    while index < code.len() {
        file[32 + index] = code[index];
        index += 1;
    }
    let message = b"Relocated MZ$";
    index = 0;
    while index < message.len() {
        file[64 + index] = message[index];
        index += 1;
    }
    file
}
