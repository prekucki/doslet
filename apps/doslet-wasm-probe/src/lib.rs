//! Browser acceptance host. A flat CPU fixture, not the DOS/PC game runtime.
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};
use doslet_x86_wasm::{
    Translation,
    host::{CpuRuntime, Host, Runtime},
};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

struct Memory(Vec<u8>);
impl MemoryBus for Memory {
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
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        self.0
            .get(address as usize..address as usize + usize::from(length))
    }
}
impl PortBus for Memory {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Ok(port as u8)
    }
    fn write_port_u8(&mut self, _port: u16, _value: u8) -> Result<(), BusError> {
        Ok(())
    }
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct CpuProbe {
    host: Host<CpuRuntime<Memory>>,
    abi: Box<[u8]>,
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl CpuProbe {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(profile: &[u8], code: &[u8]) -> Result<Self, String> {
        if code.len() > 0xe000 {
            return Err("fixture code exceeds memory".into());
        }
        let translation = Translation::from_bytes(profile).map_err(str::to_owned)?;
        let mut bus = Memory(vec![0; 65536]);
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
        cpu.load_segment(&mut bus, SegmentRegister::Cs, 8)
            .map_err(|e| e.to_string())?;
        cpu.load_segment(&mut bus, SegmentRegister::Ds, 16)
            .map_err(|e| e.to_string())?;
        cpu.registers.ip = 0x1000;
        cpu.flags = 0x203;
        Ok(Self {
            host: Host::new(
                translation,
                CpuRuntime {
                    cpu,
                    bus,
                    translated_limit: doslet_x86_wasm::MAX_QUANTUM,
                },
            ),
            abi: vec![0; doslet_x86_wasm::STATE_BYTES].into_boxed_slice(),
        })
    }
    pub fn module_bytes(&self) -> Vec<u8> {
        self.host.translation().wasm()
    }
    pub fn abi_ptr(&mut self) -> usize {
        self.abi.as_mut_ptr() as usize
    }
    pub fn abi_len(&self) -> usize {
        self.abi.len()
    }
    pub fn poll(&mut self, completed: u32, remaining: u32) -> Result<i32, String> {
        self.host
            .poll(completed, remaining, &mut self.abi)
            .map_err(|e| format!("{e:?}"))
    }
    pub fn reference_run(&mut self, budget: u32) -> Result<u32, String> {
        let rt = self.host.runtime_mut();
        let mut completed = 0;
        while completed < budget && !rt.cpu.is_halted() {
            rt.interpret().map_err(|e| e.to_string())?;
            completed += 1;
        }
        Ok(completed)
    }
    pub fn ip(&self) -> u32 {
        self.host.runtime().cpu.registers.ip
    }
    pub fn flags(&self) -> u32 {
        self.host.runtime().cpu.flags
    }
    pub fn register(&self, index: u8) -> Result<u32, String> {
        let regs = [
            Gpr32::Eax,
            Gpr32::Ecx,
            Gpr32::Edx,
            Gpr32::Ebx,
            Gpr32::Esp,
            Gpr32::Ebp,
            Gpr32::Esi,
            Gpr32::Edi,
        ];
        let reg = regs.get(index as usize).ok_or("invalid register")?;
        Ok(self.host.runtime().cpu.registers.get32(*reg))
    }
    pub fn memory_hash(&self) -> u32 {
        self.host
            .runtime()
            .bus
            .0
            .iter()
            .fold(2166136261u32, |h, b| {
                (h ^ u32::from(*b)).wrapping_mul(16777619)
            })
    }
    pub fn translated_steps(&self) -> u64 {
        self.host.stats().translated_steps
    }
    pub fn interpreted_steps(&self) -> u64 {
        self.host.stats().interpreted_steps
    }
    pub fn polls(&self) -> u64 {
        self.host.stats().polls
    }
}
