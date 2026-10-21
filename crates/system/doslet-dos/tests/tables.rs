use doslet_bus::{BusError, MemoryBus};
use doslet_dos::tables::{self, SftEntry};

struct Ram(Vec<u8>);
impl MemoryBus for Ram {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.0
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        *self
            .0
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))? = value;
        Ok(())
    }
}
const PSP: u16 = 0x1000;

fn far(ram: &mut Ram, at: u32) -> u32 {
    let offset = ram.read_u16(at).unwrap();
    let segment = ram.read_u16(at + 2).unwrap();
    u32::from(segment) * 16 + u32::from(offset)
}

/// Locate a handle's SFT entry the way DJGPP's fstat() does: the JFT through
/// PSP:34h, then the SFT chain from List of Lists+4.
fn sft_entry(ram: &mut Ram, handle: u16) -> Option<u32> {
    let psp = u32::from(PSP) * 16;
    if handle >= ram.read_u16(psp + 0x32).unwrap() {
        return None;
    }
    let jft = far(ram, psp + 0x34);
    let mut index = u16::from(ram.read_u8(jft + u32::from(handle)).unwrap());
    if index == 0xFF {
        return None;
    }
    let lol = u32::from(tables::SEGMENT) * 16 + u32::from(tables::LIST_OF_LISTS);
    let mut block = far(ram, lol + 4);
    loop {
        let count = ram.read_u16(block + 4).unwrap();
        if index < count {
            return Some(block + 6 + u32::from(index) * 0x3B);
        }
        index -= count;
        if ram.read_u16(block).unwrap() == 0xFFFF {
            return None;
        }
        block = far(ram, block);
    }
}

#[test]
fn file_handles_resolve_through_jft_and_sft_chain() {
    let mut ram = Ram(vec![0xCC; 0x20000]);
    tables::install(&mut ram, PSP).unwrap();
    tables::set_system(&mut ram, 0x0EFF, 5).unwrap();
    let lol = u32::from(tables::SEGMENT) * 16 + u32::from(tables::LIST_OF_LISTS);
    assert_eq!(ram.read_u16(lol - 2).unwrap(), 0x0EFF);
    assert_eq!(ram.read_u8(lol + 0x21).unwrap(), 5);
    // NUL heads the device chain and links to CON.
    let con = far(&mut ram, lol + 0x22);
    assert_eq!(
        &ram.0[lol as usize + 0x2C..lol as usize + 0x34],
        b"NUL     "
    );
    assert_eq!(&ram.0[con as usize + 10..con as usize + 18], b"CON     ");
    assert_eq!(far(&mut ram, lol + 0x0C), con);

    for handle in 0..3 {
        let entry = sft_entry(&mut ram, handle).unwrap() as usize;
        assert_eq!(ram.0[entry], 3);
        assert_eq!(
            u16::from_le_bytes([ram.0[entry + 5], ram.0[entry + 6]]),
            tables::CON_DEVICE_INFO
        );
        assert_eq!(&ram.0[entry + 0x20..entry + 0x2B], b"CON        ");
    }
    assert_eq!(sft_entry(&mut ram, 3), None);
    assert_eq!(sft_entry(&mut ram, 100), None);
    assert_eq!(sft_entry(&mut ram, 101), None);

    let file = SftEntry {
        references: 2,
        mode: 0x40,
        attributes: 0x20,
        device_info: 2,
        cluster: 9,
        time: 0x6000,
        date: 0x5D4E,
        size: 0x0123_4567,
        position: 0x89AB,
        directory_sector: 1,
        directory_entry: 7,
        name: *b"PAK0    PAK",
        owner: PSP,
        ..SftEntry::default()
    };
    tables::write_entry(&mut ram, tables::FIRST_FILE + 95, &file).unwrap();
    tables::set_handle(&mut ram, PSP, 6, Some(tables::FIRST_FILE + 95)).unwrap();
    tables::set_handle(&mut ram, PSP, 100, Some(tables::FIRST_FILE + 95)).unwrap();
    let entry = sft_entry(&mut ram, 100).unwrap() as usize;
    assert_eq!(sft_entry(&mut ram, 6), Some(entry as u32));
    // The PSP's own 20 entries follow the low handles.
    assert_eq!(ram.0[u32::from(PSP) as usize * 16 + 0x18 + 6], 96);
    let field = |at: usize, len: usize| &ram.0[entry + at..entry + at + len];
    assert_eq!(field(0, 2), [2, 0]);
    assert_eq!(field(2, 2), [0x40, 0]);
    assert_eq!(field(4, 1), [0x20]);
    assert_eq!(field(5, 2), [2, 0]);
    assert_eq!(field(0x07, 4), [0; 4]);
    assert_eq!(field(0x0B, 2), [9, 0]);
    assert_eq!(field(0x0D, 4), [0x00, 0x60, 0x4E, 0x5D]);
    assert_eq!(field(0x11, 4), 0x0123_4567u32.to_le_bytes());
    assert_eq!(field(0x15, 4), 0x89ABu32.to_le_bytes());
    assert_eq!(field(0x1B, 5), [1, 0, 0, 0, 7]);
    assert_eq!(field(0x20, 11), b"PAK0    PAK");
    assert_eq!(field(0x31, 2), PSP.to_le_bytes());
    assert_eq!(field(0x2B, 6), [0; 6]);
    assert_eq!(field(0x33, 8), [0; 8]);

    tables::set_handle(&mut ram, PSP, 6, None).unwrap();
    tables::write_entry(&mut ram, tables::FIRST_FILE + 95, &SftEntry::default()).unwrap();
    assert_eq!(sft_entry(&mut ram, 6), None);
    assert_eq!(ram.0[u32::from(PSP) as usize * 16 + 0x18 + 6], 0xFF);
    let entry = sft_entry(&mut ram, 100).unwrap() as usize;
    assert_eq!(&ram.0[entry..entry + 0x3B], [0; 0x3B]);
}
