//! Worker-only v86 host. The linked CPU and Rust devices use one WASM memory
//! and allocator. Bytes below 4096 are reserved by the browser linker for the
//! upstream register ABI; all guest RAM is an ordinary aligned allocation.
use crate::devices;
use doslet_bus::{BusError, CpuBus};
use doslet_v86::{Context, DeviceRequest, Registers, RunResult, Segment, Statistics};
use doslet_v86_module::{
    cpu::{cpu, memory},
    doslet, jit,
};
use std::{marker::PhantomData, ptr, slice};
use wasm_bindgen::prelude::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = globalThis, js_name = __dosletV86TableBase)]
    fn table_base() -> std::result::Result<u32, JsValue>;
    #[wasm_bindgen(catch, js_namespace = globalThis, js_name = __dosletV86Compile)]
    fn compile_module(pointer: u32, length: u32, index: u32) -> std::result::Result<(), JsValue>;
    #[wasm_bindgen(catch, js_namespace = globalThis, js_name = __dosletV86Run)]
    fn run_core(budget: u32, translated: u32, interrupt: i32) -> std::result::Result<u32, JsValue>;
    #[wasm_bindgen(catch, js_namespace = globalThis, js_name = __dosletV86Clear)]
    fn clear_module(index: u32) -> std::result::Result<(), JsValue>;
}

// A browser worker can own one CPU. No pointer is shared with another worker;
// device borrows are installed only for one synchronous execution operation.
static mut ACTIVE: bool = false;
static mut DEVICES: Option<devices::Borrow> = None;
static mut DEVICE_ERROR: Option<BusError> = None;
static mut PENDING: Option<Pending> = None;
static mut MICROSECONDS: u64 = 0;
static mut INVALIDATED: u64 = 0;
static mut DIAGNOSTIC: Option<String> = None;
struct Pending {
    index: u32,
    address: u32,
    flags: u32,
    bytes: Vec<u8>,
}

pub struct Core;
impl Core {
    pub fn compile(bytes: &[u8]) -> Result<Self> {
        if !bytes.is_empty() {
            return Err(
                "browser v86 uses the linked pinned core; replacing its module is unsupported"
                    .into(),
            );
        }
        Ok(Self)
    }
    pub fn instantiate<B: CpuBus + Send + 'static>(&self, _: B, size: u32) -> Result<Cpu<B>> {
        if !(0x10_0000..=0x400_0000).contains(&size) || size & 4095 != 0 {
            return Err("v86 RAM must be 1-64 MiB, aligned to 4 KiB".into());
        }
        unsafe {
            if ACTIVE {
                return Err("one v86 machine is allowed per browser worker".into());
            }
            cpu::WASM_TABLE_OFFSET = table_base().map_err(js_error)?;
            // This page belongs exclusively to v86; reset recreates the same
            // zeroed register image as a fresh standalone WASM instance.
            ptr::write_bytes(ptr::without_provenance_mut::<u8>(1), 0, 4095);
            MICROSECONDS = 0;
            INVALIDATED = 0;
            DIAGNOSTIC = None;
            let base = doslet::doslet_init(size);
            if base == 0 {
                return Err("v86 RAM allocation failed".into());
            }
            jit::set_jit_config(1, 1);
            jit::set_jit_config(3, 32);
            ACTIVE = true;
            Ok(Cpu {
                base,
                size,
                shadow: false,
                usable: true,
                exception_exits: false,
                stats: Statistics::default(),
                initial_jit_entries: doslet_v86_module::cpu::call_indirect::doslet_jit_entries(),
                _bus: PhantomData,
            })
        }
    }
}
fn js_error(error: JsValue) -> Box<dyn std::error::Error> {
    format!("browser v86 JIT: {error:?}").into()
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    Interpreter,
    Jit,
}
pub struct Cpu<B: 'static> {
    base: u32,
    size: u32,
    shadow: bool,
    usable: bool,
    exception_exits: bool,
    stats: Statistics,
    initial_jit_entries: u32,
    _bus: PhantomData<B>,
}
impl<B> Drop for Cpu<B> {
    fn drop(&mut self) {
        // A trap can interrupt an upstream mutex guard. That worker must be
        // recreated; touching its CPU again would reuse incomplete JIT state.
        if !self.usable {
            return;
        }
        unsafe {
            jit::jit_clear_cache_js();
            jit::embedded_release();
            std::alloc::dealloc(
                self.base as *mut u8,
                std::alloc::Layout::from_size_align(self.size as usize, 4096).unwrap(),
            );
            memory::mem8 = ptr::null_mut();
            ACTIVE = false;
            PENDING = None;
        }
    }
}
impl<B: CpuBus + Send + 'static> Cpu<B> {
    pub fn physical_ram(&self) -> &[u8] {
        // The exclusive CPU owner retains this allocation until Drop. Guest
        // execution requires &mut self and cannot overlap this returned borrow.
        unsafe { slice::from_raw_parts(self.base as *const u8, self.size as usize) }
    }
    pub fn physical_ram_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.base as *mut u8, self.size as usize) }
    }
    fn bytes(&self, offset: usize, len: usize) -> &[u8] {
        assert!(offset != 0 && offset + len <= core::arch::wasm32::memory_size(0) * 65536);
        unsafe { slice::from_raw_parts(offset as *const u8, len) }
    }
    fn write(&mut self, offset: usize, bytes: &[u8]) {
        assert!(offset != 0 && offset + bytes.len() <= core::arch::wasm32::memory_size(0) * 65536);
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), offset as *mut u8, bytes.len()) }
    }
    fn word(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.bytes(offset, 4).try_into().unwrap())
    }
    pub fn invalidate(&mut self, start: u32, end: u32) -> Result<()> {
        jit::jit_dirty_cache(start, end);
        Ok(())
    }
    pub fn set_profiling(&mut self, _: bool) {}
    pub fn set_time_microseconds(&mut self, value: u64) {
        unsafe {
            MICROSECONDS = value;
        }
    }
    pub fn set_exception_exits(&mut self, enabled: bool) -> Result<()> {
        unsafe {
            doslet::doslet_exception_exits(u32::from(enabled));
        }
        self.exception_exits = enabled;
        Ok(())
    }
    pub fn set_a20(&mut self, enabled: bool) -> Result<()> {
        unsafe {
            doslet::doslet_set_a20(u32::from(enabled));
        }
        Ok(())
    }
    pub fn set_machine_boundaries(&mut self, enabled: bool) -> Result<()> {
        unsafe {
            doslet::doslet_machine_boundaries(u32::from(enabled));
        }
        Ok(())
    }
    pub fn set_wait_boundary(&mut self, address: Option<u32>) -> Result<()> {
        unsafe {
            doslet::doslet_wait_boundary(address.unwrap_or(u32::MAX));
        }
        Ok(())
    }
    pub fn wait_for_compilation(&mut self) -> Result<()> {
        self.finish_compilation()
    }
    fn finish_compilation(&mut self) -> Result<()> {
        let pending = unsafe { (&raw mut PENDING).as_mut().unwrap().take() };
        if let Some(pending) = pending {
            compile_module(
                pending.bytes.as_ptr() as u32,
                pending.bytes.len() as u32,
                pending.index,
            )
            .map_err(js_error)?;
            jit::embedded_finalize(pending.index as u16, pending.address, pending.flags);
            self.stats.compiled_modules += 1;
        }
        Ok(())
    }
    pub fn run_with_devices<H: devices::DeviceHost>(
        &mut self,
        host: &mut H,
        budget: u32,
        execution: Execution,
        interrupt: Option<u8>,
    ) -> Result<RunResult> {
        if !self.usable {
            return Err("v86 trapped; recreate the browser worker".into());
        }
        self.finish_compilation()?;
        let initial = self.word(664);
        unsafe {
            DEVICES = Some(devices::Borrow::new(host));
            DEVICE_ERROR = None;
        }
        // The JS boundary catches a WASM trap before it can leave this scoped
        // device borrow dangling. No guest RAM or CPU image crosses it.
        let executed = run_core(
            budget.min(doslet_v86::MAX_BATCH_STEPS),
            u32::from(execution == Execution::Jit),
            interrupt.map_or(-1, i32::from),
        );
        unsafe {
            DEVICES = None;
        }
        self.usable = executed.is_ok();
        if let Some(error) = unsafe { (&raw mut DEVICE_ERROR).as_mut().unwrap().take() } {
            return Err(error.into());
        }
        let completed = executed.map_err(|error| {
            let diagnostic = unsafe {
                (&raw const DIAGNOSTIC)
                    .as_ref()
                    .unwrap()
                    .as_deref()
                    .unwrap_or("")
            };
            format!("v86 stopped: {error:?} {diagnostic}; reload the browser worker")
        })?;
        if interrupt.is_none() {
            self.shadow = completed & (1 << 31) != 0;
        }
        self.stats.run_calls += 1;
        self.finish_compilation()?;
        let vector = unsafe { doslet::doslet_take_interrupt() };
        let fault = if self.exception_exits {
            unsafe { doslet::doslet_take_exception() }
        } else {
            0
        };
        Ok(RunResult {
            steps: if interrupt.is_some() {
                1
            } else {
                self.word(664).wrapping_sub(initial)
            },
            halted: self.word(616) & 0xff != 0,
            interrupt: (vector != 0).then_some(vector.wrapping_sub(1) as u8),
            exception: (fault & 0xff != 0).then(|| doslet_v86::Exception {
                vector: ((fault & 0xff) - 1) as u8,
                error_code: (fault & 0x100 != 0)
                    .then(|| unsafe { doslet::doslet_exception_error() as u32 }),
            }),
            interrupt_shadow: self.shadow,
        })
    }
    pub fn statistics(&mut self) -> Result<Statistics> {
        self.stats.jit_entries = u64::from(
            doslet_v86_module::cpu::call_indirect::doslet_jit_entries()
                .wrapping_sub(self.initial_jit_entries),
        );
        self.stats.invalidated_modules = unsafe { INVALIDATED };
        Ok(self.stats)
    }
    pub fn registers(&mut self) -> Result<Registers> {
        let eflags = unsafe { cpu::get_eflags() as u32 };
        Ok(Registers {
            gpr: core::array::from_fn(|i| self.word(64 + i * 4)),
            eip: self.word(556).wrapping_sub(self.word(740)),
            eflags,
            segments: core::array::from_fn(|i| {
                u16::from_le_bytes(self.bytes(668 + i * 2, 2).try_into().unwrap())
            }),
        })
    }
    pub fn context(&mut self) -> Result<Context> {
        let registers = self.registers()?;
        let fpu = unsafe { doslet::doslet_fpu_image() } as usize;
        let mut segments = core::array::from_fn(|i| Segment {
            selector: u16::from_le_bytes(self.bytes(668 + i * 2, 2).try_into().unwrap()),
            base: self.word(736 + i * 4),
            limit: self.word(768 + i * 4),
            access: self.bytes(512 + i, 1)[0],
            default32: match i {
                1 => self.word(804) != 0,
                2 => self.word(808) != 0,
                _ => false,
            },
            usable: self.bytes(724 + i, 1)[0] == 0,
        });
        for segment in &mut segments[6..] {
            if segment.selector & !3 == 0 {
                *segment = Segment::default();
            }
        }
        Ok(Context {
            registers,
            segments,
            cr: core::array::from_fn(|i| self.word(580 + i * 4)),
            gdtr: (self.word(576), self.word(572) as u16),
            idtr: (self.word(568), self.word(564) as u16),
            cpl: self.bytes(612, 1)[0],
            protected: self.word(800) != 0,
            code32: self.word(804) != 0,
            stack32: self.word(808) != 0,
            halted: self.word(616) & 0xff != 0,
            interrupt_shadow: self.shadow,
            fpu: self.bytes(fpu, 108).try_into().unwrap(),
        })
    }
    pub fn restore_context(&mut self, state: &Context) -> Result<()> {
        unsafe {
            doslet::doslet_set_interrupt_shadow(u32::from(state.interrupt_shadow));
        }
        self.shadow = state.interrupt_shadow;
        for (i, v) in state.registers.gpr.iter().enumerate() {
            self.write(64 + i * 4, &v.to_le_bytes());
        }
        for (i, s) in state.segments.iter().enumerate() {
            self.write(668 + i * 2, &s.selector.to_le_bytes());
            self.write(736 + i * 4, &s.base.to_le_bytes());
            self.write(768 + i * 4, &s.limit.to_le_bytes());
            self.write(512 + i, &[s.access]);
            self.write(724 + i, &[u8::from(!s.usable)]);
        }
        for (offset, value) in [
            (
                556,
                state.segments[1].base.wrapping_add(state.registers.eip),
            ),
            (120, state.registers.eflags),
            (100, 0),
            (576, state.gdtr.0),
            (572, u32::from(state.gdtr.1)),
            (568, state.idtr.0),
            (564, u32::from(state.idtr.1)),
            (800, u32::from(state.protected)),
            (804, u32::from(state.code32)),
            (808, u32::from(state.stack32)),
            (616, u32::from(state.halted)),
        ] {
            self.write(offset, &value.to_le_bytes());
        }
        self.write(612, &[state.cpl]);
        self.write(
            1128,
            &[u8::from(
                state.segments[doslet_v86::TASK_SEGMENT].access & 8 != 0,
            )],
        );
        for (i, v) in state.cr.iter().enumerate() {
            self.write(580 + i * 4, &v.to_le_bytes());
        }
        let pointer = unsafe { doslet::doslet_fpu_image() };
        self.write(pointer as usize, &state.fpu);
        unsafe {
            doslet::doslet_rebind_context();
        }
        Ok(())
    }
}

fn device(request: DeviceRequest) -> u32 {
    unsafe {
        let host = (&raw mut DEVICES)
            .as_mut()
            .unwrap()
            .as_mut()
            .expect("v86 device callback outside execution");
        let ram = slice::from_raw_parts_mut(
            memory::mem8,
            *doslet_v86_module::cpu::global_pointers::memory_size as usize,
        );
        match host.access(ram, request) {
            Ok(value) => value,
            Err(error) => {
                DEVICE_ERROR = Some(error);
                wasm_bindgen::throw_str("v86 device access failed")
            }
        }
    }
}
macro_rules! read_port {
    ($name:ident,$width:expr) => {
        #[unsafe(no_mangle)]
        extern "C" fn $name(port: i32) -> i32 {
            device(DeviceRequest::ReadPort {
                port: port as u16,
                width: $width,
            }) as i32
        }
    };
}
macro_rules! write_port {
    ($name:ident,$width:expr) => {
        #[unsafe(no_mangle)]
        extern "C" fn $name(port: i32, value: i32) {
            device(DeviceRequest::WritePort {
                port: port as u16,
                width: $width,
                value: value as u32,
            });
        }
    };
}
read_port!(io_port_read8, 1);
read_port!(io_port_read16, 2);
read_port!(io_port_read32, 4);
write_port!(io_port_write8, 1);
write_port!(io_port_write16, 2);
write_port!(io_port_write32, 4);
#[unsafe(no_mangle)]
extern "C" fn mmap_read8(address: u32) -> i32 {
    device(DeviceRequest::ReadMemory { address, width: 1 }) as i32
}
#[unsafe(no_mangle)]
extern "C" fn mmap_read32(address: u32) -> i32 {
    device(DeviceRequest::ReadMemory { address, width: 4 }) as i32
}
macro_rules! write_memory {
    ($name:ident,$width:expr) => {
        #[unsafe(no_mangle)]
        extern "C" fn $name(address: u32, value: i32) {
            device(DeviceRequest::WriteMemory {
                address,
                width: $width,
                value: value as u32,
            });
        }
    };
}
write_memory!(mmap_write8, 1);
write_memory!(mmap_write16, 2);
write_memory!(mmap_write32, 4);
#[unsafe(no_mangle)]
extern "C" fn mmap_write64(address: u32, v0: i32, v1: i32) {
    mmap_write32(address, v0);
    mmap_write32(address.wrapping_add(4), v1);
}
#[unsafe(no_mangle)]
extern "C" fn mmap_write128(address: u32, v0: i32, v1: i32, v2: i32, v3: i32) {
    mmap_write64(address, v0, v1);
    mmap_write64(address.wrapping_add(8), v2, v3);
}
#[unsafe(no_mangle)]
extern "C" fn codegen_finalize(index: u32, address: u32, flags: u32, pointer: u32, length: u32) {
    assert!(index < 900 && length <= 8 * 1024 * 1024);
    unsafe {
        assert!((&raw const PENDING).as_ref().unwrap().is_none());
        PENDING = Some(Pending {
            index,
            address,
            flags,
            bytes: slice::from_raw_parts(pointer as *const u8, length as usize).to_vec(),
        });
    }
}
#[unsafe(no_mangle)]
extern "C" fn jit_clear_func(index: u32) {
    // A callback failure is fatal rather than permitting a stale compiled block.
    clear_module(index).expect("v86 function table reset");
    unsafe {
        INVALIDATED += 1;
    }
}
#[unsafe(no_mangle)]
extern "C" fn cpu_exception_hook(_: i32) -> bool {
    false
}
#[unsafe(no_mangle)]
extern "C" fn cpu_event_halt() {}
#[unsafe(no_mangle)]
extern "C" fn stop_idling() {}
#[unsafe(no_mangle)]
extern "C" fn microtick() -> f64 {
    unsafe { MICROSECONDS as f64 / 1000.0 }
}
#[unsafe(no_mangle)]
extern "C" fn run_hardware_timers(_: bool, _: f64) -> f64 {
    panic!("v86 hardware timers must use the Rust machine scheduler")
}
#[unsafe(no_mangle)]
extern "C" fn get_rand_int() -> i32 {
    panic!("unsupported v86 random instruction")
}
#[unsafe(no_mangle)]
extern "C" fn log_from_wasm(pointer: u32, length: u32) {
    unsafe {
        let bytes = slice::from_raw_parts(pointer as *const u8, length.min(4096) as usize);
        DIAGNOSTIC = Some(String::from_utf8_lossy(bytes).into_owned());
    }
}
#[unsafe(no_mangle)]
extern "C" fn console_log_from_wasm(pointer: u32, length: u32) {
    log_from_wasm(pointer, length);
}
#[unsafe(no_mangle)]
extern "C" fn dbg_trace_from_wasm() {}
