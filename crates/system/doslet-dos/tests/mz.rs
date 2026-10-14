use doslet_bus::{BusError, MemoryBus};
use doslet_dos::{MzError, ProcessMemory, load_mz, load_overlay};
use doslet_executable::ExecutableError;
use doslet_x86::Gpr16;

include!("../../../../tests/fixtures/relocated_mz.rs");

struct Ram {
    bytes: Vec<u8>,
    writes: usize,
}
impl Ram {
    fn new() -> Self {
        Self {
            bytes: vec![0xCC; 0x20000],
            writes: 0,
        }
    }
}
impl MemoryBus for Ram {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.bytes
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        let target = self
            .bytes
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))?;
        *target = value;
        self.writes += 1;
        Ok(())
    }
}
const MEMORY: ProcessMemory = ProcessMemory {
    psp_segment: 0x1000,
    end_segment: 0x2000,
};
fn word(file: &mut [u8], offset: usize, value: u16) {
    file[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn relocates_segments_initializes_psp_and_clears_extra_memory() {
    let mut ram = Ram::new();
    let registers = load_mz(&mut ram, &relocated_mz(), MEMORY).unwrap();
    assert_eq!(registers.cs, 0x1010);
    assert_eq!(registers.ip, 0);
    assert_eq!(registers.ss, 0x1018);
    assert_eq!(registers.get(Gpr16::Sp), 256);
    assert_eq!((registers.ds, registers.es), (0x1000, 0x1000));
    assert_eq!(ram.read_u16(0x10101).unwrap(), 0x1012);
    assert_eq!(&ram.bytes[0x10000..0x10002], &[0xCD, 0x20]);
    assert_eq!(ram.read_u16(0x10002).unwrap(), 0x1028);
    assert!(ram.bytes[0x10180..0x10280].iter().all(|byte| *byte == 0));
    assert_eq!(ram.bytes[0x10280], 0xCC);
}

#[test]
fn rejects_invalid_files_and_layout_before_writing_ram() {
    let mut ram = Ram::new();
    let cases = [
        (2, 512, MzError::Executable(ExecutableError::InvalidSize)),
        (2, 161, MzError::Executable(ExecutableError::Truncated)),
        (8, 1, MzError::Executable(ExecutableError::InvalidSize)),
        (
            24,
            27,
            MzError::Executable(ExecutableError::InvalidRelocations),
        ),
        (
            24,
            30,
            MzError::Executable(ExecutableError::InvalidRelocations),
        ),
        (
            6,
            2,
            MzError::Executable(ExecutableError::InvalidRelocations),
        ),
        (28, 127, MzError::InvalidRelocation { index: 0 }),
        (30, 0xFFFF, MzError::InvalidRelocation { index: 0 }),
        (22, 8, MzError::InvalidEntry),
        (22, 0xFFFF, MzError::InvalidEntry),
        (20, 128, MzError::InvalidEntry),
        (14, 0xFFFF, MzError::InvalidStack),
        (16, 4, MzError::InvalidStack),
        (16, 258, MzError::InvalidStack),
        (12, 15, MzError::InvalidMemoryRequest),
        (26, 1, MzError::UnsupportedMode),
        (8, 10, MzError::Empty),
    ];
    for (offset, value, expected) in cases {
        let mut file = relocated_mz();
        word(&mut file, offset, value);
        // The empty-image case still needs its relocation table in the header.
        assert_eq!(
            load_mz(&mut ram, &file, MEMORY),
            Err(expected),
            "header offset {offset}"
        );
        assert_eq!(ram.writes, 0);
    }
    assert_eq!(
        load_mz(&mut ram, &relocated_mz()[..159], MEMORY),
        Err(MzError::Executable(ExecutableError::Truncated))
    );
    assert_eq!(
        load_mz(
            &mut ram,
            &relocated_mz(),
            ProcessMemory {
                end_segment: 0x1027,
                ..MEMORY
            }
        ),
        Err(MzError::InsufficientMemory)
    );
    assert_eq!(
        load_mz(
            &mut ram,
            &relocated_mz(),
            ProcessMemory {
                end_segment: 0x0FFF,
                ..MEMORY
            }
        ),
        Err(MzError::InvalidMemoryRequest)
    );
    assert_eq!(ram.writes, 0);
}

#[test]
fn handles_high_loading_unlimited_memory_and_relocation_wrap() {
    let mut ram = Ram::new();
    let mut file = relocated_mz();
    word(&mut file, 10, 0);
    word(&mut file, 12, 0);
    word(&mut file, 14, 0);
    word(&mut file, 16, 128);
    word(&mut file, 33, 0xF000);
    let registers = load_mz(&mut ram, &file, MEMORY).unwrap();
    assert_eq!(registers.cs, 0x1FF8);
    assert_eq!(ram.read_u16(0x1FF81).unwrap(), 0x0FF8);
    assert_eq!(ram.read_u16(0x10002).unwrap(), 0x2000);
    word(&mut file, 10, 16);
    let registers = load_mz(&mut ram, &file, MEMORY).unwrap();
    assert_eq!(registers.cs, 0x1010);
    assert_eq!(ram.read_u16(0x10002).unwrap(), 0x2000);
}

#[test]
fn accepts_nonzero_entries_and_a_wrapped_stack_top() {
    let mut ram = Ram::new();
    let mut file = relocated_mz();
    word(&mut file, 22, 1);
    word(&mut file, 20, 3);
    word(&mut file, 14, 0);
    word(&mut file, 16, 0);
    word(&mut file, 12, 0);
    let registers = load_mz(
        &mut ram,
        &file,
        ProcessMemory {
            end_segment: 0x2010,
            ..MEMORY
        },
    );
    // The bus is deliberately too short for this process: report its failure.
    assert!(matches!(
        registers,
        Err(MzError::Bus(BusError::UnmappedMemory(0x20000)))
    ));
    ram.bytes.resize(0x20100, 0xCC);
    let registers = load_mz(
        &mut ram,
        &file,
        ProcessMemory {
            end_segment: 0x2010,
            ..MEMORY
        },
    )
    .unwrap();
    assert_eq!((registers.cs, registers.ip), (0x1011, 3));
    assert_eq!(registers.get(Gpr16::Sp), 0);
}

#[test]
fn rejects_extended_formats_before_loading_the_stub() {
    for signature in [b"NE", b"PE", b"LE", b"LX"] {
        let mut ram = Ram::new();
        let mut file = relocated_mz();
        word(&mut file, 8, 5);
        word(&mut file, 24, 64);
        file[60..64].copy_from_slice(&128u32.to_le_bytes());
        file[128..130].copy_from_slice(signature);
        assert_eq!(
            load_mz(&mut ram, &file, MEMORY),
            Err(MzError::UnsupportedMode)
        );
        assert_eq!(ram.writes, 0);
    }
}

#[test]
fn overlay_relocation_uses_its_parameter_and_preserves_the_psp() {
    let mut ram = Ram::new();
    let file = relocated_mz();
    load_overlay(&mut ram, &file, 0x1100, 0x2200, 0x2000).unwrap();
    assert_eq!(ram.read_u16(0x11001).unwrap(), 0x2202);
    assert_eq!(ram.bytes[0x10000], 0xCC);
    assert_eq!(ram.bytes[0x11080], 0xCC);
    let writes = ram.writes;
    assert_eq!(
        load_overlay(&mut ram, &file, 0x1100, 0x2200, 0x1107),
        Err(MzError::InsufficientMemory)
    );
    assert_eq!(ram.writes, writes);
}

#[test]
fn fcb_parsing_handles_blanks_wildcards_and_preservation() {
    let mut ram = Ram::new();
    ram.bytes[0x10200..0x10200 + 17].copy_from_slice(b" , C:foo*.b? \r\0xx");
    let result = doslet_dos::fcb::parse(&mut ram, (0x1000, 0x200), (0x1000, 0x300), 1)
        .unwrap()
        .unwrap();
    assert_eq!(result, (1, 12));
    assert_eq!(&ram.bytes[0x10300..0x1030C], b"\x03FOO?????B? ");
    ram.bytes[0x10200] = b'\r';
    ram.bytes[0x10201] = 0;
    doslet_dos::fcb::parse(&mut ram, (0x1000, 0x200), (0x1000, 0x300), 14).unwrap();
    assert_eq!(&ram.bytes[0x10300..0x1030C], b"\x03FOO?????B? ");
    doslet_dos::fcb::parse(&mut ram, (0x1000, 0x200), (0x1000, 0x300), 0).unwrap();
    assert_eq!(&ram.bytes[0x10300..0x1030C], b"\0           ");
}
