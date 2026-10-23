use doslet_bus::{BusError, MemoryBus};

pub const SHIM_SEGMENT: u16 = 0xF000;
pub const SHIM_BASE: u32 = 0xF0000;

pub const UPPERCASE_OFFSET: u16 = 0x100;
pub const VBE_OEM_OFFSET: u16 = 0x200;
pub const VBE_MODES_OFFSET: u16 = 0x210;
pub const VBE_BANK_OFFSET: u16 = 0x220;
pub const INDOS_OFFSET: u16 = 0x240;
pub const IRQ0_OFFSET: u16 = 0x180;
pub const IRQ1_OFFSET: u16 = 0x1A0;
const IRQ1_CODE: [u8; 7] = [0x50, 0xB0, 0x20, 0xE6, 0x20, 0x58, 0xCF];
const IRQ0_CODE: [u8; 9] = [0x50, 0xCD, 0x1C, 0xB0, 0x20, 0xE6, 0x20, 0x58, 0xCF];
/// Default handlers of IRQ 2–15 acknowledge the PIC and return, as a BIOS's
/// do, 16 bytes apart. IRQ 12's vector holds the mouse driver's event wrapper.
const IRQ_DEFAULT_BASE: u16 = 0x500;
const DUMMY_OFFSET: u16 = 1;
const MASTER_IRQ_CODE: [u8; 7] = IRQ1_CODE;
const SLAVE_IRQ_CODE: [u8; 9] = [0x50, 0xB0, 0x20, 0xE6, 0xA0, 0xE6, 0x20, 0x58, 0xCF];

const fn irq_vector(irq: u16) -> u8 {
    if irq < 8 {
        8 + irq as u8
    } else {
        0x70 + irq as u8 - 8
    }
}
fn default_irq_code(irq: u16) -> &'static [u8] {
    if irq < 8 {
        &MASTER_IRQ_CODE
    } else {
        &SLAVE_IRQ_CODE
    }
}

/// The hardware interrupt whose BIOS handler starts at `offset` in the shim
/// segment, with the offset of that handler's final IRET.
pub fn interrupt_handler<B: MemoryBus>(
    bus: &mut B,
    offset: u32,
) -> Result<Option<(u8, u16)>, BusError> {
    let Ok(offset) = u16::try_from(offset) else {
        return Ok(None);
    };
    let (vector, code): (u8, &[u8]) = match offset {
        IRQ0_OFFSET => (8, &IRQ0_CODE),
        IRQ1_OFFSET => (9, &IRQ1_CODE),
        _ => {
            let Some(irq) = offset
                .checked_sub(IRQ_DEFAULT_BASE)
                .filter(|delta| delta % 16 == 0)
                .map(|delta| delta / 16)
                .filter(|irq| (2..16).contains(irq) && *irq != 12)
            else {
                return Ok(None);
            };
            (irq_vector(irq), default_irq_code(irq))
        }
    };
    for (index, byte) in code.iter().enumerate() {
        if bus.read_u8(SHIM_BASE + u32::from(offset) + index as u32)? != *byte {
            return Ok(None);
        }
    }
    Ok(Some((vector, offset + code.len() as u16 - 1)))
}

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

/// Selected CGA 320x200 4-color (04h/05h) or 640x200 2-color (06h) BIOS state at B800h.
pub fn set_cga_mode<B: MemoryBus>(bus: &mut B, mode: u8, preserve: bool) -> Result<(), BusError> {
    if !preserve {
        for address in (0xB8000..0xBC000).step_by(2) {
            bus.write_u16(address, 0)?;
        }
    }
    bus.write_u8(0x449, mode)?;
    bus.write_u16(0x44A, if mode == 6 { 80 } else { 40 })?;
    bus.write_u16(0x44C, 0x4000)?;
    bus.write_u16(0x44E, 0)?;
    for address in (0x450..0x460).step_by(2) {
        bus.write_u16(address, 0)?;
    }
    bus.write_u16(0x460, 0x0607)?;
    bus.write_u8(0x462, 0)?;
    bus.write_u16(0x463, 0x3D4)?;
    bus.write_u8(
        0x465,
        match mode {
            5 => 0x2E,
            6 => 0x1E,
            _ => 0x2A,
        },
    )?;
    bus.write_u8(0x466, if mode == 6 { 0x3F } else { 0x30 })?;
    bus.write_u8(0x484, 24)?;
    bus.write_u16(0x485, 8)?;
    bus.write_u8(0x487, 0x60 | if preserve { 0x80 } else { 0 })?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceSelector {
    DivideOverflow,
    Timer,
    KeyboardIrq,
    Video,
    ExtendedBios,
    Keyboard,
    Clock,
    Terminate,
    Dos,
    TerminateStayResident,
    Mscdex,
    Mouse,
    Ems,
}
impl ServiceSelector {
    pub const fn vector(self) -> u8 {
        match self {
            Self::DivideOverflow => 0,
            Self::Timer => 8,
            Self::KeyboardIrq => 9,
            Self::Video => 0x10,
            Self::ExtendedBios => 0x15,
            Self::Keyboard => 0x16,
            Self::Clock => 0x1A,
            Self::Terminate => 0x20,
            Self::Dos => 0x21,
            Self::TerminateStayResident => 0x27,
            Self::Mscdex => 0x2F,
            Self::Mouse => 0x33,
            Self::Ems => 0x67,
        }
    }
}
struct ServiceDefinition {
    selector: ServiceSelector,
    offset: u16,
    signature: &'static [u8],
}
const SERVICES: [ServiceDefinition; 13] = [
    // DOS's divide-error handler.
    ServiceDefinition {
        selector: ServiceSelector::DivideOverflow,
        offset: 0,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Timer,
        offset: IRQ0_OFFSET,
        signature: &IRQ0_CODE,
    },
    ServiceDefinition {
        selector: ServiceSelector::KeyboardIrq,
        offset: IRQ1_OFFSET,
        signature: &IRQ1_CODE,
    },
    ServiceDefinition {
        selector: ServiceSelector::Video,
        offset: 0x10,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::ExtendedBios,
        offset: 0x15,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Keyboard,
        offset: 0x16,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Clock,
        offset: 0x1A,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Terminate,
        offset: 0x20,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Dos,
        offset: 0x21,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::TerminateStayResident,
        offset: 0x27,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Mscdex,
        offset: 0x2F,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Mouse,
        offset: 0x33,
        signature: &[0xCF],
    },
    ServiceDefinition {
        selector: ServiceSelector::Ems,
        offset: 0x67,
        signature: &[0xCF],
    },
];
pub fn service_at<B: MemoryBus>(
    bus: &mut B,
    offset: u32,
) -> Result<Option<ServiceSelector>, BusError> {
    let Some(service) = SERVICES
        .iter()
        .find(|service| u32::from(service.offset) == offset)
    else {
        return Ok(None);
    };
    for (index, byte) in service.signature.iter().enumerate() {
        if bus.read_u8(SHIM_BASE + offset + index as u32)? != *byte {
            return Ok(None);
        }
    }
    Ok(Some(service.selector))
}

/// Guest interrupts still traverse the IVT. Only arrival at an installed shim invokes HLE.
pub fn install<B: MemoryBus>(bus: &mut B) -> Result<(), BusError> {
    set_text_mode(bus, false)?;
    bus.write_u16(0x41A, 0x1E)?;
    bus.write_u16(0x41C, 0x1E)?;
    bus.write_u16(0x480, 0x1E)?;
    bus.write_u16(0x482, 0x3E)?;
    for service in SERVICES {
        let vector = service.selector.vector();
        bus.write_u16(u32::from(vector) * 4, service.offset)?;
        bus.write_u16(u32::from(vector) * 4 + 2, SHIM_SEGMENT)?;
        for (index, byte) in service.signature.iter().enumerate() {
            bus.write_u8(SHIM_BASE + u32::from(service.offset) + index as u32, *byte)?;
        }
    }
    // Vectors 1-7 return at once, as a BIOS's dummy handler does.
    bus.write_u8(SHIM_BASE + u32::from(DUMMY_OFFSET), 0xCF)?;
    for vector in 1..8u32 {
        bus.write_u16(vector * 4, DUMMY_OFFSET)?;
        bus.write_u16(vector * 4 + 2, SHIM_SEGMENT)?;
    }
    for irq in (2..16).filter(|irq| *irq != 12) {
        let offset = IRQ_DEFAULT_BASE + irq * 16;
        let vector = u32::from(irq_vector(irq));
        bus.write_u16(vector * 4, offset)?;
        bus.write_u16(vector * 4 + 2, SHIM_SEGMENT)?;
        for (index, byte) in default_irq_code(irq).iter().enumerate() {
            bus.write_u8(SHIM_BASE + u32::from(offset) + index as u32, *byte)?;
        }
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
