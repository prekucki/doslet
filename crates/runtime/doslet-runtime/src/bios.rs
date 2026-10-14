use doslet_bus::{BusError, MemoryBus};

pub const SHIM_SEGMENT: u16 = 0xF000;
pub const SHIM_BASE: u32 = 0xF0000;
pub const SERVICES: [u8; 10] = [0x08, 0x10, 0x15, 0x16, 0x1A, 0x20, 0x21, 0x2F, 0x33, 0x67];
pub const UPPERCASE_OFFSET: u16 = 0x100;
pub const VBE_OEM_OFFSET: u16 = 0x200;
pub const VBE_MODES_OFFSET: u16 = 0x210;
pub const VBE_BANK_OFFSET: u16 = 0x220;
pub const INDOS_OFFSET: u16 = 0x240;
pub const IRQ0_OFFSET: u16 = 0x180;
const IRQ0_CODE: [u8; 9] = [0x50, 0xCD, 0x1C, 0xB0, 0x20, 0xE6, 0x20, 0x58, 0xCF];

/// Selected mode-03h BIOS state. B800h is currently ordinary PC RAM;
/// VGA text-memory mapping and glyph scanout remain pending.
pub fn set_text_mode<B: MemoryBus>(bus: &mut B, preserve: bool) -> Result<(), BusError> {
    if !preserve {
        // Eight 4-KiB text pages, including the padding after each 80x25 page.
        for address in (0xB8000..0xC0000).step_by(2) {
            bus.write_u16(address, 0x0720)?;
        }
    }
    bus.write_u8(0x449, 3)?;
    bus.write_u16(0x44A, 80)?;
    bus.write_u16(0x44C, 0x1000)?;
    bus.write_u16(0x44E, 0)?;
    for address in (0x450..0x460).step_by(2) {
        bus.write_u16(address, 0)?;
    }
    bus.write_u16(0x460, 0x0607)?;
    bus.write_u8(0x462, 0)?;
    bus.write_u16(0x463, 0x3D4)?;
    bus.write_u8(0x484, 24)?;
    bus.write_u16(0x485, 16)?;
    bus.write_u8(0x487, 0x60 | if preserve { 0x80 } else { 0 })?;
    Ok(())
}

pub fn service_at<B: MemoryBus>(bus: &mut B, offset: u32) -> Result<Option<u8>, BusError> {
    if offset == u32::from(IRQ0_OFFSET) {
        for (index, byte) in IRQ0_CODE.iter().enumerate() {
            if bus.read_u8(SHIM_BASE + offset + index as u32)? != *byte {
                return Ok(None);
            }
        }
        return Ok(Some(8));
    }
    if let Ok(vector) = u8::try_from(offset)
        && vector != 8
        && SERVICES.contains(&vector)
        && bus.read_u8(SHIM_BASE + offset)? == 0xCF
    {
        return Ok(Some(vector));
    }
    Ok(None)
}

/// Guest interrupts still traverse the IVT. Only arrival at an installed shim invokes HLE.
pub fn install<B: MemoryBus>(bus: &mut B) -> Result<(), BusError> {
    set_text_mode(bus, false)?;
    bus.write_u16(0x41A, 0x1E)?;
    bus.write_u16(0x41C, 0x1E)?;
    bus.write_u16(0x480, 0x1E)?;
    bus.write_u16(0x482, 0x3E)?;
    for vector in SERVICES {
        let offset = if vector == 8 {
            IRQ0_OFFSET
        } else {
            u16::from(vector)
        };
        bus.write_u16(u32::from(vector) * 4, offset)?;
        bus.write_u16(u32::from(vector) * 4 + 2, SHIM_SEGMENT)?;
        bus.write_u8(SHIM_BASE + u32::from(vector), 0xCF)?; // IRET
    }
    for (offset, byte) in IRQ0_CODE.iter().enumerate() {
        bus.write_u8(SHIM_BASE + u32::from(IRQ0_OFFSET) + offset as u32, *byte)?;
    }
    // The default user timer hook is ordinary guest IRET code; replacement
    // handlers execute through their current vector, including extender relays.
    bus.write_u16(0x1C * 4, 0x1C)?;
    bus.write_u16(0x1C * 4 + 2, SHIM_SEGMENT)?;
    bus.write_u8(SHIM_BASE + 0x1C, 0xCF)?;
    // Far-call ABI used by the DOS country record: AL in/out, ASCII uppercase.
    let uppercase = [0x3C, b'a', 0x72, 6, 0x3C, b'z', 0x77, 2, 0x2C, 0x20, 0xCB];
    for (offset, byte) in uppercase.iter().enumerate() {
        bus.write_u8(
            SHIM_BASE + u32::from(UPPERCASE_OFFSET) + offset as u32,
            *byte,
        )?;
    }
    for (start, data) in [
        // Blocking BIOS/DOS input yields in guest STI/HLT loops, so timer IRQs
        // and user hooks continue. Retry the original service after wakeup.
        (0x300, &[0xFB, 0xF4, 0xE9, 0x11, 0xFD][..]), // JMP 0016h.
        (0x310, &[0xFB, 0xF4, 0xE9, 0x0C, 0xFD][..]), // JMP 0021h.
        (VBE_OEM_OFFSET, &b"Doslet VBE\0"[..]),
        (VBE_MODES_OFFSET, &[1, 1, 0xFF, 0xFF][..]),
        // Far-call bank switch ABI: caller supplies BH/BL/DX, AX is scratch.
        (VBE_BANK_OFFSET, &[0xB8, 5, 0x4F, 0xCD, 0x10, 0xCB][..]),
        // No asynchronous/reentrant DOS service path exists yet.
        (INDOS_OFFSET - 1, &[0, 0][..]), // Critical-error flag, InDOS flag.
    ] {
        for (offset, byte) in data.iter().enumerate() {
            bus.write_u8(SHIM_BASE + u32::from(start) + offset as u32, *byte)?;
        }
    }
    Ok(())
}
