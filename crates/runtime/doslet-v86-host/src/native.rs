//! Native host for the pinned v86 CPU and its generated WASM modules.

use doslet_bus::CpuBus;
use doslet_v86::{
    ABI_VERSION, MAX_BATCH_STEPS, MAX_COMPILED_MODULES, Registers, RunResult, Statistics,
    TABLE_OFFSET,
};
use wasmtime::{
    Caller, Engine, Extern, ExternType, Func, Instance, Linker, Memory, Module, Ref, RefType,
    Store, StoreLimits, StoreLimitsBuilder, Table, TableType, Val,
};

type Result<T> = std::result::Result<T, wasmtime::Error>;
#[path = "compiler.rs"]
mod compiler;
#[path = "context.rs"]
mod context;
use crate::devices;
#[path = "engine_config.rs"]
mod engine_config;

struct Pending {
    index: u32,
    address: i32,
    flags: i32,
    bytes: Vec<u8>,
}

struct State<B> {
    bus: B,
    pending: Option<Pending>,
    table: Option<Table>,
    stats: Statistics,
    microseconds: u64,
    diagnostic: String,
    iret_diagnostic: String,
    limits: StoreLimits,
    devices: Option<devices::Borrow>,
    ram_base: u32,
    ram_size: u32,
    memory: Option<Memory>,
}

/// Compiled upstream CPU, reusable across independent instances. No native
/// pointers or mutable guest state are shared between instances.
#[derive(Clone)]
pub struct Core {
    engine: Engine,
    module: Module,
}

impl Core {
    /// An empty slice selects the embedded core; other bytes are validated WASM.
    /// External serialized native code is never accepted here.
    pub fn compile(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Self::embedded();
        }
        let engine = Engine::new(&engine_config::config())?;
        let module = Module::new(&engine, bytes)?;
        Ok(Self { engine, module })
    }

    /// Load the trusted build artifact once; instances retain independent RAM.
    pub fn embedded() -> Result<Self> {
        static CORE: std::sync::OnceLock<std::result::Result<Core, String>> =
            std::sync::OnceLock::new();
        CORE.get_or_init(|| Self::load_embedded().map_err(|error| error.to_string()))
            .clone()
            .map_err(wasmtime::Error::msg)
    }

    #[allow(unsafe_code)]
    fn load_embedded() -> Result<Self> {
        let engine = Engine::new(&engine_config::config())?;
        // SAFETY: These immutable bytes are emitted by Engine::precompile_module
        // in this package's build.rs, with the same pinned Wasmtime and config.
        // No file, user input, or downloaded cache enters this deserializer.
        let module = unsafe {
            Module::deserialize(
                &engine,
                include_bytes!(concat!(env!("OUT_DIR"), "/doslet-v86.cwasm")),
            )?
        };
        Ok(Self { engine, module })
    }

    pub fn instantiate<B: CpuBus + Send + 'static>(&self, bus: B, ram_size: u32) -> Result<Cpu<B>> {
        if !(0x10_0000..=0x400_0000).contains(&ram_size) || ram_size & 0xfff != 0 {
            return Err(wasmtime::Error::msg(
                "v86 RAM must be 1-64 MiB, aligned to 4 KiB",
            ));
        }
        let mut store = Store::new(
            &self.engine,
            State {
                bus,
                pending: None,
                table: None,
                stats: Statistics::default(),
                microseconds: 0,
                diagnostic: String::new(),
                iret_diagnostic: String::new(),
                devices: None,
                ram_base: 0,
                ram_size,
                memory: None,
                limits: StoreLimitsBuilder::new()
                    .memory_size(256 * 1024 * 1024)
                    .table_elements(2048)
                    .instances(MAX_COMPILED_MODULES as usize + 1)
                    .build(),
            },
        );
        store.limiter(|state| &mut state.limits);
        let mut linker = Linker::new(&self.engine);
        for import in self.module.imports() {
            let item: Extern = match import.ty() {
                ExternType::Table(ty) => {
                    if ty.minimum() > 2048 {
                        return Err(wasmtime::Error::msg("v86 table exceeds its limit"));
                    }
                    let table = Table::new(
                        &mut store,
                        TableType::new(RefType::FUNCREF, ty.minimum() as u32, Some(2048)),
                        Ref::Func(None),
                    )?;
                    let remaining = 2048 - table.size(&store);
                    table.grow(&mut store, remaining, Ref::Func(None))?;
                    store.data_mut().table = Some(table);
                    table.into()
                }
                ExternType::Func(ty) => {
                    let name = import.name().to_owned();
                    Func::new(&mut store, ty, move |caller, args, results| {
                        callback(&name, caller, args, results)
                    })
                    .into()
                }
                _ => {
                    return Err(wasmtime::Error::msg(format!(
                        "unsupported v86 import {}.{}",
                        import.module(),
                        import.name()
                    )));
                }
            };
            linker.define(&store, import.module(), import.name(), item)?;
        }
        let instance = linker.instantiate(&mut store, &self.module)?;
        let abi = instance
            .get_typed_func::<(), u32>(&mut store, "doslet_abi_version")?
            .call(&mut store, ())?;
        if abi != ABI_VERSION {
            return Err(wasmtime::Error::msg("unsupported v86 ABI"));
        }
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| wasmtime::Error::msg("missing v86 memory"))?;
        store.data_mut().memory = Some(memory);
        let ram_base = instance
            .get_typed_func::<u32, u32>(&mut store, "doslet_init")?
            .call(&mut store, ram_size)?;
        let jit_config = instance.get_typed_func::<(u32, u32), ()>(&mut store, "set_jit_config")?;
        // Browser v86 can afford large multi-page compilation units. Native
        // Cranelift spends much longer allocating their registers; keep cold
        // compilation latency bounded with one-page units and fewer extras.
        jit_config.call(&mut store, (1, 1))?;
        jit_config.call(&mut store, (3, 32))?;
        let ram_end = u64::from(ram_base) + u64::from(ram_size);
        if ram_base < 4096 || ram_end > memory.data_size(&store) as u64 {
            return Err(wasmtime::Error::msg("invalid v86 guest RAM layout"));
        }
        store.data_mut().ram_base = ram_base;
        Ok(Cpu {
            store,
            instance,
            memory,
            ram_base,
            ram_size,
            usable: true,
            profiling: false,
            background: false,
            exception_exits: false,
            interrupt_shadow: false,
            compiler: None,
        })
    }
}

/// The v86 instance owns its RAM. `bus` supplies ports and mapped device memory;
/// ordinary guest RAM is exposed by borrowed views of that same WASM memory.
pub struct Cpu<B: 'static> {
    store: Store<State<B>>,
    instance: Instance,
    memory: Memory,
    ram_base: u32,
    ram_size: u32,
    usable: bool,
    profiling: bool,
    background: bool,
    exception_exits: bool,
    interrupt_shadow: bool,
    compiler: Option<compiler::Compiler>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Execution {
    Interpreter,
    Jit,
}

impl<B: CpuBus + Send + 'static> Cpu<B> {
    pub fn set_profiling(&mut self, enabled: bool) {
        self.profiling = enabled;
    }
    /// Scope an external device owner around one complete CPU operation. RAM
    /// remains in this CPU; no guest-memory image is copied into the host.
    fn with_devices<H: devices::DeviceHost, T>(
        &mut self,
        host: &mut H,
        execute: impl FnOnce(&mut Self) -> T,
    ) -> T {
        assert!(
            self.store.data().devices.is_none(),
            "nested device installation"
        );
        self.store.data_mut().devices = Some(devices::Borrow::new(host));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(self)));
        self.store.data_mut().devices = None;
        match result {
            Ok(value) => value,
            Err(panic) => {
                self.usable = false;
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Run with a borrowed machine bus. The borrow cannot escape this call.
    pub fn run_with_devices<H: devices::DeviceHost>(
        &mut self,
        host: &mut H,
        budget: u32,
        execution: Execution,
        interrupt: Option<u8>,
    ) -> Result<RunResult> {
        self.with_devices(host, |cpu| {
            if let Some(vector) = interrupt {
                let accepted = cpu.inject_interrupt(vector)?;
                if vector == 9 && trace_keys() {
                    eprintln!("[INFO doslet] v86 keyboard: IRQ1 accepted={accepted}");
                }
                Ok(RunResult {
                    steps: 1,
                    halted: false,
                    interrupt: None,
                    exception: None,
                    interrupt_shadow: cpu.interrupt_shadow,
                })
            } else {
                cpu.run(budget, execution)
            }
        })
    }

    pub fn physical_ram(&self) -> &[u8] {
        let start = self.ram_base as usize;
        &self.memory.data(&self.store)[start..start + self.ram_size as usize]
    }

    /// Machine-owner access. Invalidate changed pages before the next run.
    pub fn physical_ram_mut(&mut self) -> &mut [u8] {
        let start = self.ram_base as usize;
        &mut self.memory.data_mut(&mut self.store)[start..start + self.ram_size as usize]
    }

    pub fn invalidate(&mut self, start: u32, end: u32) -> Result<()> {
        self.instance
            .get_typed_func::<(u32, u32), ()>(&mut self.store, "jit_dirty_cache")?
            .call(&mut self.store, (start, end))
    }
    pub fn bus(&self) -> &B {
        &self.store.data().bus
    }
    pub fn bus_mut(&mut self) -> &mut B {
        &mut self.store.data_mut().bus
    }

    /// Last bounded upstream diagnostic, including panic-hook output.
    pub fn diagnostic(&self) -> &str {
        &self.store.data().diagnostic
    }

    /// First rejected IRET frame, retained even if a later panic replaces the
    /// ordinary upstream diagnostic. Captured only on an architectural fault.
    pub fn iret_diagnostic(&self) -> &str {
        &self.store.data().iret_diagnostic
    }

    pub fn set_exception_exits(&mut self, enabled: bool) -> Result<()> {
        if self.exception_exits == enabled {
            return Ok(());
        }
        self.instance
            .get_typed_func::<u32, ()>(&mut self.store, "doslet_exception_exits")?
            .call(&mut self.store, u32::from(enabled))?;
        self.exception_exits = enabled;
        Ok(())
    }

    /// Explicit host-provided virtual time for RDTSC; execution never reads a
    /// wall clock or advances PC hardware implicitly.
    pub fn set_time_microseconds(&mut self, value: u64) {
        self.store.data_mut().microseconds = value;
    }

    /// Deliver a vector already acknowledged by the external PIC. A masked
    /// interrupt is declined so the machine can keep it pending.
    pub fn inject_interrupt(&mut self, vector: u8) -> Result<bool> {
        if !self.usable {
            return Err(wasmtime::Error::msg("v86 trapped; recreate the instance"));
        }
        let result = self
            .instance
            .get_typed_func::<u32, u32>(&mut self.store, "doslet_inject_interrupt")?
            .call(&mut self.store, u32::from(vector));
        if result.is_err() {
            self.usable = false;
        }
        result.map(|value| value != 0)
    }

    fn word(&self, offset: usize) -> u32 {
        u32::from_le_bytes(
            self.memory.data(&self.store)[offset..offset + 4]
                .try_into()
                .expect("validated state span"),
        )
    }

    pub fn registers(&mut self) -> Result<Registers> {
        let eflags = self
            .instance
            .get_typed_func::<(), u32>(&mut self.store, "get_eflags")?
            .call(&mut self.store, ())?;
        let bytes = self.memory.data(&self.store);
        let segments = core::array::from_fn(|index| {
            u16::from_le_bytes(
                bytes[668 + index * 2..670 + index * 2]
                    .try_into()
                    .expect("state span"),
            )
        });
        Ok(Registers {
            gpr: core::array::from_fn(|index| self.word(64 + index * 4)),
            eip: self.word(556).wrapping_sub(self.word(740)),
            eflags,
            segments,
        })
    }

    /// Reset to real mode and initialize the integer context. Reset discards
    /// compiled bindings, but retains the same guest RAM owner and host bus.
    pub fn reset_real_mode(&mut self, registers: Registers) -> Result<()> {
        if !self.usable {
            return Err(wasmtime::Error::msg("v86 trapped; recreate the instance"));
        }
        self.finish_compilation(true)?;
        self.instance
            .get_typed_func::<(), ()>(&mut self.store, "reset_cpu")?
            .call(&mut self.store, ())?;
        let segment = self
            .instance
            .get_typed_func::<(i32, i32), ()>(&mut self.store, "doslet_set_real_segment")?;
        for (index, value) in registers.segments.into_iter().enumerate() {
            segment.call(&mut self.store, (index as i32, i32::from(value)))?;
        }
        for (index, value) in registers.gpr.into_iter().enumerate() {
            self.memory
                .write(&mut self.store, 64 + index * 4, &value.to_le_bytes())?;
        }
        let linear_ip = u32::from(registers.segments[1]) * 16 + (registers.eip & 0xffff);
        self.memory
            .write(&mut self.store, 556, &linear_ip.to_le_bytes())?;
        self.instance
            .get_typed_func::<u32, ()>(&mut self.store, "update_eflags")?
            .call(&mut self.store, registers.eflags)?;
        Ok(())
    }

    pub fn ram(&self, address: u32, length: u32) -> Result<&[u8]> {
        let end = address
            .checked_add(length)
            .ok_or_else(|| wasmtime::Error::msg("RAM range overflow"))?;
        if end > self.ram_size || (address < 0xc0000 && end > 0xa0000 && length != 0) {
            return Err(wasmtime::Error::msg("range is outside ordinary v86 RAM"));
        }
        let start = self.ram_base as usize + address as usize;
        Ok(&self.memory.data(&self.store)[start..start + length as usize])
    }

    /// Host writes invalidate compiled pages before changing their source.
    pub fn write_ram(&mut self, address: u32, bytes: &[u8]) -> Result<()> {
        let length =
            u32::try_from(bytes.len()).map_err(|_| wasmtime::Error::msg("RAM write too large"))?;
        self.ram(address, length)?;
        if bytes.is_empty() {
            return Ok(());
        }
        self.instance
            .get_typed_func::<(u32, u32), ()>(&mut self.store, "jit_dirty_cache")?
            .call(&mut self.store, (address, address + length))?;
        self.memory.write(
            &mut self.store,
            self.ram_base as usize + address as usize,
            bytes,
        )?;
        Ok(())
    }

    /// A preparation operation; actual JIT use is reported separately.
    pub fn prepare_current(&mut self) -> Result<()> {
        if self.store.data().stats.compilation_exhausted {
            return Err(wasmtime::Error::msg(
                "v86 compilation limit exhausted; interpreter remains available",
            ));
        }
        self.finish_compilation(true)?;
        let address = self.word(556) as i32;
        self.instance
            .get_typed_func::<i32, ()>(&mut self.store, "doslet_force_compile")?
            .call(&mut self.store, address)?;
        self.finish_compilation(true)
    }

    /// Setup/diagnostics only: wait for the one outstanding job. Ordinary
    /// execution always polls and continues through v86's interpreter.
    pub fn wait_for_compilation(&mut self) -> Result<()> {
        self.finish_compilation(true)
    }

    fn finish_compilation(&mut self, blocking: bool) -> Result<()> {
        let completed = self
            .compiler
            .as_mut()
            .map(|compiler| compiler.poll(blocking))
            .transpose()?
            .flatten();
        if let Some(completed) = completed {
            self.store.data_mut().stats.compilation_nanos += completed.nanos;
            self.install_compilation(completed.pending, completed.module)?;
        }
        let Some(pending) = self.store.data_mut().pending.take() else {
            return Ok(());
        };
        if self.background && !blocking {
            if self.compiler.is_none() {
                self.compiler = Some(compiler::Compiler::new(self.store.engine().clone())?);
            }
            return self
                .compiler
                .as_mut()
                .expect("created compiler")
                .submit(pending);
        }
        let started = self.profiling.then(std::time::Instant::now);
        let module = Module::new(self.store.engine(), &pending.bytes)?;
        if let Some(started) = started {
            self.store.data_mut().stats.compilation_nanos += started.elapsed().as_nanos() as u64;
        }
        self.install_compilation(pending, module)
    }

    fn install_compilation(&mut self, pending: Pending, module: Module) -> Result<()> {
        let mut linker = Linker::new(self.store.engine());
        linker.define(&self.store, "e", "m", self.memory)?;
        let exports: Vec<_> = self
            .instance
            .exports(&mut self.store)
            .map(|export| (export.name().to_owned(), export.into_extern()))
            .collect();
        for (name, export) in exports {
            linker.define(&self.store, "e", &name, export)?;
        }
        let compiled = linker.instantiate(&mut self.store, &module)?;
        let function = compiled
            .get_func(&mut self.store, "f")
            .ok_or_else(|| wasmtime::Error::msg("missing generated v86 function"))?;
        let table = self
            .store
            .data()
            .table
            .ok_or_else(|| wasmtime::Error::msg("missing v86 function table"))?;
        table.set(
            &mut self.store,
            u64::from(TABLE_OFFSET + pending.index),
            Ref::Func(Some(function)),
        )?;
        self.instance
            .get_typed_func::<(u32, i32, i32), ()>(&mut self.store, "codegen_finalize_finished")?
            .call(
                &mut self.store,
                (pending.index, pending.address, pending.flags),
            )?;
        self.store.data_mut().stats.compiled_modules += 1;
        // Wasmtime retains generated instances for the Store's lifetime even
        // after table eviction. Bound that retention and keep interpretation.
        if self.store.data().stats.compiled_modules >= MAX_COMPILED_MODULES {
            self.instance
                .get_typed_func::<(u32, u32), ()>(&mut self.store, "set_jit_config")?
                .call(&mut self.store, (0, 1))?;
            self.store.data_mut().stats.compilation_exhausted = true;
        }
        Ok(())
    }

    /// Generated basic blocks must fit the remaining budget. Short remainders
    /// use v86's interpreter. STI may include its interrupt-shadow instruction.
    pub fn run(&mut self, budget: u32, execution: Execution) -> Result<RunResult> {
        if !self.usable {
            return Err(wasmtime::Error::msg("v86 trapped; recreate the instance"));
        }
        let result = self.run_inner(budget.min(MAX_BATCH_STEPS), execution);
        if result.is_err() {
            self.usable = false;
        }
        result.map_err(|mut error| {
            let state = self.store.data();
            if !state.diagnostic.is_empty() {
                error = error.context(state.diagnostic.clone());
            }
            if !state.iret_diagnostic.is_empty() && state.iret_diagnostic != state.diagnostic {
                error = error.context(state.iret_diagnostic.clone());
            }
            error
        })
    }

    fn run_inner(&mut self, budget: u32, execution: Execution) -> Result<RunResult> {
        let started = self.profiling.then(std::time::Instant::now);
        self.store.data_mut().stats.run_calls += 1;
        self.finish_compilation(false)?;
        let initial = self.word(664);
        if budget != 0 {
            let completed = self
                .instance
                .get_typed_func::<(u32, u32), u32>(&mut self.store, "doslet_run")?
                .call(
                    &mut self.store,
                    (budget, u32::from(execution == Execution::Jit)),
                )?;
            self.interrupt_shadow = completed & (1 << 31) != 0;
            self.finish_compilation(false)?;
        }
        let interrupt = self
            .instance
            .get_typed_func::<(), u32>(&mut self.store, "doslet_take_interrupt")?
            .call(&mut self.store, ())?;
        let exception = if self.exception_exits {
            self.instance
                .get_typed_func::<(), u32>(&mut self.store, "doslet_take_exception")?
                .call(&mut self.store, ())?
        } else {
            0
        };
        let exception = if exception & 0xff != 0 {
            Some(doslet_v86::Exception {
                vector: ((exception & 0xff) - 1) as u8,
                error_code: if exception & 0x100 != 0 {
                    Some(
                        self.instance
                            .get_typed_func::<(), u32>(&mut self.store, "doslet_exception_error")?
                            .call(&mut self.store, ())?,
                    )
                } else {
                    None
                },
            })
        } else {
            None
        };
        if let Some(started) = started {
            self.store.data_mut().stats.run_nanos += started.elapsed().as_nanos() as u64;
        }
        Ok(RunResult {
            steps: self.word(664).wrapping_sub(initial),
            halted: self.word(616) & 0xff != 0,
            interrupt: (interrupt != 0).then_some(interrupt.wrapping_sub(1) as u8),
            exception,
            interrupt_shadow: self.interrupt_shadow,
        })
    }

    pub fn statistics(&mut self) -> Result<Statistics> {
        let entries = self
            .instance
            .get_typed_func::<(), u32>(&mut self.store, "doslet_jit_entries")?
            .call(&mut self.store, ())?;
        let mut stats = self.store.data().stats;
        stats.jit_entries = u64::from(entries);
        stats.compilation_pending = self
            .compiler
            .as_ref()
            .is_some_and(compiler::Compiler::is_busy);
        Ok(stats)
    }
}

fn trace_keys() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("DOSLET_TRACE_KEYS").is_some())
}

fn callback<B: CpuBus>(
    name: &str,
    mut caller: Caller<'_, State<B>>,
    args: &[Val],
    results: &mut [Val],
) -> Result<()> {
    let value = |index: usize| args[index].i32().expect("validated v86 import") as u32;
    if caller.data().devices.is_some() {
        use doslet_v86::DeviceRequest::*;
        if matches!(name, "mmap_write64" | "mmap_write128") {
            let memory = match caller.data().memory {
                Some(memory) => memory,
                None => caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| wasmtime::Error::msg("missing v86 memory"))?,
            };
            let (bytes, state) = memory.data_and_store_mut(&mut caller);
            let start = state.ram_base as usize;
            let ram = &mut bytes[start..start + state.ram_size as usize];
            for index in 1..args.len() {
                state.devices.as_mut().expect("scoped device host").access(
                    ram,
                    WriteMemory {
                        address: value(0).wrapping_add((index as u32 - 1) * 4),
                        width: 4,
                        value: value(index),
                    },
                )?;
            }
            return Ok(());
        }
        let request = match name {
            "io_port_read8" => Some(ReadPort {
                port: value(0) as u16,
                width: 1,
            }),
            "io_port_read16" => Some(ReadPort {
                port: value(0) as u16,
                width: 2,
            }),
            "io_port_read32" => Some(ReadPort {
                port: value(0) as u16,
                width: 4,
            }),
            "io_port_write8" => Some(WritePort {
                port: value(0) as u16,
                width: 1,
                value: value(1),
            }),
            "io_port_write16" => Some(WritePort {
                port: value(0) as u16,
                width: 2,
                value: value(1),
            }),
            "io_port_write32" => Some(WritePort {
                port: value(0) as u16,
                width: 4,
                value: value(1),
            }),
            "mmap_read8" => Some(ReadMemory {
                address: value(0),
                width: 1,
            }),
            "mmap_read32" => Some(ReadMemory {
                address: value(0),
                width: 4,
            }),
            "mmap_write8" => Some(WriteMemory {
                address: value(0),
                width: 1,
                value: value(1),
            }),
            "mmap_write16" => Some(WriteMemory {
                address: value(0),
                width: 2,
                value: value(1),
            }),
            "mmap_write32" => Some(WriteMemory {
                address: value(0),
                width: 4,
                value: value(1),
            }),
            _ => None,
        };
        if let Some(request) = request {
            let memory = match caller.data().memory {
                Some(memory) => memory,
                None => caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| wasmtime::Error::msg("missing v86 memory"))?,
            };
            let (bytes, state) = memory.data_and_store_mut(&mut caller);
            let start = state.ram_base as usize;
            let ram = &mut bytes[start..start + state.ram_size as usize];
            let result = state
                .devices
                .as_mut()
                .expect("scoped device host")
                .access(ram, request)?;
            if matches!(request, ReadPort { port: 0x60, .. }) && trace_keys() {
                eprintln!("[INFO doslet] v86 keyboard: port60={result:02x}");
            }
            if !results.is_empty() {
                results[0] = Val::I32(result as i32);
            }
            return Ok(());
        }
    }
    let returned = match name {
        "io_port_read8" => Some(caller.data_mut().bus.read_port_u8(value(0) as u16)? as u32),
        "io_port_read16" => Some(caller.data_mut().bus.read_port_u16(value(0) as u16)? as u32),
        "io_port_read32" => Some(caller.data_mut().bus.read_port_u32(value(0) as u16)?),
        "io_port_write8" => {
            caller
                .data_mut()
                .bus
                .write_port_u8(value(0) as u16, value(1) as u8)?;
            None
        }
        "io_port_write16" => {
            caller
                .data_mut()
                .bus
                .write_port_u16(value(0) as u16, value(1) as u16)?;
            None
        }
        "io_port_write32" => {
            caller
                .data_mut()
                .bus
                .write_port_u32(value(0) as u16, value(1))?;
            None
        }
        "mmap_read8" => Some(caller.data_mut().bus.read_u8(value(0))? as u32),
        "mmap_read32" => Some(caller.data_mut().bus.read_u32(value(0))?),
        "mmap_write8" => {
            caller.data_mut().bus.write_u8(value(0), value(1) as u8)?;
            None
        }
        "mmap_write16" => {
            caller.data_mut().bus.write_u16(value(0), value(1) as u16)?;
            None
        }
        "mmap_write32" => {
            caller.data_mut().bus.write_u32(value(0), value(1))?;
            None
        }
        "mmap_write64" | "mmap_write128" => {
            for index in 1..args.len() {
                caller
                    .data_mut()
                    .bus
                    .write_u32(value(0).wrapping_add((index as u32 - 1) * 4), value(index))?;
            }
            None
        }
        "codegen_finalize" => {
            if caller.data().pending.is_some() {
                return Err(wasmtime::Error::msg("overlapping v86 compilation"));
            }
            if value(0) >= 900 || value(4) > 8 * 1024 * 1024 {
                return Err(wasmtime::Error::msg("v86 generated module exceeds limits"));
            }
            let memory = match caller.data().memory {
                Some(memory) => memory,
                None => caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| wasmtime::Error::msg("missing v86 memory"))?,
            };
            let offset = value(3) as usize;
            let end = offset
                .checked_add(value(4) as usize)
                .ok_or_else(|| wasmtime::Error::msg("v86 module range overflow"))?;
            let bytes = memory
                .data(&caller)
                .get(offset..end)
                .ok_or_else(|| wasmtime::Error::msg("v86 module outside memory"))?
                .to_vec();
            caller.data_mut().pending = Some(Pending {
                index: value(0),
                address: value(1) as i32,
                flags: value(2) as i32,
                bytes,
            });
            None
        }
        "jit_clear_func" => {
            if let Some(table) = caller.data().table {
                table.set(
                    &mut caller,
                    u64::from(TABLE_OFFSET + value(0)),
                    Ref::Func(None),
                )?;
            }
            caller.data_mut().stats.invalidated_modules += 1;
            None
        }
        "cpu_exception_hook" => Some(0),
        "cpu_event_halt" | "stop_idling" => None,
        "microtick" => {
            results[0] = Val::F64((caller.data().microseconds as f64 / 1000.0).to_bits());
            None
        }
        "log_from_wasm" | "console_log_from_wasm" => {
            let memory = match caller.data().memory {
                Some(memory) => memory,
                None => caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| wasmtime::Error::msg("missing v86 memory"))?,
            };
            let start = value(0) as usize;
            let end = start
                .checked_add(value(1).min(4096) as usize)
                .ok_or_else(|| wasmtime::Error::msg("v86 diagnostic range overflow"))?;
            let bytes = memory
                .data(&caller)
                .get(start..end)
                .ok_or_else(|| wasmtime::Error::msg("v86 diagnostic outside memory"))?;
            let diagnostic = String::from_utf8_lossy(bytes).into_owned();
            if diagnostic.starts_with("IRET fault:") && caller.data().iret_diagnostic.is_empty() {
                caller.data_mut().iret_diagnostic = diagnostic.clone();
            }
            caller.data_mut().diagnostic = diagnostic;
            None
        }
        // No implicit hardware timers, randomness, JS devices or silent stubs.
        _ => {
            return Err(wasmtime::Error::msg(format!(
                "unsupported v86 host operation: {name}"
            )));
        }
    };
    if let Some(value) = returned {
        results[0] = Val::I32(value as i32);
    }
    Ok(())
}
