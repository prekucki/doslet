use super::*;
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, SegmentRegister};
use doslet_x86_wasm::{Translation, host::CpuRuntime};

struct Bus(Vec<u8>);
impl MemoryBus for Bus {
    fn read_u8(&mut self, a: u32) -> Result<u8, BusError> {
        self.0
            .get(a as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(a))
    }
    fn write_u8(&mut self, a: u32, b: u8) -> Result<(), BusError> {
        *self
            .0
            .get_mut(a as usize)
            .ok_or(BusError::UnmappedMemory(a))? = b;
        Ok(())
    }
    fn code_bytes(&self, a: u32, l: u8) -> Option<&[u8]> {
        self.0.get(a as usize..a as usize + usize::from(l))
    }
}
impl PortBus for Bus {
    fn read_port_u8(&mut self, p: u16) -> Result<u8, BusError> {
        Ok(p as u8)
    }
    fn write_port_u8(&mut self, _p: u16, _v: u8) -> Result<(), BusError> {
        Ok(())
    }
}
fn translation(code: &[u8]) -> Translation {
    let mut p = b"DSJ1\x01\0".to_vec();
    p.extend_from_slice(&0x1000_u32.to_le_bytes());
    p.push(code.len() as u8);
    p.extend_from_slice(code);
    Translation::from_profile(&p).unwrap()
}
fn runtime(code: &[u8]) -> CpuRuntime<Bus> {
    let mut bus = Bus(vec![0; 65536]);
    bus.0[0x1000..0x1000 + code.len()].copy_from_slice(code);
    bus.0[0x208..0x210].copy_from_slice(&[255, 255, 0, 0, 0, 0x9a, 0xcf, 0]);
    bus.0[0x210..0x218].copy_from_slice(&[255, 255, 0, 0, 0, 0x92, 0xcf, 0]);
    let mut cpu = Cpu::default();
    cpu.set_dynamic_core(false);
    cpu.set_decode_cache(false);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 23,
    };
    cpu.set_protected_mode(true);
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 16).unwrap();
    cpu.registers.ip = 0x1000;
    cpu.flags = 0x203;
    CpuRuntime {
        cpu,
        bus,
        translated_limit: 4096,
    }
}
#[test]
fn aot_and_fallback_match_reference_before_and_after_guest_reset() {
    let code = [0x83, 0xc0, 1, 0xec, 0x43, 0xeb, 0xf9];
    let artifact = Artifact::compile(&translation(&code).wasm()).unwrap();
    assert!(!artifact.serialized().is_empty());
    let mut runner = artifact
        .instantiate(Host::new(translation(&code), runtime(&code)))
        .unwrap();
    for _ in 0..2 {
        runner.host_mut().replace_runtime(runtime(&code)).unwrap();
        let mut reference = runtime(&code);
        assert_eq!(runner.run(4000).unwrap(), 4000);
        for _ in 0..4000 {
            reference.cpu.step(&mut reference.bus).unwrap();
        }
        assert!(
            runner
                .host()
                .runtime()
                .cpu
                .matches(&reference.cpu.snapshot())
        );
        assert_eq!(runner.host().runtime().bus.0, reference.bus.0);
        assert_eq!(runner.host().stats().translated_steps, 3000);
        assert_eq!(runner.host().stats().interpreted_steps, 1000);
    }
}
#[test]
fn aot_requires_exact_matching_host_profile_and_has_independent_instances() {
    let code = [0x40, 0xeb, 0xfd];
    let artifact = Artifact::compile(&translation(&code).wasm()).unwrap();
    assert!(
        artifact
            .instantiate(Host::new(translation(&[0x48, 0xeb, 0xfd]), runtime(&code)))
            .is_err()
    );
    let mut a = artifact
        .instantiate(Host::new(translation(&code), runtime(&code)))
        .unwrap();
    let mut b = artifact
        .instantiate(Host::new(translation(&code), runtime(&code)))
        .unwrap();
    a.run(4000).unwrap();
    assert_eq!(a.host().stats().translated_steps, 4000);
    assert_eq!(b.host().stats().translated_steps, 0);
    b.run(4000).unwrap();
    assert!(
        a.host()
            .runtime()
            .cpu
            .matches(&b.host().runtime().cpu.snapshot())
    );
}
#[test]
fn aot_runtime_error_does_not_replay_faulting_guest_code() {
    let code = [0x0f, 0xff];
    let artifact = Artifact::compile(&translation(&code).wasm()).unwrap();
    let mut runner = artifact
        .instantiate(Host::new(translation(&code), runtime(&code)))
        .unwrap();
    assert!(runner.run(10).is_err());
    let polls = runner.host().stats().polls;
    assert!(runner.run(10).is_err());
    assert_eq!(runner.host().stats().polls, polls);
}
