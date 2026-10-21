//! Diagnostic exact-x87 helper ABI, not a production CPU execution interface.
//! One owner holds the complete FPU. Execution allocates nothing; portable
//! snapshots allocate only when explicitly requested outside measurement.
use doslet_state::{State, prost::Message};
use doslet_x87::X87;

#[path = "x87_locals_programs.rs"]
pub mod local_programs;
#[path = "x87_locals.rs"]
mod locals;

pub const MAX_ITERATIONS: u32 = 2_000_000;
pub const LOAD: u32 = 0;
pub const MULTIPLY: u32 = 1;
pub const ADD: u32 = 2;
pub const STORE_INTEGER_POP: u32 = 3;
pub const ACCEPTED: u64 = 1 << 32;
pub const BASE: u32 = 1.25f32.to_bits();
pub const MULTIPLIER: u32 = 2.25f32.to_bits();
pub const ADDEND: u32 = 0.5f32.to_bits();
pub const START_IP: u32 = 0x1000;
pub const OPERAND: u32 = 0x2000;

pub struct Kernel {
    fpu: X87,
    // Public ABI is this explicit u32 array, never Kernel/X87's Rust layout:
    // iterations, integer checksum, completed iterations, refused operation+1.
    frame: [u32; 4],
    snapshot: Vec<u8>,
}

impl Default for Kernel {
    fn default() -> Self {
        Self {
            fpu: X87::new(),
            frame: [0; 4],
            snapshot: Vec::new(),
        }
    }
}

impl Kernel {
    pub fn reset(&mut self, control: u16, iterations: u32) {
        assert!((1..=MAX_ITERATIONS).contains(&iterations));
        self.fpu = X87::new();
        self.fpu.load_control(control);
        self.frame = [iterations, 0, 0, 0];
        self.snapshot.clear();
    }

    /// High 32 bits are 1 only for an accepted, completed operation. Low 32
    /// bits contain FISTP's signed-i32 encoding; all other accepted forms return
    /// zero there. A zero result refuses before ANY FPU/pointer/frame mutation.
    /// Masked arithmetic/denormal/precision/invalid status effects remain exact.
    /// Unknown operations, PC=01, any unmasked exception and missing/full stack
    /// operands decline. Guest memory/CR0/segment admission are outside this
    /// proof: the owner supplies operand bits and valid metadata explicitly.
    pub fn operation(
        &mut self,
        operation: u32,
        bits: u32,
        ip: u32,
        operand: u32,
        fast: bool,
    ) -> u64 {
        let control = self.fpu.control();
        if operation > STORE_INTEGER_POP
            || control & 0x3f != 0x3f
            || (control >> 8) & 3 == 1
            || if operation == LOAD {
                !self.fpu.is_empty(7)
            } else {
                self.fpu.is_empty(0)
            }
        {
            return 0;
        }
        let value = match operation {
            LOAD => {
                if fast {
                    self.fpu.fast_load_real(u64::from(bits), true)
                } else {
                    self.fpu.load_real(u64::from(bits), true)
                }
                .expect("masked real32 load with an empty push slot");
                0
            }
            MULTIPLY | ADD => {
                let op = u8::from(operation == MULTIPLY);
                if fast {
                    self.fpu.fast_arithmetic_real(u64::from(bits), true, op)
                } else {
                    self.fpu.arithmetic_real(u64::from(bits), true, op)
                }
                .expect("masked basic arithmetic with a nonempty operand");
                0
            }
            _ => self
                .fpu
                .store_integer(32)
                .expect("masked FISTP with a nonempty operand") as u32,
        };
        // Real-mode CS=DS=0 fixture, matching the four reference instructions.
        let opcode = [0x106, 0x00e, 0x006, 0x31e][operation as usize];
        self.fpu.record_instruction(ip, 0, opcode);
        self.fpu.record_operand(operand, 0);
        if operation == STORE_INTEGER_POP {
            self.fpu.pop().expect("preflighted FISTP stack pop");
        }
        ACCEPTED | u64::from(value)
    }

    pub fn run(&mut self, fast: bool) -> bool {
        for iteration in 0..self.frame[0] {
            let operands = [BASE + (iteration & 0xffff), MULTIPLIER, ADDEND, 0];
            let mut value = 0;
            for operation in LOAD..=STORE_INTEGER_POP {
                let result = self.operation(
                    operation,
                    operands[operation as usize],
                    START_IP + operation * 4,
                    OPERAND + operation * 4,
                    fast,
                );
                if result & ACCEPTED == 0 {
                    self.frame[3] = operation + 1;
                    return false;
                }
                value = result as u32;
            }
            self.frame[1] = checksum(self.frame[1], value, iteration);
            self.frame[2] = iteration + 1;
        }
        true
    }

    pub fn state(&mut self) -> &[u8] {
        self.snapshot = self.fpu.save().encode_to_vec();
        for word in self.frame {
            self.snapshot.extend_from_slice(&word.to_le_bytes());
        }
        &self.snapshot
    }

    pub fn frame(&self) -> [u32; 4] {
        self.frame
    }
}

pub fn checksum(previous: u32, value: u32, iteration: u32) -> u32 {
    previous
        .rotate_left(5)
        .wrapping_add(value)
        .wrapping_add(iteration ^ 0x9e37)
}

// Trusted synchronous diagnostic FFI, following the existing static-probe
// handle contract. Every handle originates here and has one exclusive owner.
// Generated code receives only that handle and the explicit 16-byte frame.
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod ffi {
    use super::*;
    #[unsafe(no_mangle)]
    pub extern "C" fn x87_probe_new() -> u32 {
        Box::into_raw(Box::new(Kernel::default())) as u32
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn x87_probe_free(handle: u32) {
        drop(unsafe { Box::from_raw(handle as *mut Kernel) });
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn x87_probe_reset(handle: u32, control: u32, iterations: u32) {
        unsafe { &mut *(handle as *mut Kernel) }.reset(control as u16, iterations);
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn x87_probe_frame(handle: u32) -> u32 {
        unsafe { &mut *(handle as *mut Kernel) }.frame.as_mut_ptr() as u32
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_x87_operation(
        handle: u32,
        operation: u32,
        bits: u32,
        ip: u32,
        operand: u32,
    ) -> u64 {
        unsafe { &mut *(handle as *mut Kernel) }.operation(operation, bits, ip, operand, true)
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn x87_probe_reference(handle: u32, fast: u32) -> u32 {
        u32::from(unsafe { &mut *(handle as *mut Kernel) }.run(fast != 0))
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn x87_probe_locals(handle: u32, batch: u32) -> u32 {
        u32::from(unsafe { &mut *(handle as *mut Kernel) }.run_locals(batch))
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn x87_probe_check_locals(seed: u64, control: u32, batches: u32) -> u32 {
        u32::from(local_programs::check(seed, control as u16, batches))
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn x87_probe_state(handle: u32) -> u64 {
        let bytes = unsafe { &mut *(handle as *mut Kernel) }.state();
        ((bytes.as_ptr() as u64) << 32) | bytes.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_bus::{BusError, MemoryBus, PortBus};
    use doslet_x86::Cpu;
    struct Bus([u8; 0x3000]);
    impl MemoryBus for Bus {
        fn read_u8(&mut self, at: u32) -> Result<u8, BusError> {
            Ok(self.0[at as usize])
        }
        fn write_u8(&mut self, at: u32, value: u8) -> Result<(), BusError> {
            self.0[at as usize] = value;
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
    fn exact_helpers_match_actual_cpu_and_full_fpu_state() {
        let code = [
            0xd9, 6, 0, 0x20, 0xd8, 0x0e, 4, 0x20, 0xd8, 6, 8, 0x20, 0xdb, 0x1e, 12, 0x20,
        ];
        for precision in [0, 2, 3] {
            for rounding in 0..4 {
                for input in [
                    BASE,
                    (-1.25f32).to_bits(),
                    0,
                    0x8000_0000,
                    1,
                    0x7f80_0000,
                    0x7fc0_1234,
                ] {
                    let control = 0x7f | (precision << 8) | (rounding << 10);
                    let mut helper = Kernel::default();
                    helper.reset(control, 1);
                    let mut cpu = Cpu::default().with_x87();
                    cpu.fpu.as_mut().unwrap().load_control(control);
                    cpu.registers.ip = START_IP;
                    let mut bus = Bus([0; 0x3000]);
                    bus.0[0x1000..0x1010].copy_from_slice(&code);
                    for (operation, bits) in [input, MULTIPLIER, ADDEND, 0].into_iter().enumerate()
                    {
                        bus.0[0x2000 + operation * 4..0x2004 + operation * 4]
                            .copy_from_slice(&bits.to_le_bytes());
                        let result = helper.operation(
                            operation as u32,
                            bits,
                            START_IP + operation as u32 * 4,
                            OPERAND + operation as u32 * 4,
                            true,
                        );
                        assert_ne!(result & ACCEPTED, 0);
                        cpu.step(&mut bus).unwrap();
                        assert_eq!(
                            helper.fpu.save().encode_to_vec(),
                            cpu.fpu.as_ref().unwrap().save().encode_to_vec()
                        );
                        if operation == STORE_INTEGER_POP as usize {
                            assert_eq!(
                                result as u32,
                                u32::from_le_bytes(bus.0[0x200c..0x2010].try_into().unwrap())
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn refusal_is_unchanged_and_masked_conversion_remains_exact() {
        for control in [0x017f, 0x0c5f, 0x0c7e] {
            let mut kernel = Kernel::default();
            kernel.reset(control, 1);
            let before = kernel.state().to_vec();
            assert_eq!(kernel.operation(LOAD, BASE, 1, 2, true), 0);
            assert_eq!(kernel.state(), before);
        }
        let mut kernel = Kernel::default();
        kernel.reset(0x0c7f, 1);
        for operation in [MULTIPLY, ADD, STORE_INTEGER_POP, 99] {
            let before = kernel.state().to_vec();
            assert_eq!(kernel.operation(operation, BASE, 1, 2, true), 0);
            assert_eq!(kernel.state(), before);
        }
        for _ in 0..8 {
            assert_ne!(kernel.operation(LOAD, BASE, 1, 2, true), 0);
        }
        let before = kernel.state().to_vec();
        assert_eq!(kernel.operation(LOAD, BASE, 1, 2, true), 0);
        assert_eq!(kernel.state(), before);
    }

    #[test]
    fn mixed_loop_matches_reference_without_execution_allocations() {
        for precision in [0, 2, 3] {
            for rounding in 0..4 {
                let control = 0x7f | (precision << 8) | (rounding << 10);
                let mut reference = Kernel::default();
                let mut helper = Kernel::default();
                reference.reset(control, 257);
                helper.reset(control, 257);
                assert!(reference.run(false));
                let measured = allocation_counter::measure(|| {
                    assert!(helper.run(true));
                });
                assert_eq!(measured.count_total, 0);
                assert_eq!(helper.frame(), reference.frame());
                assert_eq!(helper.state(), reference.state());
            }
        }
    }
}
