//! Doslet-specific entry points. Upstream source remains inside one isolated
//! WASM instance: its fixed pointers never become native Rust pointers.
use crate::cpu::{cpu, global_pointers as state, memory};

pub static mut MACHINE_BOUNDARIES: bool = false;
static mut WAIT_BOUNDARY: u32 = u32::MAX;
pub static mut A20_ENABLED: bool = true;
pub static mut STOPPED_INTERRUPT: u32 = 0;
static mut EXCEPTION_EXITS: bool = false;
static mut EXTERNAL_INTERRUPT: bool = false;
static mut STOPPED_EXCEPTION: u32 = 0;
static mut EXCEPTION_ERROR: Option<i32> = None;
static mut INTERRUPT_SHADOW: bool = false;
pub const BUDGET: *mut u32 = 1320 as *mut u32;
static mut FPU_IMAGE: [u8; 108] = [0; 108];
static mut REPORTED_IRET_FAULT: bool = false;

/// Capture only the first rejected return, before exception delivery can
/// replace the stack/CS. No guest buffer is copied and no successful IRET logs.
pub unsafe fn iret_fault(reason: &str, is_16: bool, ip: i32, cs: i32, flags: i32) {
    if REPORTED_IRET_FAULT { return; }
    REPORTED_IRET_FAULT = true;
    console_log!(
        "IRET fault: {} width={} at={:04x}:{:08x} ss:esp={:04x}:{:08x} ss_base={:08x} ss_limit={:08x} ss_access={:02x} stack32={} cpl={} return={:04x}:{:08x} flags={:08x} gdtr={:08x}/{} ldtr={:04x} ldt={:08x}/{}",
        reason, if is_16 { 16 } else { 32 },
        *state::sreg.add(cpu::CS as usize),
        (*state::previous_ip).wrapping_sub(*state::segment_offsets.add(cpu::CS as usize)),
        *state::sreg.add(cpu::SS as usize), *state::reg32.add(cpu::ESP as usize),
        *state::segment_offsets.add(cpu::SS as usize), *state::segment_limits.add(cpu::SS as usize),
        *state::segment_access_bytes.add(cpu::SS as usize), *state::stack_size_32,
        *state::cpl, cs, ip, flags, *state::gdtr_offset, *state::gdtr_size,
        *state::sreg.add(cpu::LDTR as usize), *state::segment_offsets.add(cpu::LDTR as usize),
        *state::segment_limits.add(cpu::LDTR as usize)
    );
}

#[no_mangle]
pub unsafe fn doslet_set_budget(budget: u32) { *BUDGET = budget; }

pub unsafe fn boundary_at(address: u32) -> bool {
    if address == WAIT_BOUNDARY { return true; }
    // Prefixes do not hide an I/O, interrupt or mode-changing instruction.
    for offset in 0..15 {
        let at = address.wrapping_add(offset);
        if at >= *state::memory_size { return true; }
        let opcode = *memory::mem8.add(at as usize);
        match opcode {
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3 => continue,
            0x6c..=0x6f | 0xe4..=0xe7 | 0xec..=0xef | 0xcc..=0xcf | 0xfa | 0xfb | 0xf4 | 0x8e | 0x9d | 0x07 | 0x17 | 0x1f | 0x9a | 0xea | 0xca | 0xcb => return true,
            0x0f => {
                // Most extended opcodes are ordinary work (MOVZX, MOVSX,
                // CMOV, SETcc, SSE). Only system/time/mode boundaries need
                // the machine to commit preceding device time.
                if at.wrapping_add(1) >= *state::memory_size { return true; }
                let second = *memory::mem8.add(at.wrapping_add(1) as usize);
                return matches!(second, 0x00 | 0x01 | 0x05 | 0x07 | 0x20..=0x23 | 0x30..=0x35 | 0xa1 | 0xa9 | 0xaa | 0xb2);
            },
            _ => return false,
        }
    }
    true
}

#[no_mangle]
pub unsafe fn doslet_run(budget: u32, translated: u32) -> u32 {
    let initial = *state::instruction_counter;
    while (*state::instruction_counter).wrapping_sub(initial) < budget && !*state::in_hlt && STOPPED_INTERRUPT == 0 && STOPPED_EXCEPTION == 0 {
        let completed = (*state::instruction_counter).wrapping_sub(initial);
        *BUDGET = budget - completed;
        let physical = match cpu::get_phys_eip() { Ok(address) => address, Err(()) => continue };
        let boundary = MACHINE_BOUNDARIES && boundary_at(physical);
        if boundary && completed != 0 { break; }
        let before = *state::instruction_counter;
        if INTERRUPT_SHADOW {
            // MOV/POP SS (and LSS) inhibit IRQs through exactly the next
            // instruction. Execute that instruction alone across a host
            // boundary so no scheduler can inject between SS and SP.
            INTERRUPT_SHADOW = false;
            doslet_step();
            if boundary { break; }
            continue;
        }
        if translated != 0 && !boundary { cpu::cycle_internal(); }
        if STOPPED_EXCEPTION == 0 && (translated == 0 || boundary || before == *state::instruction_counter) { doslet_step(); }
        if boundary { break; }
    }
    (*state::instruction_counter).wrapping_sub(initial) | if INTERRUPT_SHADOW { 1 << 31 } else { 0 }
}
#[no_mangle]
pub unsafe fn doslet_machine_boundaries(enabled: u32) { MACHINE_BOUNDARIES = enabled != 0; }
#[no_mangle]
pub unsafe fn doslet_wait_boundary(address: u32) { WAIT_BOUNDARY = address; }

pub unsafe fn set_interrupt_shadow() { INTERRUPT_SHADOW = true; }
#[no_mangle]
pub unsafe fn doslet_set_interrupt_shadow(enabled: u32) { INTERRUPT_SHADOW = enabled != 0; }

#[no_mangle]
pub unsafe fn doslet_exception_exits(enabled: u32) { EXCEPTION_EXITS = enabled != 0; }

pub unsafe fn intercept_exception(vector: i32, software: bool, error: Option<i32>) -> bool {
    if !EXCEPTION_EXITS || EXTERNAL_INTERRUPT || software || !(0..32).contains(&vector) || !*state::protected_mode {
        return false;
    }
    STOPPED_EXCEPTION = vector as u32 + 1;
    EXCEPTION_ERROR = error;
    cpu::after_block_boundary();
    true
}

/// Packed vector+1 and an error-code-present flag. The error value is read
/// separately, preserving all 32 bits without reserving a sentinel value.
#[no_mangle]
pub unsafe fn doslet_take_exception() -> u32 {
    let error = EXCEPTION_ERROR;
    let value = STOPPED_EXCEPTION | if error.is_some() { 0x100 } else { 0 };
    STOPPED_EXCEPTION = 0;
    value
}
#[no_mangle]
pub unsafe fn doslet_exception_error() -> i32 { EXCEPTION_ERROR.unwrap_or(0) }
#[no_mangle]
pub unsafe fn doslet_set_a20(enabled: u32) {
    if A20_ENABLED != (enabled != 0) {
        A20_ENABLED = enabled != 0;
        cpu::full_clear_tlb();
        crate::jit::jit_clear_cache_js();
    }
}
pub fn a20(address: u32) -> u32 { if unsafe { A20_ENABLED } { address } else { address & !(1 << 20) } }

pub unsafe fn after_interrupt(vector: u8) {
    if MACHINE_BOUNDARIES {
        STOPPED_INTERRUPT = u32::from(vector) + 1;
        cpu::after_block_boundary();
    }
}
#[no_mangle]
pub unsafe fn doslet_take_interrupt() -> u32 {
    let value = STOPPED_INTERRUPT;
    STOPPED_INTERRUPT = 0;
    value
}

#[no_mangle]
pub unsafe fn doslet_fpu_image() -> u32 {
    use crate::cpu::fpu;
    let bytes = &mut *(&raw mut FPU_IMAGE);
    bytes.fill(0);
    for offset in [2, 6, 10, 26] { bytes[offset..offset + 2].copy_from_slice(&0xffffu16.to_le_bytes()); }
    for (offset, value) in [(0, *state::fpu_control_word), (4, fpu::fpu_load_status_word()), (8, fpu::fpu_load_tag_word() as u16), (16, *state::fpu_ip_selector as u16), (18, *state::fpu_opcode as u16), (24, *state::fpu_dp_selector as u16)] {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    bytes[12..16].copy_from_slice(&(*state::fpu_ip as u32).to_le_bytes());
    bytes[20..24].copy_from_slice(&(*state::fpu_dp as u32).to_le_bytes());
    for i in 0..8 {
        let value = *state::fpu_st.add((usize::from(*state::fpu_stack_ptr) + i) & 7);
        bytes[28 + i * 10..36 + i * 10].copy_from_slice(&value.mantissa.to_le_bytes());
        bytes[36 + i * 10..38 + i * 10].copy_from_slice(&value.sign_exponent.to_le_bytes());
    }
    &raw const FPU_IMAGE as u32
}

#[no_mangle]
pub unsafe fn doslet_rebind_context() {
    use crate::cpu::fpu;
    let bytes = &*(&raw const FPU_IMAGE);
    let word = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    fpu::set_control_word(word(0));
    fpu::fpu_set_status_word(word(4));
    fpu::fpu_set_tag_word(i32::from(word(8)));
    *state::fpu_ip = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
    *state::fpu_ip_selector = i32::from(word(16));
    *state::fpu_opcode = i32::from(word(18));
    *state::fpu_dp = i32::from_le_bytes(bytes[20..24].try_into().unwrap());
    *state::fpu_dp_selector = i32::from(word(24));
    for i in 0..8 {
        *state::fpu_st.add((usize::from(*state::fpu_stack_ptr) + i) & 7) = crate::softfloat::F80 {
            mantissa: u64::from_le_bytes(bytes[28 + i * 10..36 + i * 10].try_into().unwrap()),
            sign_exponent: word(36 + i * 10),
        };
    }
    cpu::full_clear_tlb();
    cpu::update_state_flags();
}

#[no_mangle]
pub unsafe fn doslet_abi_version() -> u32 { 4 }

#[no_mangle]
pub unsafe fn doslet_init(size: u32) -> u32 {
    REPORTED_IRET_FAULT = false;
    MACHINE_BOUNDARIES = false;
    WAIT_BOUNDARY = u32::MAX;
    A20_ENABLED = true;
    STOPPED_INTERRUPT = 0;
    STOPPED_EXCEPTION = 0;
    EXCEPTION_ERROR = None;
    EXCEPTION_EXITS = false;
    EXTERNAL_INTERRUPT = false;
    INTERRUPT_SHADOW = false;
    // Embedded workers reuse this module after machine replacement, unlike
    // standalone hosts which instantiate a fresh module for each CPU.
    cpu::tsc_offset = 0;
    cpu::tsc_last_value = 0;
    cpu::tsc_resolution = u64::MAX;
    cpu::tsc_number_of_same_readings = 0;
    cpu::tsc_speed = 1;
    cpu::in_jit = false;
    cpu::jit_block_boundary = false;
    cpu::jit_exit_reason = cpu::JitExitReason::None;
    #[cfg(debug_assertions)] { cpu::tsc_last_extra = 0; }
    crate::jit::rust_init();
    let base = memory::allocate_memory(size);
    *state::memory_size = size;
    memory::zero_memory(0, size);
    cpu::reset_cpu();
    base
}

#[no_mangle]
pub unsafe fn doslet_step() {
    *state::previous_ip = *state::instruction_pointer;
    let opcode = match cpu::read_imm8() { Ok(opcode) => opcode, Err(()) => return };
    cpu::run_instruction(opcode | ((*state::is_32 as i32) << 8));
    *state::instruction_counter = (*state::instruction_counter).wrapping_add(1);
}

#[no_mangle]
pub unsafe fn doslet_cycle() { cpu::cycle_internal(); }

#[no_mangle]
pub unsafe fn doslet_fault_step() -> u32 {
    matches!(cpu::jit_exit_reason, cpu::JitExitReason::CpuException { .. }) as u32
}

#[no_mangle]
pub unsafe fn doslet_inject_interrupt(vector: u32) -> u32 {
    if vector > 255 || *state::flags & cpu::FLAG_INTERRUPT == 0 || INTERRUPT_SHADOW { return 0; }
    *state::in_hlt = false;
    *state::previous_ip = *state::instruction_pointer;
    EXTERNAL_INTERRUPT = true;
    cpu::call_interrupt_vector(vector as i32, false, None);
    EXTERNAL_INTERRUPT = false;
    1
}

#[no_mangle]
pub unsafe fn doslet_set_real_segment(index: i32, selector: i32) {
    if index == cpu::CS { cpu::switch_cs_real_mode(selector); }
    else { cpu::switch_seg(index, selector); }
    cpu::full_clear_tlb();
    cpu::update_state_flags();
}

#[no_mangle]
pub unsafe fn doslet_force_compile(address: i32) {
    let phys = match cpu::translate_address_fetch(address) { Ok(phys) => phys, Err(()) => return };
    crate::jit::jit_increase_hotness_and_maybe_compile(address, phys, cpu::get_seg_cs() as u32, cpu::get_state_flags(), crate::jit::JIT_THRESHOLD);
}
