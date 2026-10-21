//! Batched x87 forms against the byte-fetching reference handler: random
//! programs over the batched subset and some reference-only forms, in flat
//! protected and real mode, compare registers, flags, complete FPU state,
//! errors and RAM after every program.
use super::*;
use crate::PcMachine;
use alloc::vec::Vec;
use doslet_x86::{Gpr32, SegmentRegister};

const CODE: u32 = 0x1_0000;
/// Its own page, reachable from real-mode segment 1000h.
const DATA: u32 = 0x1_8000;
/// FLDCW operands, outside the area that stores may overwrite.
const CONTROLS: u32 = DATA + 0x800;
const CONTROL_WORDS: [u16; 14] = [
    0x037F, 0x027F, 0x007F, 0x0C7F, 0x087F, 0x047F, 0x0E7F, 0x0A7F, 0x067F, 0x0F7F, 0x0B7F, 0x017F,
    0x035F, 0x0C7E,
];

fn descriptor(bus: &mut PcBus, selector: u16, base: u32, limit: u32, code: bool) {
    let bytes = [
        limit as u8,
        (limit >> 8) as u8,
        base as u8,
        (base >> 8) as u8,
        (base >> 16) as u8,
        if code { 0x9B } else { 0x93 },
        ((limit >> 16) as u8 & 15) | 0x40 | 0x80,
        (base >> 24) as u8,
    ];
    for (i, value) in bytes.into_iter().enumerate() {
        bus.write_u8(0x800 + u32::from(selector) + i as u32, value)
            .unwrap();
    }
}

/// Flat 32-bit protected mode, or real mode with every segment at 1000h.
fn machine(protected: bool, batched: bool, fast: bool) -> PcMachine {
    let mut m = PcMachine::default();
    assert!(m.cpu.fpu.is_some());
    m.bus.set_a20(true);
    if protected {
        descriptor(&mut m.bus, 8, 0, 0xFFFFF, true);
        descriptor(&mut m.bus, 16, 0, 0xFFFFF, false);
        m.cpu.control.cr0 |= 1;
        m.cpu.control.gdtr.base = 0x800;
        m.cpu.control.gdtr.limit = 0xFF;
    }
    m.bus.write_u8(0, 0x90).unwrap();
    m.cpu.step(&mut m.bus).unwrap(); // Synchronize the mode before segment loads.
    for segment in SegmentRegister::ALL {
        let selector = match (protected, segment) {
            (true, SegmentRegister::Cs) => 8,
            (true, _) => 16,
            (false, _) => 0x1000,
        };
        m.cpu.load_segment(&mut m.bus, segment, selector).unwrap();
    }
    m.cpu.flags = 2;
    m.cpu.set_decode_cache(batched);
    m.cpu.set_x87_batching(batched);
    m.cpu.set_x87_fast_paths(fast);
    m
}

struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// A memory ModR/M operand addressing `DATA + offset`, through EBX/ESI (BX/SI
/// in real mode) or an absolute displacement. Displacement-8 forms reach
/// offsets 0..7Fh.
fn memory(random: &mut Random, group: u8, offset: u32, protected: bool, bytes: &mut Vec<u8>) {
    let reg = group << 3;
    let forms = if offset < 0x80 { 3 } else { 2 };
    if protected {
        match random.below(forms) {
            0 => {
                bytes.push(reg | 5);
                bytes.extend_from_slice(&(DATA + offset).to_le_bytes());
            }
            1 => {
                // [EBX + disp32]; EBX = DATA.
                bytes.push(0x80 | reg | 3);
                bytes.extend_from_slice(&offset.to_le_bytes());
            }
            _ => {
                // [EBX + ESI*4 + disp8]; ESI = 4.
                bytes.extend_from_slice(&[0x44 | reg, 0xB3, (offset as u8).wrapping_sub(16)]);
            }
        }
    } else {
        // Real-mode linear DATA is 1000h:(DATA − 10000h).
        match random.below(forms) {
            0 => {
                bytes.push(reg | 6);
                bytes.extend_from_slice(&((DATA - 0x1_0000 + offset) as u16).to_le_bytes());
            }
            1 => {
                // [BX + disp16]; BX = DATA − 10000h.
                bytes.push(0x80 | reg | 7);
                bytes.extend_from_slice(&(offset as u16).to_le_bytes());
            }
            _ => {
                // [BP + SI + disp8] uses SS; BP = DATA − 10000h − 4, SI = 4.
                bytes.extend_from_slice(&[0x42 | reg, offset as u8]);
            }
        }
    }
}

/// One random instruction; `depth` tracks the intended stack depth.
fn instruction(random: &mut Random, depth: &mut i32, protected: bool, bytes: &mut Vec<u8>) {
    let offset = if random.below(2) == 0 {
        random.below(0x78) as u32 + 8
    } else {
        random.below(0x1F0) as u32
    };
    // Mostly a register that holds a value; sometimes any, so that stack
    // faults also reach the reference handler.
    let i = if *depth > 0 && random.below(256) != 0 {
        random.below(*depth as u64) as u8
    } else {
        random.below(8) as u8
    };
    let push = *depth < 8;
    let full = *depth > 0;
    let two = *depth > 1;
    loop {
        match random.below(26) {
            // Memory arithmetic/compare: D8/DC real, DA/DE integer.
            0..=4 if full => {
                let opcode = [0xD8, 0xDC, 0xDA, 0xDE][random.below(4) as usize];
                let group = random.below(8) as u8;
                bytes.push(opcode);
                memory(random, group, offset, protected, bytes);
                *depth -= i32::from(group == 3);
            }
            // Loads: FLD m32/m64/m80, FILD m16/m32/m64.
            5..=7 if push => {
                let (opcode, group) = [
                    (0xD9, 0),
                    (0xDD, 0),
                    (0xDB, 5),
                    (0xDF, 0),
                    (0xDB, 0),
                    (0xDF, 5),
                ][random.below(6) as usize];
                bytes.push(opcode);
                memory(random, group, offset, protected, bytes);
                *depth += 1;
            }
            // Stores: FST(P) m32/m64, FSTP m80, FIST(P) m16/m32, FISTP m64.
            8..=9 if full => {
                let (opcode, group) = [
                    (0xD9, 2),
                    (0xD9, 3),
                    (0xDD, 2),
                    (0xDD, 3),
                    (0xDB, 7),
                    (0xDB, 2),
                    (0xDB, 3),
                    (0xDF, 2),
                    (0xDF, 3),
                    (0xDF, 7),
                ][random.below(10) as usize];
                bytes.push(opcode);
                memory(random, group, offset, protected, bytes);
                *depth -= i32::from(group == 3 || group == 7);
            }
            // FLDCW from the control table, FNSTCW/FNSTSW m16.
            10 => {
                // Mostly fully masked control words with PC=24/53/64.
                let count = if random.below(8) == 0 {
                    CONTROL_WORDS.len()
                } else {
                    CONTROL_WORDS.len() - 3
                };
                bytes.push(0xD9);
                let control = random.below(count as u64) as u32 * 2;
                memory(random, 5, 0x800 + control, protected, bytes);
            }
            11 => {
                let (opcode, group) = [(0xD9, 7), (0xDD, 7)][random.below(2) as usize];
                bytes.push(opcode);
                memory(random, group, offset, protected, bytes);
            }
            // Register arithmetic.
            12..=15 if full => {
                let opcode = [0xD8, 0xDC, 0xDE][random.below(3) as usize];
                let mut group = random.below(8) as u8;
                if opcode != 0xD8 && (group == 2 || group == 3) {
                    group = 0;
                }
                bytes.extend_from_slice(&[opcode, 0xC0 | (group << 3) | i]);
                *depth -= i32::from(opcode == 0xDE || (opcode == 0xD8 && group == 3));
            }
            16 if full => {
                // FCOMPP, FUCOMPP, FUCOM(P), FTST, FXAM.
                let (form, pops) = [
                    ([0xDD, 0xE0 | i], 0),
                    ([0xDD, 0xE8 | i], 1),
                    ([0xD9, 0xE4], 0),
                    ([0xD9, 0xE5], 0),
                    ([0xDE, 0xD9], 2),
                    ([0xDA, 0xE9], 2),
                ][random.below(if two || i == 7 { 6 } else { 4 }) as usize];
                bytes.extend_from_slice(&form);
                *depth -= pops;
            }
            17..=18 if full => {
                // FXCH, FST/FSTP ST(i), FFREE.
                let (form, pops) = [
                    ([0xD9, 0xC8 | i], 0),
                    ([0xDD, 0xD0 | i], 0),
                    ([0xDD, 0xD8 | i], 1),
                    ([0xD9, 0xC8 | u8::from(two)], 0),
                ][random.below(4) as usize];
                bytes.extend_from_slice(&form);
                *depth -= pops;
            }
            19 if push => {
                // FLD ST(i) or a constant.
                if full && random.below(2) == 0 {
                    bytes.extend_from_slice(&[0xD9, 0xC0 | i]);
                } else {
                    bytes.extend_from_slice(&[0xD9, 0xE8 | (random.below(7) as u8)]);
                }
                *depth += 1;
            }
            20 if full => {
                // FCHS, FABS, FSQRT, FRNDINT.
                let modrm = [0xE0, 0xE1, 0xFA, 0xFC][random.below(4) as usize];
                bytes.extend_from_slice(&[0xD9, modrm]);
            }
            21 => {
                // FNOP, FNCLEX, FNSTSW AX, FFREE of an empty register and a
                // FDECSTP/FINCSTP pair.
                match random.below(5) {
                    0 => bytes.extend_from_slice(&[0xD9, 0xD0]),
                    1 => bytes.extend_from_slice(&[0xDB, 0xE2]),
                    2 => bytes.extend_from_slice(&[0xDF, 0xE0]),
                    3 if push => bytes.extend_from_slice(&[0xDD, 0xC7]),
                    _ => bytes.extend_from_slice(&[0xD9, 0xF6, 0xD9, 0xF7]),
                }
            }
            22 if full && push && random.below(8) == 0 => {
                // Reference-only forms between batched ones: FXTRACT, FSIN,
                // FSCALE and FPREM (which also read ST(1)).
                let modrm =
                    [0xF4, 0xFE, 0xFD, 0xF8][random.below(if two { 4 } else { 2 }) as usize];
                bytes.extend_from_slice(&[0xD9, modrm]);
                *depth += i32::from(modrm == 0xF4);
            }
            23 => {
                // Integer work and a segment override on a memory form.
                bytes.extend_from_slice(&[0x40 | random.below(2) as u8]); // INC EAX/ECX
            }
            24 if full => {
                bytes.push(0x3E);
                bytes.push(0xD8);
                let group = random.below(8) as u8;
                memory(random, group, offset, protected, bytes);
                *depth -= i32::from(group == 3);
            }
            25 if random.below(16) == 0 => {
                bytes.extend_from_slice(&[0xDB, 0xE3]); // FNINIT
                *depth = 0;
            }
            _ => continue,
        }
        return;
    }
}

fn data(random: &mut Random, m: &mut PcMachine) {
    for offset in (0..0x200).step_by(4) {
        let value = match random.below(6) {
            0 => (random.below(400) as f32 - 200.0).to_bits(),
            1 => (random.next() as f32 / 3.0e9).to_bits(),
            2 => random.next() as u32,
            3 => (random.below(2000) as i32 - 1000) as u32,
            4 => ((random.next() as f64 * 1e-19) as f32).to_bits(),
            _ => 0,
        };
        m.bus.write_u32(DATA + offset, value).unwrap();
    }
    for (i, control) in CONTROL_WORDS.into_iter().enumerate() {
        m.bus.write_u16(CONTROLS + 2 * i as u32, control).unwrap();
    }
}

fn prepare(m: &mut PcMachine, protected: bool, program: &[u8], seed: u64) {
    let mut random = Random(seed);
    data(&mut random, m);
    for (i, byte) in program.iter().enumerate() {
        m.bus.write_u8(CODE + i as u32, *byte).unwrap();
    }
    let base = if protected { 0 } else { 0x1_0000 };
    m.cpu.registers.ip = CODE - base;
    m.cpu.registers.set32(Gpr32::Ebx, DATA - base);
    m.cpu.registers.set32(Gpr32::Esi, 4);
    m.cpu.registers.set32(Gpr32::Ebp, DATA - base - 4);
    m.cpu.registers.set32(Gpr32::Eax, 0);
}

/// Run `steps` instructions: batches where possible, otherwise one reference
/// step, as the machine schedules them. Stops at the first error.
fn run(m: &mut PcMachine, steps: u32) -> Result<(), doslet_x86::CpuError> {
    let mut done = 0;
    while done < steps {
        let count = m
            .cpu
            .interpret_batch(&mut m.bus, steps - done, |_| steps - done);
        done += count;
        if count == 0 {
            m.cpu.step(&mut m.bus)?;
            done += 1;
        }
    }
    Ok(())
}

fn equal(a: &PcMachine, b: &PcMachine) {
    assert_eq!(a.cpu.registers, b.cpu.registers);
    assert_eq!(a.cpu.flags, b.cpu.flags);
    assert!(
        a.cpu
            .fpu
            .as_ref()
            .unwrap()
            .bitwise_eq(b.cpu.fpu.as_ref().unwrap()),
        "{:?}\n{:?}",
        a.cpu.fpu,
        b.cpu.fpu
    );
    assert_eq!(
        a.bus.ram.view(DATA as usize..DATA as usize + 0x1000),
        b.bus.ram.view(DATA as usize..DATA as usize + 0x1000)
    );
}

#[test]
fn batched_x87_forms_match_the_reference_handler() {
    let mut random = Random(0x0087_BA7C);
    let (mut batched_steps, mut errors) = (0, 0);
    for protected in [true, false] {
        let mut reference = machine(protected, false, false);
        let mut variants = [
            machine(protected, true, false),
            machine(protected, true, true),
        ];
        for program in 0..300 {
            // FNINIT; stale registers from the previous program remain.
            let mut bytes = alloc::vec![0xDB, 0xE3];
            let mut depth = 0;
            let mut steps = 0;
            while bytes.len() < 0xF00 && steps < 400 {
                instruction(&mut random, &mut depth, protected, &mut bytes);
                depth = depth.clamp(0, 8);
                steps += 1;
            }
            let seed = random.next();
            prepare(&mut reference, protected, &bytes, seed);
            let mut expected: Result<(), doslet_x86::CpuError> = Ok(());
            let mut executed = 0;
            for _ in 0..steps {
                if let Err(error) = reference.cpu.step(&mut reference.bus) {
                    expected = Err(error);
                    break;
                }
                executed += 1;
            }
            for variant in &mut variants {
                prepare(variant, protected, &bytes, seed);
                let actual = run(variant, executed);
                assert_eq!(actual, Ok(()), "program {program}");
                if expected.is_err() {
                    // The failing instruction must be declined without effect
                    // and then fail identically in the reference step.
                    assert_eq!(
                        variant.cpu.interpret_batch(&mut variant.bus, 256, |_| 256),
                        0,
                        "program {program}"
                    );
                    assert_eq!(
                        variant.cpu.step(&mut variant.bus).err(),
                        expected.err(),
                        "program {program}"
                    );
                }
                equal(&reference, variant);
            }
            batched_steps += executed;
            errors += u32::from(expected.is_err());
        }
    }
    assert!(batched_steps > 60_000, "{batched_steps}");
    assert!(errors > 100, "{errors}");
}
