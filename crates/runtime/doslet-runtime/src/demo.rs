//! Original test program: fills mode 13h with 16 horizontal palette bands, then exits.
//! This is executed by the guest CPU; it is not a host-side framebuffer animation.
pub const COLOR_BARS_COM: &[u8] = &[
    0xB8, 0x13, 0x00, // MOV AX,0013h
    0xCD, 0x10, // INT 10h
    0xB8, 0x00, 0xA0, // MOV AX,A000h
    0x8E, 0xC0, // MOV ES,AX
    0x31, 0xFF, // XOR DI,DI
    0x31, 0xC0, // XOR AX,AX
    0xBD, 0x10, 0x00, // MOV BP,16
    0xB9, 0xA0, 0x0F, // band: MOV CX,4000
    0xF3, 0xAA, // REP STOSB
    0x40, // INC AX
    0x4D, // DEC BP
    0x75, 0xF7, // JNZ band
    0xB8, 0x00, 0x4C, // MOV AX,4C00h
    0xCD, 0x21, // INT 21h
];
