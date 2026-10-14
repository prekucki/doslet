// Original synthetic COM program. It selects VGA mode 13h in real mode,
// installs its own GDT, enters a 32-bit code segment and fills VGA via REP STOSD.
pub fn protected_graphics_com() -> Vec<u8> {
    let mut program = vec![0; 0x400];
    let startup = [
        0xB8, 0x13, 0, 0xCD, 0x10,
        0x66, 0x0F, 0x01, 0x16, 0, 2,
        0xB8, 0x15, 0, 0x0F, 0x01, 0xF0,
        0x66, 0xEA, 0, 0, 0, 0, 8, 0,
    ];
    program[..startup.len()].copy_from_slice(&startup);
    program[0x100..0x106].copy_from_slice(&[0x17, 0, 0, 3, 1, 0]); // GDTR at physical 10300h.
    program[0x208..0x210].copy_from_slice(&[0xFF, 0xFF, 0, 4, 1, 0x9A, 0x40, 0]); // Code 10400h.
    program[0x210..0x218].copy_from_slice(&[0xFF, 0x0F, 0, 0, 0, 0x92, 0xC0, 0]); // Flat data, 16 MiB.
    let protected = [
        0xB8, 0x10, 0, 0, 0, 0x8E, 0xC0,
        0xB8, 1, 1, 1, 1,
        0xB9, 0x80, 0x3E, 0, 0,
        0xBF, 0, 0, 0x0A, 0, 0xFC, 0xF3, 0xAB, 0xFA, 0xF4,
    ];
    program[0x300..0x300 + protected.len()].copy_from_slice(&protected);
    program
}
