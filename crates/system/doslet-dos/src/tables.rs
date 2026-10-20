//! Guest-visible DOS 5 kernel tables: a List of Lists subset, the NUL and CON
//! device headers, one system file table (SFT) block and the process's job
//! file table (JFT). The runtime owns open files; these tables mirror them for
//! programs that read DOS memory directly, such as DJGPP's `fstat()`.
use crate::files::HANDLE_COUNT;
use doslet_bus::{BusError, MemoryBus};
use doslet_x86::real_address;

/// Kernel data sits below the first MCB, as with DOS=LOW.
pub const SEGMENT: u16 = 0x0080;
/// INT 21h/52h returns SEGMENT:LIST_OF_LISTS; the first MCB is at BX-2.
pub const LIST_OF_LISTS: u16 = 0x0010;
const NUL_HEADER: u16 = LIST_OF_LISTS + 0x22;
const CON_HEADER: u16 = 0x0050;
// Strategy and interrupt entries of both headers; nothing calls them.
const DEVICE_RETURN: u16 = CON_HEADER + 18;
const JFT: u16 = 0x0070;
const SFT: u16 = 0x00E0;
/// The DOS 4.0–6.x entry layout; INT 21h/30h reports 5.0.
const SFT_ENTRY: u16 = 0x3B;
/// SFT entry 0 is CON, shared by handles 0–2. Open descriptions follow.
pub const FIRST_FILE: usize = 1;
const SFT_ENTRIES: usize = FIRST_FILE + HANDLE_COUNT;
/// Handles 0–4 are reserved and files are numbered from 5, so the table
/// exceeds the PSP's own 20 entries, as after INT 21h/67h.
const JFT_SIZE: u16 = 5 + HANDLE_COUNT as u16;
const PSP_JFT_SIZE: u16 = 20;
const CLOSED: u8 = 0xFF;
/// CON's device information word, as IOCTL 4400h reports it.
pub const CON_DEVICE_INFO: u16 = 0x80D3;

/// One SFT entry. Fields DOS keeps for SHARE, networks and IFS stay zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SftEntry {
    /// Handles referring to the entry; zero marks it free.
    pub references: u16,
    pub mode: u16,
    pub attributes: u8,
    pub device_info: u16,
    /// Device header or DPB as segment:offset; zero when absent.
    pub driver: (u16, u16),
    pub cluster: u16,
    pub time: u16,
    pub date: u16,
    pub size: u32,
    pub position: u32,
    pub directory_sector: u32,
    pub directory_entry: u8,
    pub name: [u8; 11],
    pub owner: u16,
}

fn address(offset: u16) -> u32 {
    real_address(SEGMENT, offset)
}

fn write_far<B: MemoryBus>(
    bus: &mut B,
    at: u32,
    (segment, offset): (u16, u16),
) -> Result<(), BusError> {
    bus.write_u16(at, offset)?;
    bus.write_u16(at + 2, segment)
}

fn write_device<B: MemoryBus>(
    bus: &mut B,
    header: u16,
    next: (u16, u16),
    attributes: u16,
    name: &[u8; 8],
) -> Result<(), BusError> {
    let at = address(header);
    write_far(bus, at, next)?;
    bus.write_u16(at + 4, attributes)?;
    bus.write_u16(at + 6, DEVICE_RETURN)?;
    bus.write_u16(at + 8, DEVICE_RETURN)?;
    for (index, byte) in name.iter().enumerate() {
        bus.write_u8(at + 10 + index as u32, *byte)?;
    }
    Ok(())
}

/// Write the tables for a process whose PSP is at `psp`, with every file
/// handle closed. Absent structures, such as DPBs, CDS and disk buffers,
/// have null pointers.
pub fn install<B: MemoryBus>(bus: &mut B, psp: u16) -> Result<(), BusError> {
    for offset in 0..SFT + 6 + SFT_ENTRY * SFT_ENTRIES as u16 {
        bus.write_u8(address(offset), 0)?;
    }
    let lol = address(LIST_OF_LISTS);
    write_far(bus, lol, (0xFFFF, 0xFFFF))?; // No DPB chain.
    write_far(bus, lol + 4, (SEGMENT, SFT))?;
    write_far(bus, lol + 0x0C, (SEGMENT, CON_HEADER))?;
    bus.write_u16(lol + 0x10, 512)?;
    bus.write_u8(lol + 0x20, 3)?; // Block devices A:, B: and C:.
    write_device(bus, NUL_HEADER, (SEGMENT, CON_HEADER), 0x8004, b"NUL     ")?;
    write_device(bus, CON_HEADER, (0xFFFF, 0xFFFF), 0x8013, b"CON     ")?;
    bus.write_u8(address(DEVICE_RETURN), 0xCB)?;
    write_far(bus, address(SFT), (0xFFFF, 0xFFFF))?;
    bus.write_u16(address(SFT + 4), SFT_ENTRIES as u16)?;
    write_entry(
        bus,
        0,
        &SftEntry {
            references: 3,
            mode: 2,
            device_info: CON_DEVICE_INFO,
            driver: (SEGMENT, CON_HEADER),
            name: *b"CON        ",
            owner: psp,
            ..SftEntry::default()
        },
    )?;
    for handle in 0..JFT_SIZE {
        set_handle(bus, psp, handle, (handle < 3).then_some(0))?;
    }
    let base = real_address(psp, 0);
    bus.write_u16(base + 0x32, JFT_SIZE)?;
    write_far(bus, base + 0x34, (SEGMENT, JFT))
}

/// List of Lists fields fixed only after launch setup.
pub fn set_system<B: MemoryBus>(
    bus: &mut B,
    first_mcb: u16,
    lastdrive: u8,
) -> Result<(), BusError> {
    bus.write_u16(address(LIST_OF_LISTS - 2), first_mcb)?;
    bus.write_u8(address(LIST_OF_LISTS + 0x21), lastdrive)
}

/// Point a handle at an SFT entry, or mark it closed. The PSP's own 20
/// entries are kept too, for readers that predate the JFT pointer.
pub fn set_handle<B: MemoryBus>(
    bus: &mut B,
    psp: u16,
    handle: u16,
    entry: Option<usize>,
) -> Result<(), BusError> {
    assert!(handle < JFT_SIZE && entry.is_none_or(|entry| entry < SFT_ENTRIES));
    let value = entry.map_or(CLOSED, |entry| entry as u8);
    bus.write_u8(address(JFT + handle), value)?;
    if handle < PSP_JFT_SIZE {
        bus.write_u8(real_address(psp, 0x18 + handle), value)?;
    }
    Ok(())
}

pub fn write_entry<B: MemoryBus>(
    bus: &mut B,
    index: usize,
    entry: &SftEntry,
) -> Result<(), BusError> {
    assert!(index < SFT_ENTRIES);
    let at = address(SFT + 6 + SFT_ENTRY * index as u16);
    for offset in 0..u32::from(SFT_ENTRY) {
        bus.write_u8(at + offset, 0)?;
    }
    bus.write_u16(at, entry.references)?;
    bus.write_u16(at + 0x02, entry.mode)?;
    bus.write_u8(at + 0x04, entry.attributes)?;
    bus.write_u16(at + 0x05, entry.device_info)?;
    write_far(bus, at + 0x07, entry.driver)?;
    bus.write_u16(at + 0x0B, entry.cluster)?;
    bus.write_u16(at + 0x0D, entry.time)?;
    bus.write_u16(at + 0x0F, entry.date)?;
    bus.write_u32(at + 0x11, entry.size)?;
    bus.write_u32(at + 0x15, entry.position)?;
    bus.write_u32(at + 0x1B, entry.directory_sector)?;
    bus.write_u8(at + 0x1F, entry.directory_entry)?;
    for (index, byte) in entry.name.iter().enumerate() {
        bus.write_u8(at + 0x20 + index as u32, *byte)?;
    }
    bus.write_u16(at + 0x31, entry.owner)
}
