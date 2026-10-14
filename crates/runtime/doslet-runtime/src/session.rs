use crate::{bios, demo::COLOR_BARS_COM};
#[cfg(test)]
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use doslet_bus::{BusError, MemoryBus};
use doslet_dos::files::ReadOnlyDrive;
use doslet_dos::memory::MemoryArena;
use doslet_dos::{ComError, MzError, ProcessMemory, load_com, load_mz, load_mz_stub};
use doslet_pc::PcMachine;
use doslet_vga::IndexedFrame;
use doslet_x86::{CpuError, Gpr16, Registers, StepOutcome, linear_address};
mod vesa;

const CONSOLE_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Ready,
    Running,
    Paused,
    Exited(u8),
    Halted,
    Faulted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    Machine(doslet_pc::MachineError),
    Load(ComError),
    LoadMz(MzError),
    Cpu(CpuError),
    Bus(BusError),
    UnsupportedService { vector: u8, ax: u16 },
    ConsoleFull,
    InvalidProgramName,
}
impl From<ComError> for RuntimeError {
    fn from(error: ComError) -> Self {
        Self::Load(error)
    }
}
impl From<MzError> for RuntimeError {
    fn from(error: MzError) -> Self {
        Self::LoadMz(error)
    }
}
impl From<CpuError> for RuntimeError {
    fn from(error: CpuError) -> Self {
        Self::Cpu(error)
    }
}
impl From<doslet_pc::MachineError> for RuntimeError {
    fn from(error: doslet_pc::MachineError) -> Self {
        match error {
            doslet_pc::MachineError::Cpu(error) => Self::Cpu(error),
            doslet_pc::MachineError::Bus(error) => Self::Bus(error),
            _ => Self::Machine(error),
        }
    }
}
impl From<BusError> for RuntimeError {
    fn from(error: BusError) -> Self {
        Self::Bus(error)
    }
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Machine(error) => error.fmt(f),
            Self::Load(error) => error.fmt(f),
            Self::LoadMz(error) => error.fmt(f),
            Self::Cpu(error) => error.fmt(f),
            Self::Bus(error) => error.fmt(f),
            Self::UnsupportedService { vector, ax } => {
                write!(f, "unsupported INT {vector:02X}h service AX={ax:04X}")
            }
            Self::ConsoleFull => f.write_str("DOS console exceeds its fixed 4096-byte capacity"),
            Self::InvalidProgramName => {
                f.write_str("DOS program name must contain 1..127 non-NUL ASCII bytes")
            }
        }
    }
}
impl core::error::Error for RuntimeError {}

pub struct Session {
    machine: PcMachine,
    status: SessionStatus,
    fault: Option<RuntimeError>,
    steps: u64,
    console: Vec<u8>,
    memory: MemoryArena,
    drive: ReadOnlyDrive,
    dta: (u16, u16),
    overlays: u32,
    break_check: bool,
}
impl Session {
    pub fn new(program: &[u8]) -> Result<Self, RuntimeError> {
        let mut machine = PcMachine::default();
        machine.cpu.registers = load_com(&mut machine.bus, program)?;
        Self::from_machine(machine)
    }
    pub fn from_mz(program: &[u8]) -> Result<Self, RuntimeError> {
        Self::mz_session(program, false)
    }
    /// Begin with the DOS startup stub. A guest extender may load the extended
    /// image through DOS services and enter the implemented protected-mode subset.
    pub fn from_mz_stub(program: &[u8]) -> Result<Self, RuntimeError> {
        Self::from_mz_stub_named(program, b"C:\\PROGRAM.EXE")
    }
    pub fn from_mz_stub_named(program: &[u8], name: &[u8]) -> Result<Self, RuntimeError> {
        if name.is_empty() || name.len() > 127 || !name.is_ascii() || name.contains(&0) {
            return Err(RuntimeError::InvalidProgramName);
        }
        let mut session = Self::mz_session(program, true)?;
        session.memory = session.memory.with_environment(0x0F00, 0x1000);
        session.memory.write_mcbs(&mut session.machine.bus)?;
        let bus = &mut session.machine.bus;
        let environment = b"PATH=C:\\\0\0\x01\0";
        for (offset, value) in environment
            .iter()
            .chain(name)
            .chain(core::iter::once(&0))
            .enumerate()
        {
            bus.write_u8(0x0F000 + offset as u32, *value)?;
        }
        bus.write_u16(0x1002C, 0x0F00)?;
        Ok(session)
    }
    fn mz_session(program: &[u8], dos_stub: bool) -> Result<Self, RuntimeError> {
        let mut machine = PcMachine::default();
        let memory = ProcessMemory {
            psp_segment: doslet_dos::loader::PSP_SEGMENT,
            end_segment: 0xA000,
        };
        let range = usize::from(memory.psp_segment) * 16..usize::from(memory.end_segment) * 16;
        if machine.bus.ram_view(range).is_none() {
            return Err(MzError::InsufficientMemory.into());
        }
        machine.cpu.registers = if dos_stub {
            load_mz_stub(&mut machine.bus, program, memory)?
        } else {
            load_mz(&mut machine.bus, program, memory)?
        };
        Self::from_machine(machine)
    }
    fn from_machine(mut machine: PcMachine) -> Result<Self, RuntimeError> {
        bios::install(&mut machine.bus)?;
        let process_end = machine.bus.read_u16(0x10002)?;
        let memory = MemoryArena::new(0x1000, process_end, 0xA000);
        memory.write_mcbs(&mut machine.bus)?;
        Ok(Self {
            machine,
            status: SessionStatus::Ready,
            fault: None,
            steps: 0,
            console: Vec::with_capacity(CONSOLE_CAPACITY),
            memory,
            drive: ReadOnlyDrive::new(),
            dta: (0x1000, 0x80),
            overlays: 0,
            break_check: false,
        })
    }
    /// Transfer a preloaded input drive into the session during setup.
    pub fn with_drive(mut self, drive: ReadOnlyDrive) -> Self {
        self.drive = drive;
        self
    }
    pub fn demo() -> Self {
        Self::new(COLOR_BARS_COM).expect("embedded COM demo must fit its segment")
    }
    pub fn status(&self) -> SessionStatus {
        self.status
    }
    pub fn fault(&self) -> Option<RuntimeError> {
        self.fault
    }
    pub fn steps(&self) -> u64 {
        self.steps
    }
    pub fn loaded_overlays(&self) -> u32 {
        self.overlays
    }
    pub fn drain_midi(&mut self, output: &mut [u8]) -> usize {
        self.machine.bus.mpu.drain(output)
    }
    pub fn drain_pcm(&mut self, output: &mut [[i16; 2]]) -> usize {
        self.machine.drain_pcm(output)
    }
    pub fn emulated_nanos(&self) -> u64 {
        self.machine.time().as_nanos()
    }
    pub fn cpu_halted(&self) -> bool {
        self.machine.cpu.is_halted()
    }
    #[cfg(feature = "profiling")]
    pub fn execution_stats(&self) -> doslet_pc::ExecutionStats {
        self.machine.execution_stats()
    }
    pub fn pending_midi(&self) -> usize {
        self.machine.bus.mpu.pending_output()
    }
    pub fn midi_stats(&self) -> (u64, u64, bool) {
        let mpu = &self.machine.bus.mpu;
        (
            mpu.command_count(),
            mpu.transmitted_bytes(),
            mpu.uart_mode(),
        )
    }
    pub fn file_access_stats(&self) -> (u64, u64, Option<&[u8]>) {
        self.drive.access_stats()
    }
    pub fn midi_file_request(&self) -> Option<(&[u8], u8, u16)> {
        self.drive.midi_request()
    }
    pub fn video_mode(&self) -> doslet_vga::VideoMode {
        self.machine.bus.vga.mode()
    }
    pub fn registers(&self) -> &Registers {
        &self.machine.cpu.registers
    }
    /// Read-only diagnostic view; mapped video ranges are excluded by the machine.
    pub fn ram_view(&self, range: core::ops::Range<usize>) -> Option<&[u8]> {
        self.machine.bus.ram_view(range)
    }
    pub fn flags(&self) -> u16 {
        self.machine.cpu.flags as u16
    }
    pub fn eflags(&self) -> u32 {
        self.machine.cpu.flags
    }
    pub fn control_registers(&self) -> &doslet_x86::ControlRegisters {
        &self.machine.cpu.control
    }
    pub fn a20_enabled(&self) -> bool {
        self.machine.bus.a20_enabled()
    }
    pub fn console(&self) -> &[u8] {
        &self.console
    }
    pub fn frame(&self) -> Option<IndexedFrame<'_>> {
        self.machine.bus.vga.frame()
    }
    pub fn start(&mut self) {
        if matches!(self.status, SessionStatus::Ready | SessionStatus::Paused) {
            self.status = SessionStatus::Running;
        }
    }
    pub fn pause(&mut self) {
        if self.status == SessionStatus::Running {
            self.status = SessionStatus::Paused;
        }
    }
    /// No wall-clock checks or allocations. Every batch is limited by execution steps.
    pub fn advance(&mut self, budget: u32) -> u32 {
        let mut completed = 0;
        while completed < budget && self.status == SessionStatus::Running {
            match self.execute_step(budget - completed) {
                Ok(consumed) => {
                    completed += consumed;
                    self.steps += u64::from(consumed);
                }
                Err(error) => {
                    self.fault = Some(error);
                    self.status = SessionStatus::Faulted;
                }
            }
        }
        completed
    }
    pub fn single_step(&mut self) {
        if matches!(self.status, SessionStatus::Ready | SessionStatus::Paused) {
            self.start();
            self.advance(1);
            self.pause();
        }
    }
    fn execute_step(&mut self, budget: u32) -> Result<u32, RuntimeError> {
        if !self.machine.cpu.protected_mode() && self.machine.cpu.registers.cs == bios::SHIM_SEGMENT
        {
            let ip = self.machine.cpu.registers.ip;
            if let Some(vector) = bios::service_at(&mut self.machine.bus, ip)? {
                self.service(vector)?;
                if self.status != SessionStatus::Running {
                    return Ok(1);
                }
            }
        }
        let (outcome, consumed) = self.machine.step_with_idle_budget(budget)?;
        if outcome == StepOutcome::Halted {
            self.status = SessionStatus::Halted;
        }
        Ok(consumed)
    }
    fn service(&mut self, vector: u8) -> Result<(), RuntimeError> {
        let ax = self.machine.cpu.registers.get(Gpr16::Ax);
        match (vector, ax >> 8) {
            // Mouse driver is not yet installed. A reset query returns AX=0
            // through a real vector instead of creating a null protected gate.
            (0x33, _) if ax == 0 => self.machine.cpu.registers.set(Gpr16::Ax, 0),
            (0x10, 0x4F) => self.vesa_service(ax as u8)?,
            (0x10, 0x0F) => {
                let (mode, columns) = match self.machine.bus.vga.mode() {
                    doslet_vga::VideoMode::Mode13 => (0x13, 40),
                    doslet_vga::VideoMode::Text3 => (3, 80),
                    doslet_vga::VideoMode::Vbe101 => (1, 80), // Low byte of the VBE mode.
                };
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Ax, (columns << 8) | mode);
                self.machine.cpu.registers.set_byte(7, 0); // Active page zero.
            }
            (0x21, 0x33) if ax as u8 <= 1 => {
                if ax as u8 == 0 {
                    self.machine
                        .cpu
                        .registers
                        .set_byte(2, u8::from(self.break_check));
                } else {
                    self.break_check = self.machine.cpu.registers.get_byte(2) != 0;
                }
            }
            (0x08, _) => {
                let ticks = u32::from(self.machine.bus.read_u16(0x46C)?)
                    | (u32::from(self.machine.bus.read_u16(0x46E)?) << 16);
                let next = ticks + 1;
                let next = if next >= 0x1800B0 {
                    let rollover = self.machine.bus.read_u8(0x470)?;
                    self.machine.bus.write_u8(0x470, rollover.wrapping_add(1))?;
                    0
                } else {
                    next
                };
                self.machine.bus.write_u16(0x46C, next as u16)?;
                self.machine.bus.write_u16(0x46E, (next >> 16) as u16)?;
            }
            (0x67, _) if ax == 0xDE00 => {
                self.machine.cpu.registers.set_byte(4, 0x84); // VCPI function unavailable, EMS AH status ABI.
            }
            (0x2F, _) if ax == 0x1687 => {
                self.machine.cpu.registers.set(Gpr16::Ax, 1); // No external DPMI host installed.
            }
            (0x2F, _) if ax == 0x4300 => {
                self.machine.cpu.registers.set_byte(0, 0); // No external XMS driver installed.
            }
            (0x15, 0x88) => {
                let kilobytes = (doslet_pc::memory::RAM_SIZE - 0x100000) / 1024;
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Ax, kilobytes.min(65535) as u16);
                self.set_carry(false)?;
            }
            (0x15, 0x24) if ax as u8 <= 3 => {
                match ax as u8 {
                    0 => self.machine.bus.set_a20(false),
                    1 => self.machine.bus.set_a20(true),
                    2 => self
                        .machine
                        .cpu
                        .registers
                        .set_byte(0, u8::from(self.machine.bus.a20_enabled())),
                    _ => self.machine.cpu.registers.set(Gpr16::Bx, 2), // Fast gate at port 92h.
                }
                self.machine.cpu.registers.set_byte(4, 0);
                self.set_carry(false)?;
            }
            (0x15, _) => {
                // BIOS reports unavailable functions with AH=86h and saved CF=1.
                self.machine.cpu.registers.set_byte(4, 0x86);
                self.set_carry(true)?;
            }
            (0x10, 0) if ax as u8 == 0x13 => self.machine.bus.vga.set_mode_13h(),
            (0x20, _) => self.status = SessionStatus::Exited(0),
            (0x21, 0x4C) => self.status = SessionStatus::Exited(ax as u8),
            (0x21, 0x30) => {
                self.machine.cpu.registers.set(Gpr16::Ax, 5);
                self.machine.cpu.registers.set(Gpr16::Bx, 0xFF00);
                self.machine.cpu.registers.set(Gpr16::Cx, 0);
            }
            (0x21, 0x51 | 0x62) => self.machine.cpu.registers.set(Gpr16::Bx, 0x1000),
            (0x21, 0x34) => {
                self.machine.cpu.registers.es = bios::SHIM_SEGMENT;
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Bx, bios::INDOS_OFFSET);
            }
            (0x21, 0x52) => {
                // DOS 5 list-of-lists subset: first MCB is at returned ES:BX-2.
                self.machine
                    .bus
                    .write_u16(0xF101E, self.memory.first_mcb())?;
                self.machine.cpu.registers.es = 0xF100;
                self.machine.cpu.registers.set(Gpr16::Bx, 0x20);
            }
            (0x21, 0x0E) if self.machine.cpu.registers.get_byte(2) == 2 => {
                // C: is the only mounted/default drive. AL is the drive count,
                // including reserved A:/B: letters, not an error code.
                self.machine.cpu.registers.set_byte(0, 3);
            }
            (0x21, 0x19) => self.machine.cpu.registers.set_byte(0, 2),
            (0x21, 0x38)
                if ax as u8 == 0 && self.machine.cpu.registers.get(Gpr16::Dx) != 0xFFFF =>
            {
                // DOS country 1 (US), 24-byte AH=38h ABI including a far upcase callback.
                let mut country = [0; 24];
                country[2] = b'$';
                country[7] = b',';
                country[9] = b'.';
                country[11] = b'/';
                country[13] = b':';
                country[16] = 2;
                country[18..20].copy_from_slice(&bios::UPPERCASE_OFFSET.to_le_bytes());
                country[20..22].copy_from_slice(&bios::SHIM_SEGMENT.to_le_bytes());
                country[22] = b',';
                let r = &self.machine.cpu.registers;
                let (ds, dx) = (r.ds, r.get(Gpr16::Dx));
                for (offset, byte) in country.iter().enumerate() {
                    self.machine
                        .bus
                        .write_u8(linear_address(ds, dx.wrapping_add(offset as u16)), *byte)?;
                }
                self.machine.cpu.registers.set(Gpr16::Ax, 1);
                self.machine.cpu.registers.set(Gpr16::Bx, 1);
                self.set_carry(false)?;
            }
            (0x21, 0x1A) => {
                self.dta = (
                    self.machine.cpu.registers.ds,
                    self.machine.cpu.registers.get(Gpr16::Dx),
                )
            }
            (0x21, 0x2F) => {
                self.machine.cpu.registers.es = self.dta.0;
                self.machine.cpu.registers.set(Gpr16::Bx, self.dta.1);
            }
            (0x21, 0x25) => {
                let address = u32::from(ax as u8) * 4;
                self.machine
                    .bus
                    .write_u16(address, self.machine.cpu.registers.get(Gpr16::Dx))?;
                self.machine
                    .bus
                    .write_u16(address + 2, self.machine.cpu.registers.ds)?;
            }
            (0x21, 0x29) => {
                let r = &self.machine.cpu.registers;
                let source = (r.ds, r.get(Gpr16::Si));
                let destination = (r.es, r.get(Gpr16::Di));
                if let Some((status, consumed)) =
                    doslet_dos::fcb::parse(&mut self.machine.bus, source, destination, ax as u8)?
                {
                    self.machine.cpu.registers.set_byte(0, status);
                    self.machine
                        .cpu
                        .registers
                        .set(Gpr16::Si, source.1.wrapping_add(consumed));
                } else {
                    return Err(RuntimeError::UnsupportedService { vector, ax });
                }
            }
            (0x21, 0x35) => {
                let address = u32::from(ax as u8) * 4;
                let offset = self.machine.bus.read_u16(address)?;
                let segment = self.machine.bus.read_u16(address + 2)?;
                self.machine.cpu.registers.es = segment;
                self.machine.cpu.registers.set(Gpr16::Bx, offset);
            }
            (0x21, 0x44) if ax as u8 == 0 => {
                let handle = self.machine.cpu.registers.get(Gpr16::Bx);
                if handle <= 2 || self.drive.is_open(handle) {
                    self.machine
                        .cpu
                        .registers
                        .set(Gpr16::Dx, if handle <= 2 { 0x80D3 } else { 2 });
                    self.set_carry(false)?;
                } else {
                    self.dos_error(6)?;
                }
            }
            (0x21, 0x47) => {
                let r = &self.machine.cpu.registers;
                if !matches!(r.get_byte(2), 0 | 3) {
                    self.dos_error(15)?;
                } else {
                    for (offset, byte) in self
                        .drive
                        .current_directory()
                        .iter()
                        .chain(core::iter::once(&0))
                        .enumerate()
                    {
                        self.machine.bus.write_u8(
                            linear_address(r.ds, r.get(Gpr16::Si).wrapping_add(offset as u16)),
                            *byte,
                        )?;
                    }
                    self.machine.cpu.registers.set(Gpr16::Ax, 0x0100);
                    self.set_carry(false)?;
                }
            }
            (0x21, 0x3B) => {
                let Some((name, length)) = self.read_path()? else {
                    return Ok(());
                };
                match self.drive.change_directory(&name[..length]) {
                    Ok(()) => self.set_carry(false)?,
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x4B) if ax as u8 == 3 => {
                let Some((name, length)) = self.read_path()? else {
                    return Ok(());
                };
                let r = &self.machine.cpu.registers;
                let parameter = linear_address(r.es, r.get(Gpr16::Bx));
                let load_segment = self.machine.bus.read_u16(parameter)?;
                let relocation_segment = self.machine.bus.read_u16(parameter + 2)?;
                match self.drive.file(&name[..length]) {
                    Ok(file) => {
                        doslet_dos::load_overlay(
                            &mut self.machine.bus,
                            file,
                            load_segment,
                            relocation_segment,
                            0xA000,
                        )?;
                        self.overlays += 1;
                        self.machine.cpu.registers.set(Gpr16::Ax, 0);
                        self.machine.cpu.registers.set(Gpr16::Dx, 0);
                        self.set_carry(false)?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x3D | 0x43) => {
                let Some((name, length)) = self.read_path()? else {
                    return Ok(());
                };
                let result = if ax >> 8 == 0x3D {
                    self.drive.open(&name[..length], ax as u8)
                } else if ax as u8 == 0 {
                    self.drive.attributes(&name[..length])
                } else {
                    Err(5)
                };
                match result {
                    Ok(value) => {
                        self.machine.cpu.registers.set(
                            if ax >> 8 == 0x43 {
                                Gpr16::Cx
                            } else {
                                Gpr16::Ax
                            },
                            value,
                        );
                        self.set_carry(false)?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x3E) => match self.drive.close(self.machine.cpu.registers.get(Gpr16::Bx)) {
                Ok(()) => self.set_carry(false)?,
                Err(code) => self.dos_error(code)?,
            },
            (0x21, 0x3F) => {
                let r = &self.machine.cpu.registers;
                let (ds, dx) = (r.ds, r.get(Gpr16::Dx));
                match self.drive.read_view(r.get(Gpr16::Bx), r.get(Gpr16::Cx)) {
                    Ok(bytes) => {
                        let length = bytes.len() as u16;
                        for offset in 0..bytes.len() {
                            self.machine.bus.write_u8(
                                linear_address(ds, dx.wrapping_add(offset as u16)),
                                bytes.byte(offset).unwrap(),
                            )?;
                        }
                        self.machine.cpu.registers.set(Gpr16::Ax, length);
                        self.set_carry(false)?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x40) => {
                let r = &self.machine.cpu.registers;
                let count = usize::from(r.get(Gpr16::Cx));
                if !matches!(r.get(Gpr16::Bx), 1 | 2) {
                    let (ds, dx) = (r.ds, r.get(Gpr16::Dx));
                    match self.drive.write_buffer(r.get(Gpr16::Bx), r.get(Gpr16::Cx)) {
                        Ok(bytes) => {
                            for (offset, byte) in bytes.iter_mut().enumerate() {
                                *byte = self
                                    .machine
                                    .bus
                                    .read_u8(linear_address(ds, dx.wrapping_add(offset as u16)))?;
                            }
                            self.machine.cpu.registers.set(Gpr16::Ax, count as u16);
                            self.set_carry(false)?;
                        }
                        Err(code) => self.dos_error(code)?,
                    }
                } else {
                    if count > CONSOLE_CAPACITY - self.console.len() {
                        return Err(RuntimeError::ConsoleFull);
                    }
                    let (ds, dx) = (r.ds, r.get(Gpr16::Dx));
                    for offset in 0..count {
                        self.console.push(
                            self.machine
                                .bus
                                .read_u8(linear_address(ds, dx.wrapping_add(offset as u16)))?,
                        );
                    }
                    self.machine.cpu.registers.set(Gpr16::Ax, count as u16);
                    self.set_carry(false)?;
                }
            }
            (0x21, 0x42) => {
                let r = &self.machine.cpu.registers;
                let displacement =
                    ((u32::from(r.get(Gpr16::Cx)) << 16) | u32::from(r.get(Gpr16::Dx))) as i32;
                match self.drive.seek(r.get(Gpr16::Bx), ax as u8, displacement) {
                    Ok(position) => {
                        self.machine.cpu.registers.set(Gpr16::Ax, position as u16);
                        self.machine
                            .cpu
                            .registers
                            .set(Gpr16::Dx, (position >> 16) as u16);
                        self.set_carry(false)?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x48..=0x4A) => {
                let r = &self.machine.cpu.registers;
                let result = match ax >> 8 {
                    0x48 => self.memory.allocate(r.get(Gpr16::Bx), 0x1000).map(Some),
                    0x49 => self.memory.free(r.es).map(|()| None),
                    _ => self.memory.resize(r.es, r.get(Gpr16::Bx)).map(|()| None),
                };
                self.memory.write_mcbs(&mut self.machine.bus)?;
                if ax >> 8 == 0x4A
                    && self.machine.cpu.registers.es == 0x1000
                    && let Ok(end) = self.memory.block_end(0x1000)
                {
                    self.machine.bus.write_u16(0x10002, end)?;
                }
                match result {
                    Ok(segment) => {
                        if let Some(segment) = segment {
                            self.machine.cpu.registers.set(Gpr16::Ax, segment);
                        }
                        if ax >> 8 == 0x4A {
                            self.machine
                                .cpu
                                .registers
                                .set(Gpr16::Ax, self.machine.cpu.registers.es);
                        }
                        self.set_carry(false)?;
                    }
                    Err(error) => {
                        self.machine.cpu.registers.set(Gpr16::Ax, error.code);
                        if ax >> 8 != 0x49 {
                            self.machine.cpu.registers.set(Gpr16::Bx, error.available);
                        }
                        self.set_carry(true)?;
                    }
                }
            }
            (0x21, 9) => {
                let ds = self.machine.cpu.registers.ds;
                let mut offset = self.machine.cpu.registers.get(Gpr16::Dx);
                loop {
                    let byte = self.machine.bus.read_u8(linear_address(ds, offset))?;
                    if byte == b'$' {
                        break;
                    }
                    if self.console.len() == CONSOLE_CAPACITY {
                        return Err(RuntimeError::ConsoleFull);
                    }
                    self.console.push(byte);
                    offset = offset.wrapping_add(1);
                }
                self.machine.cpu.registers.set_byte(0, b'$');
            }
            _ => return Err(RuntimeError::UnsupportedService { vector, ax }),
        }
        Ok(())
    }
    fn set_carry(&mut self, carry: bool) -> Result<(), BusError> {
        let r = &self.machine.cpu.registers;
        let address = linear_address(r.ss, r.get(Gpr16::Sp).wrapping_add(4));
        let flags = self.machine.bus.read_u16(address)?;
        self.machine
            .bus
            .write_u16(address, (flags & !1) | u16::from(carry))
    }
    fn dos_error(&mut self, code: u16) -> Result<(), BusError> {
        self.machine.cpu.registers.set(Gpr16::Ax, code);
        self.set_carry(true)
    }
    fn read_path(&mut self) -> Result<Option<([u8; 128], usize)>, RuntimeError> {
        let r = &self.machine.cpu.registers;
        let (ds, dx) = (r.ds, r.get(Gpr16::Dx));
        let mut name = [0; 128];
        for (offset, byte) in name.iter_mut().enumerate() {
            *byte = self
                .machine
                .bus
                .read_u8(linear_address(ds, dx.wrapping_add(offset as u16)))?;
            if *byte == 0 {
                return Ok(Some((name, offset)));
            }
        }
        self.dos_error(3)?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_demo_runs_through_cpu_bios_and_mmio() {
        let mut session = Session::demo();
        session.start();
        assert_eq!(session.advance(10), 10);
        assert_eq!(session.status(), SessionStatus::Running);
        session.advance(100_000);
        assert_eq!(session.status(), SessionStatus::Exited(0));
        let frame = session.frame().unwrap();
        for band in 0..16 {
            assert!(
                frame.pixels[band * 4000..(band + 1) * 4000]
                    .iter()
                    .all(|value| usize::from(*value) == band)
            );
        }
    }
    #[test]
    fn pause_and_step_are_bounded_and_instances_are_independent() {
        let mut first = Session::demo();
        let second = Session::demo();
        first.start();
        first.advance(3);
        first.pause();
        assert_eq!(first.advance(100), 0);
        first.single_step();
        assert_eq!(first.steps(), 4);
        assert_eq!(first.status(), SessionStatus::Paused);
        assert_eq!(second.steps(), 0);
        assert!(second.frame().is_none());
    }
    #[test]
    fn unknown_services_and_opcodes_report_explicit_faults() {
        let mut session = Session::new(&[0xB4, 0xFF, 0xCD, 0x21]).unwrap();
        session.start();
        session.advance(100);
        assert!(matches!(
            session.fault(),
            Some(RuntimeError::UnsupportedService { vector: 0x21, .. })
        ));
        let mut session = Session::new(&[0x0F, 0xFF]).unwrap();
        session.start();
        session.advance(100);
        assert!(matches!(
            session.fault(),
            Some(RuntimeError::Cpu(CpuError::UnsupportedExtendedOpcode {
                ip: 0x100,
                ..
            }))
        ));
    }
    #[test]
    fn unsupported_com_sizes_are_rejected_before_execution() {
        assert!(matches!(
            Session::new(&[]),
            Err(RuntimeError::Load(ComError::Empty))
        ));
        let bytes = vec![0; doslet_dos::loader::MAX_COM_BYTES + 1];
        assert!(matches!(
            Session::new(&bytes),
            Err(RuntimeError::Load(ComError::TooLarge))
        ));
    }
    #[test]
    fn a_guest_replacement_vector_executes_instead_of_the_bios_service() {
        let mut session = Session::new(&[0xCD, 0x10, 0xB8, 0, 0x4C, 0xCD, 0x21]).unwrap();
        session.machine.bus.write_u16(0x40, 0x300).unwrap();
        session.machine.bus.write_u16(0x42, 0x1000).unwrap();
        session.machine.bus.write_u8(0x10300, 0xCF).unwrap();
        session.start();
        session.advance(100);
        assert_eq!(session.status(), SessionStatus::Exited(0));
        assert!(session.frame().is_none());
    }
}
