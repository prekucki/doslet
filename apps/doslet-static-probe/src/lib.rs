//! Synthetic acceptance fixture: the actual Rust CPU and a static program share
//! one WASM memory and a synchronous WASM-to-WASM callback. No WASI/JS imports.
//!
//! This register-only loop tests aliases and flags, not games or device timing.
//! Setup allocates the CPU/bus once; execution neither clones RAM nor allocates.

use doslet_bus::{BusError, CodeCheck, CodeOwnerId, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister, StepOutcome};
use doslet_x86_wasm::static_program::{
    Accounting, Address, Alu, Condition, Frame, Operation, Program, Region, RegionId, Register,
    Source, Terminator, abi,
};

pub mod x87;

pub const START_IP: u32 = 0x1000;
pub const END_OFFSET: u32 = 10;
pub const INSTRUCTIONS_PER_ITERATION: u32 = 5;
pub const MAX_ITERATIONS: u32 = 2_000_000;
pub const SERVICE_INTERVAL: u32 = 64;
// Protected-mode 32-bit code: INC EAX; ADD DX,3; XOR BH,AL; DEC ECX; JNZ -10.
// HLT marks the endpoint and is deliberately never executed.
pub const CODE: [u8; 11] = [
    0x40, 0x66, 0x83, 0xc2, 3, 0x30, 0xc7, 0x49, 0x75, 0xf6, 0xf4,
];
pub const INITIAL_GPR: [u32; 8] = [
    0x8765_fedc,
    0,
    0xabcd_fffd,
    0x7654_a512,
    0x9000,
    0x1357_2468,
    0x89ab_cdef,
    0xfedc_ba98,
];
const GPRS: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];

/// Explicit fixture lowering, independent of executable parsing/profiles.
pub fn fixture_program(accounting: Accounting) -> Program {
    Program {
        regions: vec![
            Region {
                id: RegionId(0),
                entry: Address {
                    space: 1,
                    offset: 0,
                },
                operations: vec![
                    Operation::Inc(Register::Eax),
                    Operation::Alu {
                        kind: Alu::Add,
                        target: Register::Dx,
                        source: Source::Immediate(3),
                    },
                    Operation::Alu {
                        kind: Alu::Xor,
                        target: Register::Bh,
                        source: Source::Register(Register::Al),
                    },
                    Operation::Dec(Register::Ecx),
                ],
                terminator: Terminator::Branch {
                    condition: Condition::NotEqual,
                    taken: RegionId(0),
                    not_taken: RegionId(1),
                },
                accounting,
            },
            Region {
                id: RegionId(1),
                entry: Address {
                    space: 1,
                    offset: END_OFFSET,
                },
                operations: vec![],
                terminator: Terminator::Fallback(Address {
                    space: 1,
                    offset: END_OFFSET,
                }),
                accounting: Accounting::Counted,
            },
        ],
    }
}

struct Memory {
    bytes: Box<[u8]>,
    owner: CodeOwnerId,
    generations: [u64; 16],
}
impl MemoryBus for Memory {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.bytes
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        let byte = self
            .bytes
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))?;
        *byte = value;
        // Exhaustion makes code checks unavailable, never valid again.
        let generation = &mut self.generations[address as usize >> 12];
        *generation = generation.saturating_add(1);
        Ok(())
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        self.bytes
            .get(address as usize..(address as usize).checked_add(usize::from(length))?)
    }
    fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
        self.code_bytes(address, length)?;
        let first = address >> 12;
        let last = address.checked_add(u32::from(length).checked_sub(1)?)? >> 12;
        let generations = [
            self.generations[first as usize],
            self.generations[last as usize],
        ];
        if generations.contains(&u64::MAX) {
            return None;
        }
        Some(CodeCheck {
            space: u64::from(self.owner.get()) << 32,
            pages: [first, last],
            generations,
        })
    }
    fn code_valid(&self, check: &CodeCheck) -> bool {
        check.space == u64::from(self.owner.get()) << 32
            && check
                .pages
                .into_iter()
                .zip(check.generations)
                .all(|(page, generation)| {
                    generation != u64::MAX
                        && self.generations.get(page as usize) == Some(&generation)
                })
    }
}
impl PortBus for Memory {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

/// The wire frame is first: its wasm32 address is also this opaque owner handle.
/// No Rust reference survives across entry to the generated program.
#[repr(C)]
pub struct Probe {
    frame: [u8; abi::FRAME_BYTES],
    cpu: Cpu,
    bus: Memory,
    iterations: u32,
    cache: bool,
    callbacks: u64,
    service_callbacks: u64,
    fallback_callbacks: u64,
    completed: u32,
}
impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}
impl Probe {
    pub fn new() -> Self {
        let mut bus = Memory {
            bytes: vec![0; 65536].into_boxed_slice(),
            owner: CodeOwnerId::fresh().expect("probe code owner available"),
            generations: [0; 16],
        };
        bus.bytes[START_IP as usize..START_IP as usize + CODE.len()].copy_from_slice(&CODE);
        bus.bytes[0x208..0x210].copy_from_slice(&[255, 255, 0, 0, 0, 0x9a, 0xcf, 0]);
        bus.bytes[0x210..0x218].copy_from_slice(&[255, 255, 0, 0, 0, 0x92, 0xcf, 0]);
        let mut cpu = Cpu::default();
        cpu.set_dynamic_core(false);
        cpu.set_decode_cache(false);
        cpu.control.gdtr = DescriptorTable {
            base: 0x200,
            limit: 23,
        };
        cpu.set_protected_mode(true);
        cpu.load_segment(&mut bus, SegmentRegister::Cs, 8)
            .expect("fixture CS");
        for segment in [
            SegmentRegister::Ds,
            SegmentRegister::Es,
            SegmentRegister::Ss,
        ] {
            cpu.load_segment(&mut bus, segment, 16)
                .expect("fixture data segment");
        }
        let mut probe = Self {
            frame: [0; abi::FRAME_BYTES],
            cpu,
            bus,
            iterations: 0,
            cache: false,
            callbacks: 0,
            service_callbacks: 0,
            fallback_callbacks: 0,
            completed: 0,
        };
        probe.reset(1, false);
        probe
    }
    /// Reset registers outside timing, preserving a warmed decode cache when
    /// its policy is unchanged. No segment reload or guest-memory replacement.
    pub fn reset(&mut self, iterations: u32, cache: bool) {
        assert!((1..=MAX_ITERATIONS).contains(&iterations));
        if self.cache != cache {
            self.cpu.set_decode_cache(cache);
            self.cache = cache;
        }
        for (register, value) in GPRS.into_iter().zip(INITIAL_GPR) {
            self.cpu.registers.set32(register, value);
        }
        self.cpu.registers.set32(Gpr32::Ecx, iterations);
        self.cpu.registers.ip = START_IP;
        self.cpu.flags = 0x203;
        self.iterations = iterations;
        self.callbacks = 0;
        self.service_callbacks = 0;
        self.fallback_callbacks = 0;
        self.completed = 0;
        Frame {
            gpr: GPRS.map(|r| self.cpu.registers.get32(r)),
            eflags: self.cpu.flags,
            address: Address {
                space: 1,
                offset: 0,
            },
            // One extra unit reaches the endpoint callback before budget exit.
            remaining_budget: u64::from(iterations) * u64::from(INSTRUCTIONS_PER_ITERATION) + 1,
            ..Frame::default()
        }
        .write(&mut self.frame)
        .expect("fixed frame size");
    }
    /// Actual CPU interpreter, using its ordinary bounded batch API when the
    /// decode cache is enabled. Stop before HLT after precisely 5*N steps.
    pub fn reference(&mut self) -> u32 {
        let target = self.iterations * INSTRUCTIONS_PER_ITERATION;
        while self.completed < target {
            let remaining = target - self.completed;
            let batched = if self.cache {
                self.cpu
                    .interpret_batch(&mut self.bus, remaining, |_| remaining)
            } else {
                0
            };
            if batched == 0 {
                assert_eq!(
                    self.cpu.step(&mut self.bus).expect("fixture instruction"),
                    StepOutcome::Continue
                );
                self.completed += 1;
            } else {
                self.completed += batched;
            }
        }
        assert_eq!(self.cpu.registers.ip, START_IP + END_OFFSET);
        let mut frame = Frame::read(&self.frame).expect("fixed frame size");
        frame.gpr = GPRS.map(|r| self.cpu.registers.get32(r));
        frame.eflags = self.cpu.flags;
        frame.address.offset = self.cpu.registers.ip - START_IP;
        frame.charged_work = u64::from(self.completed);
        frame.remaining_budget -= u64::from(self.completed);
        frame.write(&mut self.frame).expect("fixed frame size");
        self.completed
    }
    #[cfg(any(target_arch = "wasm32", test))]
    fn callback(&mut self, reason: u32) -> u32 {
        self.callbacks += 1;
        assert!(
            self.callbacks <= u64::from(self.iterations) + 2,
            "bounded fixture callback limit"
        );
        let frame = Frame::read(&self.frame).expect("fixed frame size");
        assert_eq!(frame.address.space, 1);
        assert!(frame.address.offset <= END_OFFSET);
        for (r, value) in GPRS.into_iter().zip(frame.gpr) {
            self.cpu.registers.set32(r, value);
        }
        self.cpu.flags = frame.eflags;
        self.cpu.registers.ip = START_IP + frame.address.offset;
        match reason {
            abi::REASON_SERVICE => {
                self.service_callbacks += 1;
                abi::CONTINUE
            }
            abi::REASON_FALLBACK => {
                self.fallback_callbacks += 1;
                assert_eq!(
                    frame.address.offset, END_OFFSET,
                    "only endpoint fallback is admitted"
                );
                assert_eq!(frame.gpr[1], 0);
                abi::EXIT
            }
            _ => panic!("invalid callback reason"),
        }
    }
    /// Small architectural results only; the complete guest RAM remains owned
    /// by this probe and is compared through a full-memory FNV-1a hash.
    pub fn result(&self, field: u32) -> u64 {
        match field {
            0..=7 => u64::from(self.cpu.registers.get32(GPRS[field as usize])),
            8 => u64::from(self.cpu.flags),
            9 => u64::from(self.cpu.registers.ip),
            10 => self.bus.bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
            }),
            11 => self.callbacks,
            12 => self.service_callbacks,
            13 => self.fallback_callbacks,
            14 => u64::from(self.completed),
            _ => panic!("unknown result field"),
        }
    }
}

// Trusted diagnostic host only. Every handle must come from probe_new in this
// instance, remain live, and be exclusively borrowed for the entire call.
// The host must not reenter generated execution from static_runtime. Generated
// code borrows only frame bytes; callbacks synchronously recover the sole owner.
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod ffi {
    use super::*;
    #[unsafe(no_mangle)]
    pub extern "C" fn probe_new() -> u32 {
        Box::into_raw(Box::new(Probe::new())) as u32
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_free(handle: u32) {
        drop(unsafe { Box::from_raw(handle as *mut Probe) });
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_reset(handle: u32, iterations: u32, cache: u32) {
        unsafe { &mut *(handle as *mut Probe) }.reset(iterations, cache != 0);
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_reference(handle: u32) -> u32 {
        unsafe { &mut *(handle as *mut Probe) }.reference()
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_result(handle: u32, field: u32) -> u64 {
        unsafe { &*(handle as *const Probe) }.result(field)
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_runtime(frame_pointer: u32, reason: u32) -> u32 {
        unsafe { &mut *(frame_pointer as *mut Probe) }.callback(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_interpreter_preserves_aliases_and_cache_results() {
        let mut probe = Probe::new();
        for iterations in [1, 2, 17, 257, 65_537] {
            probe.reset(iterations, false);
            assert_eq!(probe.reference(), iterations * INSTRUCTIONS_PER_ITERATION);
            let expected = core::array::from_fn::<_, 11, _>(|i| probe.result(i as u32));
            assert_eq!(
                expected[0],
                u64::from(INITIAL_GPR[0].wrapping_add(iterations))
            );
            assert_eq!(expected[1], 0);
            assert_eq!(expected[2] >> 16, u64::from(INITIAL_GPR[2] >> 16));
            assert_eq!(
                expected[2] as u16,
                (INITIAL_GPR[2] as u16).wrapping_add((iterations * 3) as u16)
            );
            let mut bh = (INITIAL_GPR[3] >> 8) as u8;
            for i in 1..=iterations {
                bh ^= INITIAL_GPR[0].wrapping_add(i) as u8;
            }
            assert_eq!(
                expected[3],
                u64::from((INITIAL_GPR[3] & !0xff00) | (u32::from(bh) << 8))
            );
            assert_eq!(expected[8], 0x246);
            assert_eq!(expected[9], u64::from(START_IP + END_OFFSET));
            for _ in 0..2 {
                probe.reset(iterations, true);
                probe.reference();
                assert_eq!(expected, core::array::from_fn(|i| probe.result(i as u32)));
            }
        }
    }
    #[test]
    fn shared_frame_callback_updates_the_single_cpu_owner() {
        let mut probe = Probe::new();
        probe.reference();
        let expected = core::array::from_fn::<_, 11, _>(|i| probe.result(i as u32));
        let final_frame = Frame::read(&probe.frame).unwrap();
        probe.reset(1, false);
        final_frame.write(&mut probe.frame).unwrap();
        assert_eq!(probe.callback(abi::REASON_SERVICE), abi::CONTINUE);
        assert_eq!(probe.callback(abi::REASON_FALLBACK), abi::EXIT);
        assert_eq!(expected, core::array::from_fn(|i| probe.result(i as u32)));
        assert_eq!(
            (
                probe.callbacks,
                probe.service_callbacks,
                probe.fallback_callbacks
            ),
            (2, 1, 1)
        );
    }
}
