use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Access, Cpu, CpuError, Gpr16, Gpr32, SegmentRegister, flags};

struct Bus {
    ram: Box<[u8]>,
}

#[test]
fn enter_dword_builds_nested_frames_and_leave_restores_stack() {
    for nesting in [0_u8, 1, 3, 35] {
        let (mut cpu, mut bus) = Bus::setup();
        Bus::enter(&mut cpu, &mut bus, true);
        cpu.load_segment(&mut bus, SegmentRegister::Ss, 0x20)
            .unwrap();
        cpu.registers.set32(Gpr32::Esp, 0x38000);
        cpu.registers.set32(Gpr32::Ebp, 0x37000);
        bus.write_u32(0x36FFC, 0x11223344).unwrap();
        bus.write_u32(0x36FF8, 0x55667788).unwrap();
        bus.ram[0x30020..0x30025].copy_from_slice(&[0xC8, 16, 0, nesting, 0xC9]);
        let flags = cpu.flags;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.registers.get32(Gpr32::Ebp), 0x37FFC);
        let count = if nesting & 31 == 0 {
            1
        } else {
            u32::from(nesting & 31) + 1
        };
        assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x38000 - count * 4 - 16);
        assert_eq!(bus.read_u32(0x37FFC).unwrap(), 0x37000);
        if nesting & 31 == 3 {
            assert_eq!(bus.read_u32(0x37FF8).unwrap(), 0x11223344);
            assert_eq!(bus.read_u32(0x37FF4).unwrap(), 0x55667788);
            assert_eq!(bus.read_u32(0x37FF0).unwrap(), 0x37FFC);
        }
        assert_eq!(cpu.flags, flags);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.registers.get32(Gpr32::Ebp), 0x37000);
        assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x38000);
    }
}

#[test]
fn double_shift_in_dword_code_honors_the_word_operand_override() {
    for (code, expected) in [
        (&[0x0F, 0xAC, 0xD0, 8][..], 0xDD12_3456),
        (&[0x66, 0x0F, 0xAC, 0xD0, 8][..], 0x1234_DD56),
    ] {
        let (mut cpu, mut bus) = Bus::setup();
        Bus::enter(&mut cpu, &mut bus, true);
        cpu.registers.set32(Gpr32::Eax, 0x1234_5678);
        cpu.registers.set32(Gpr32::Edx, 0xAABB_CCDD);
        bus.ram[0x30020..0x30020 + code.len()].copy_from_slice(code);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.registers.get32(Gpr32::Eax), expected);
    }
}

#[test]
fn xlat_in_32_bit_code_uses_ebx_and_checks_segment_limits() {
    for (code, offset) in [(&[0xD7][..], 0x200DE), (&[0x67, 0xD7][..], 0xDE)] {
        let (mut cpu, mut bus) = Bus::setup();
        Bus::enter(&mut cpu, &mut bus, true);
        cpu.load_segment(&mut bus, SegmentRegister::Ds, 0x20)
            .unwrap();
        cpu.registers.set32(Gpr32::Ebx, 0x1FFF0);
        cpu.registers.set32(Gpr32::Eax, 0x1234_ABEE);
        bus.ram[0x30020..0x30020 + code.len()].copy_from_slice(code);
        bus.ram[offset] = 0x57;
        let before = cpu.flags;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x1234_AB57);
        assert_eq!(cpu.flags, before);
    }
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, true);
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 0x10)
        .unwrap();
    cpu.registers.set32(Gpr32::Ebx, 0x1FFF0);
    cpu.registers.set32(Gpr32::Eax, 0x1234_ABEE);
    bus.ram[0x30020] = 0xD7;
    assert!(matches!(
        cpu.step(&mut bus),
        Err(CpuError::SegmentAccess {
            segment: SegmentRegister::Ds,
            offset: 0x200DE,
            access: Access::Read,
            ..
        })
    ));
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x1234_ABEE);
}
impl Bus {
    fn new() -> Self {
        Self {
            ram: vec![0; 4 * 1024 * 1024].into_boxed_slice(),
        }
    }
    fn descriptor(&mut self, selector: u16, base: u32, limit: u32, access: u8, attributes: u8) {
        let offset = 0x800 + usize::from(selector & !7);
        self.ram[offset..offset + 8].copy_from_slice(&[
            limit as u8,
            (limit >> 8) as u8,
            base as u8,
            (base >> 8) as u8,
            (base >> 16) as u8,
            access,
            ((limit >> 16) as u8 & 15) | attributes,
            (base >> 24) as u8,
        ]);
    }
    fn setup() -> (Cpu, Self) {
        let mut bus = Self::new();
        bus.descriptor(8, 0, 0xFFFF, 0x9A, 0);
        bus.descriptor(0x10, 0x10000, 0xFFFF, 0x92, 0);
        bus.descriptor(0x18, 0x20000, 0x1FFFF, 0x9A, 0x40);
        bus.descriptor(0x20, 0, 0x3FFFF, 0x92, 0x40);
        let mut cpu = Cpu::default();
        cpu.control.gdtr.base = 0x800;
        cpu.control.gdtr.limit = 0x7F;
        (cpu, bus)
    }
    fn enter(cpu: &mut Cpu, bus: &mut Self, wide: bool) {
        let code: &[u8] = if wide {
            &[
                0xB8, 0x15, 0, 0x0F, 0x01, 0xF0, 0x90, 0x66, 0xEA, 0x20, 0, 1, 0, 0x18, 0,
            ]
        } else {
            &[0xB8, 0x15, 0, 0x0F, 0x01, 0xF0, 0x90, 0xEA, 0x20, 0, 8, 0]
        };
        bus.ram[..code.len()].copy_from_slice(code);
        for _ in 0..4 {
            cpu.step(bus).unwrap();
        }
    }
}
impl MemoryBus for Bus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.ram
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        *self
            .ram
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))? = value;
        Ok(())
    }
}
impl PortBus for Bus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

#[test]
fn descriptor_reload_changes_the_base_and_sets_the_accessed_bit() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, false);
    bus.ram[0x20..0x28].copy_from_slice(&[0xB8, 0x10, 0, 0x8E, 0xD8, 0xA1, 0, 1]);
    bus.write_u16(0x10100, 0xBEEF).unwrap();
    for _ in 0..3 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0xBEEF);
    assert_eq!(
        cpu.registers.segment_cache(SegmentRegister::Ds).base,
        0x10000
    );
    assert_eq!(bus.ram[0x815], 0x93);
}

#[test]
fn default_dword_code_uses_eip_above_64k_and_66_selects_a_word_operand() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, true);
    assert_eq!((cpu.registers.cs, cpu.registers.ip), (0x18, 0x10020));
    bus.ram[0x30020..0x30029]
        .copy_from_slice(&[0xB8, 0x44, 0x33, 0x22, 0x11, 0x66, 0xB8, 0xEF, 0xBE]);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x11223344);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x1122BEEF);
    assert_eq!(cpu.registers.ip, 0x10029);
}

#[test]
fn sib_addressing_and_67_word_addresses_use_the_same_cached_data_base() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, true);
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 0x10)
        .unwrap();
    cpu.registers.set32(Gpr32::Ebx, 0x110);
    cpu.registers.set32(Gpr32::Ecx, 3);
    cpu.registers.set32(Gpr32::Esi, 3);
    bus.ram[0x30020..0x30028].copy_from_slice(&[0x8B, 0x44, 0x8B, 0xFC, 0x67, 0x8B, 0x40, 0xFC]);
    bus.ram[0x10118..0x1011C].copy_from_slice(&0x12345678u32.to_le_bytes());
    bus.ram[0x1010F..0x10113].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x12345678);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0xDEADBEEF);
}

#[test]
fn null_readonly_and_expand_down_data_segments_are_enforced() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, false);
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 0).unwrap();
    assert!(matches!(
        cpu.registers
            .address(SegmentRegister::Ds, 0, 1, Access::Read),
        Err(CpuError::SegmentAccess { selector: 0, .. })
    ));
    bus.descriptor(0x28, 0x10000, 0xFFFF, 0x90, 0);
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 0x28)
        .unwrap();
    assert_eq!(
        cpu.registers
            .address(SegmentRegister::Ds, 1, 1, Access::Read),
        Ok(0x10001)
    );
    assert!(
        cpu.registers
            .address(SegmentRegister::Ds, 1, 1, Access::Write)
            .is_err()
    );
    bus.descriptor(0x30, 0x10000, 0x100, 0x96, 0);
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 0x30)
        .unwrap();
    assert!(
        cpu.registers
            .address(SegmentRegister::Ds, 0x100, 1, Access::Read)
            .is_err()
    );
    assert_eq!(
        cpu.registers
            .address(SegmentRegister::Ds, 0x101, 1, Access::Read),
        Ok(0x10101)
    );
    assert!(
        cpu.registers
            .address(SegmentRegister::Ds, 0xFFFF, 2, Access::Read)
            .is_err()
    );
}

#[test]
fn word_pushes_and_pops_in_a_dword_stack_keep_the_full_esp() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, true);
    cpu.load_segment(&mut bus, SegmentRegister::Ss, 0x20)
        .unwrap();
    cpu.registers.set32(Gpr32::Esp, 0x2F000);
    cpu.registers.set32(Gpr32::Eax, 0xDEADBEEF);
    cpu.registers.set32(Gpr32::Ebx, 0x11223344);
    bus.ram[0x30020..0x30024].copy_from_slice(&[0x66, 0x50, 0x66, 0x5B]);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x2EFFE);
    assert_eq!(bus.read_u16(0x2EFFE).unwrap(), 0xBEEF);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x2F000);
    assert_eq!(cpu.registers.get32(Gpr32::Ebx), 0x1122BEEF);
}

#[test]
fn interrupt_gate_and_iretd_preserve_the_full_return_offset_and_flags() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, true);
    cpu.load_segment(&mut bus, SegmentRegister::Ss, 0x20)
        .unwrap();
    cpu.registers.set32(Gpr32::Esp, 0x2F000);
    cpu.control.idtr.base = 0x1000;
    cpu.control.idtr.limit = 0x7FF;
    bus.ram[0x1200..0x1208].copy_from_slice(&[0, 0x10, 0x18, 0, 0, 0x8E, 1, 0]);
    bus.ram[0x30020..0x30022].copy_from_slice(&[0xCD, 0x40]);
    bus.ram[0x31000..0x31006].copy_from_slice(&[0xB8, 0x0C, 0xB0, 0xCE, 0xFA, 0xCF]);
    let original_flags = cpu.flags;
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 0x11000);
    assert_eq!(cpu.flags & flags::INTERRUPT, 0);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 0x10022);
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0xFACEB00C);
    assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x2F000);
    assert_eq!(cpu.flags, original_flags);
}

#[test]
fn a_word_near_jump_wraps_its_target_before_checking_the_code_limit() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, false);
    cpu.registers.ip = 0x20;
    bus.ram[0x20..0x23].copy_from_slice(&[0xE9, 0, 0x80]);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 0x8023);
}

#[test]
fn lar_and_lsl_report_nonpresent_descriptors_and_leave_destinations_on_invalid_selectors() {
    let (mut cpu, mut bus) = Bus::setup();
    Bus::enter(&mut cpu, &mut bus, false);
    bus.descriptor(0x28, 0, 0x12345, 0x12, 0x40); // Present flag clear, still queryable.
    cpu.registers.set(Gpr16::Ax, 0x28);
    bus.ram[0x20..0x2A]
        .copy_from_slice(&[0x66, 0x0F, 0x02, 0xC8, 0x66, 0x0F, 0x03, 0xD0, 0x0F, 0x02]);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Ecx), 0x401200);
    assert_ne!(cpu.flags & flags::ZERO, 0);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Edx), 0x12345);
    cpu.registers.set(Gpr16::Ax, 0);
    bus.ram[0x2A] = 0xC8;
    let before = cpu.registers.get32(Gpr32::Ecx);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.flags & flags::ZERO, 0);
    assert_eq!(cpu.registers.get32(Gpr32::Ecx), before);
}
