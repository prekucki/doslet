use crate::{bios, demo::COLOR_BARS_COM};
use alloc::boxed::Box;
#[cfg(test)]
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use doslet_bus::{BusError, MemoryBus};
use doslet_config::HardwareConfig;
use doslet_dos::files::{ReadOnlyDrive, VisibleFile};
use doslet_dos::memory::MemoryArena;
use doslet_dos::{ComError, MzError, ProcessMemory, load_com, load_mz, load_mz_stub};
use doslet_pc::PcMachine;
use doslet_storage::{CachedSource, HostCompletion, HostRequest, StorageError};
use doslet_vfs::{HandleTable, MountId, NodeId, OpenFile};
use doslet_vga::IndexedFrame;
use doslet_x86::{CpuError, Gpr16, Registers, StepOutcome, linear_address};
mod analyzer;
pub use analyzer::{AnalyzerStats, CallerEffect, CandidateStats};
mod cdrom;
mod clock;
mod cooperative;
pub use cooperative::{EventIdleSite, MenuIdleSite, MenuIdleStats};
pub mod debug;
pub mod dpmi;
mod idle;
mod observation;
mod synchronization;
pub mod wait_budget;
use cdrom::CdromImage;
pub use idle::IdleLoopStats;
pub use observation::IdleObservation;
pub use synchronization::SynchronizationStats;
mod keyboard;
mod memory_usage;
mod mouse;
pub use memory_usage::MemoryUsage;
pub mod patches;
#[cfg(feature = "save-state")]
mod saved;
#[cfg(feature = "save-state")]
pub use saved::{StateResources, WorkingState};
mod storage;
mod vesa;
mod video;
pub use video::VideoModeLabel;

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
    InvalidLaunch,
    InvalidWorkingDirectory(u16),
    InvalidCdromImage,
    Storage(StorageError),
    Input(doslet_input::InputError),
    PatchTimeout(u8),
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
            Self::InvalidLaunch => f.write_str("launch setup requires a ready session and a printable ASCII DOS command tail of at most 126 bytes"),
            Self::InvalidWorkingDirectory(code) => write!(f, "invalid DOS working directory (DOS error {code})"),
            Self::Input(error)=>write!(f,"input error: {error:?}"),
            Self::PatchTimeout(index) => write!(f, "memory patch #{index}: expected RAM bytes not found before the guest step deadline"),
            Self::Storage(error) => write!(f,"storage error: {error:?}"),
            Self::InvalidCdromImage => f.write_str("MSCDEX image must be a sector-aligned ISO 9660 image with a valid primary volume descriptor"),
        }
    }
}
impl core::error::Error for RuntimeError {}

pub struct Session {
    machine: Box<PcMachine>,
    status: SessionStatus,
    fault: Option<RuntimeError>,
    steps: u64,
    /// Transient presentation telemetry; not part of portable guest state.
    disk_read_bytes: u64,
    patches: Vec<patches::MemoryPatch>,
    patches_applied: usize,
    console: Vec<u8>,
    console_truncated: bool,
    extended_key: Option<u8>,
    mouse: doslet_dos::mouse::Mouse,
    mouse_fraction: [f64; 2],
    mouse_last_read: Option<u64>,
    mouse_last_relative_read: Option<u64>,
    dos_time_origin_nanos: u64,
    dos_time_origin_centis: u32,
    memory: MemoryArena,
    drive: ReadOnlyDrive,
    dta: (u16, u16),
    overlays: u32,
    break_check: bool,
    cdrom: Option<CdromImage>,
    handles: HandleTable,
    staging: Box<[u8]>,
    continuation: Option<storage::Continuation>,
    storage_wait: bool,
    timeslice_yielded: bool,
    epoch: u64,
    active_service: u8,
    persistent: Option<storage::Persistent>,
    base_identity: [u8; 32],
    keyboard_prefix: bool,
    input: doslet_input::InputRouter,
    joystick_axes: [i16; 2],
    joystick_buttons: [bool; 2],
    idle: idle::IdleLoops,
    dpmi: Option<Box<dpmi::Host>>,
    service_flags: Option<u16>,
    menu_idle: cooperative::MenuIdle,
    observer: observation::Observer,
    synchronization: synchronization::Synchronization,
    analyzer: analyzer::Analyzer,
}

impl Session {
    /// Configure the PSP and DOS working directory before guest execution.
    pub fn configure_launch(
        &mut self,
        command_tail: &[u8],
        working_directory: &[u8],
    ) -> Result<(), RuntimeError> {
        if self.status != SessionStatus::Ready
            || command_tail.len() > 126
            || !command_tail.is_ascii()
            || command_tail
                .iter()
                .any(|byte| *byte < 0x20 || *byte == 0x7F)
        {
            return Err(RuntimeError::InvalidLaunch);
        }
        self.drive
            .change_directory(working_directory)
            .map_err(RuntimeError::InvalidWorkingDirectory)?;
        let base = u32::from(doslet_dos::loader::PSP_SEGMENT) * 16;
        self.machine
            .bus
            .write_u8(base + 0x80, command_tail.len() as u8)?;
        for offset in 0..127 {
            self.machine.bus.write_u8(
                base + 0x81 + offset as u32,
                command_tail
                    .get(offset)
                    .copied()
                    .unwrap_or(if offset == command_tail.len() {
                        0x0D
                    } else {
                        0
                    }),
            )?;
        }
        Ok(())
    }
    pub fn new(program: &[u8]) -> Result<Self, RuntimeError> {
        Self::new_configured(program, HardwareConfig::default())
    }
    pub fn new_configured(program: &[u8], config: HardwareConfig) -> Result<Self, RuntimeError> {
        let mut machine = PcMachine::from_config(config)?;
        machine.cpu.registers = load_com(&mut machine.bus, program)?;
        Self::from_machine(machine)
    }
    pub fn from_mz(program: &[u8]) -> Result<Self, RuntimeError> {
        Self::mz_session(program, false, HardwareConfig::default())
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
        Self::from_mz_stub_named_configured(program, name, HardwareConfig::default())
    }
    pub fn from_mz_stub_named_configured(
        program: &[u8],
        name: &[u8],
        config: HardwareConfig,
    ) -> Result<Self, RuntimeError> {
        if name.is_empty() || name.len() > 127 || !name.is_ascii() || name.contains(&0) {
            return Err(RuntimeError::InvalidProgramName);
        }
        let mut session = Self::mz_session(program, true, config)?;
        session.memory = session.memory.with_environment(0x0F00, 0x1000);
        session.memory.write_mcbs(&mut session.machine.bus)?;
        let bus = &mut session.machine.bus;
        // DOS games commonly consult BLASTER before probing the DSP. Describe
        // the emulated card, independently of whether host audio is enabled.
        let mut environment = Vec::from(&b"PATH=C:\\\0"[..]);
        if config.sb16 {
            environment.extend_from_slice(
                alloc::format!(
                    "BLASTER=A220 I{} D{} H{} T6{}\0",
                    config.sb_irq,
                    config.sb_dma8,
                    config.sb_dma16,
                    if config.mpu401 { " P330" } else { "" },
                )
                .as_bytes(),
            );
        }
        environment.extend_from_slice(b"\0\x01\0");
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
    pub fn from_mz_configured(
        program: &[u8],
        config: HardwareConfig,
    ) -> Result<Self, RuntimeError> {
        Self::mz_session(program, false, config)
    }
    pub fn demo_configured(config: HardwareConfig) -> Result<Self, RuntimeError> {
        Self::new_configured(COLOR_BARS_COM, config)
    }
    fn mz_session(
        program: &[u8],
        dos_stub: bool,
        config: HardwareConfig,
    ) -> Result<Self, RuntimeError> {
        let mut machine = PcMachine::from_config(config)?;
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
    fn from_machine(machine: PcMachine) -> Result<Self, RuntimeError> {
        let mut machine = Box::new(machine);
        bios::install(&mut machine.bus)?;
        // Virtual mouse IRQ wrapper preserves full general and segment registers.
        machine.bus.write_u16(0x74 * 4, 0x400)?;
        machine.bus.write_u16(0x74 * 4 + 2, bios::SHIM_SEGMENT)?;
        for (offset, byte) in [
            0x66, 0x60, 0x1E, 0x06, 0x0F, 0xA0, 0x0F, 0xA8, 0x9A, 0, 0, 0, 0, 0x0F, 0xA9, 0x0F,
            0xA1, 0x07, 0x1F, 0x66, 0x61, 0xCF,
        ]
        .into_iter()
        .enumerate()
        {
            machine.bus.write_u8(0xF0400 + offset as u32, byte)?;
        }
        let process_end = machine.bus.read_u16(0x10002)?;
        let memory = MemoryArena::new(0x1000, process_end, 0xA000);
        memory.write_mcbs(&mut machine.bus)?;
        let epoch = storage::new_epoch()?;
        Ok(Self {
            machine,
            status: SessionStatus::Ready,
            fault: None,
            steps: 0,
            disk_read_bytes: 0,
            patches: Vec::new(),
            patches_applied: 0,
            console: Vec::with_capacity(CONSOLE_CAPACITY),
            console_truncated: false,
            extended_key: None,
            mouse: doslet_dos::mouse::Mouse::default(),
            mouse_fraction: [0.0; 2],
            mouse_last_read: None,
            mouse_last_relative_read: None,
            dos_time_origin_nanos: 0,
            dos_time_origin_centis: 0,
            memory,
            drive: ReadOnlyDrive::new(),
            dta: (0x1000, 0x80),
            overlays: 0,
            break_check: false,
            cdrom: None,
            handles: HandleTable::default(),
            staging: alloc::vec![0;65536].into_boxed_slice(),
            continuation: None,
            storage_wait: false,
            timeslice_yielded: false,
            epoch,
            active_service: 0,
            persistent: None,
            base_identity: [0; 32],
            keyboard_prefix: false,
            input: doslet_input::InputRouter::new(epoch),
            joystick_axes: [0; 2],
            joystick_buttons: [false; 2],
            idle: idle::IdleLoops::default(),
            dpmi: None,
            service_flags: None,
            menu_idle: cooperative::MenuIdle::default(),
            observer: observation::Observer::default(),
            synchronization: synchronization::Synchronization::default(),
            analyzer: analyzer::Analyzer::default(),
        })
    }
    /// Transfer a preloaded input drive into the session during setup.
    pub fn with_drive(mut self, drive: ReadOnlyDrive) -> Self {
        self.base_identity = drive.base_identity();
        self.drive = drive;
        self
    }
    /// Inspect the effective C: file tree during host-side packaging.
    pub fn visible_drive_files(&self) -> impl Iterator<Item = VisibleFile<'_>> {
        self.drive.visible_files()
    }
    pub fn drive_has_empty_directories(&self) -> bool {
        self.drive.has_empty_directories()
    }
    pub fn drive_base_identity(&self) -> [u8; 32] {
        self.base_identity
    }
    /// Attach an immutable ISO 9660 image as a DOS CD-ROM drive before execution.
    /// `letter` is the drive letter (D through Z); the MSCDEX unit is zero-based.
    pub fn with_cdrom_image(mut self, letter: u8, image: Vec<u8>) -> Result<Self, RuntimeError> {
        if self.status != SessionStatus::Ready || self.cdrom.is_some() {
            return Err(RuntimeError::InvalidLaunch);
        }
        self.cdrom = Some(CdromImage::new(letter, image)?);
        Ok(self)
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
    pub fn drain_pcm_timed(&mut self, output: &mut [doslet_pcm::Frame]) -> usize {
        self.machine.drain_pcm_timed(output)
    }
    pub fn capture_audio_sources(&mut self, enabled: bool) {
        self.machine.capture_audio_sources(enabled);
    }
    pub fn drain_audio_source(
        &mut self,
        fm: bool,
        output: &mut [doslet_pcm::SourceFrame],
    ) -> usize {
        self.machine.drain_audio_source(fm, output)
    }
    pub fn audio_status(&self) -> doslet_pc::AudioStatus {
        self.machine.audio_status()
    }
    pub fn audio_use(&self) -> doslet_pc::AudioUse {
        self.machine.audio_use()
    }
    /// Whether changing audio would invalidate a possibly cached DOS environment.
    pub fn audio_environment_requires_restart(
        old: doslet_config::AudioHardware,
        new: doslet_config::AudioHardware,
    ) -> bool {
        old.sb16 != new.sb16
            || (old.sb16
                && (old.irq, old.dma8, old.dma16, old.mpu401)
                    != (new.irq, new.dma8, new.dma16, new.mpu401))
    }
    pub fn configure_unused_audio(
        &mut self,
        audio: doslet_config::AudioHardware,
    ) -> Result<(), doslet_pc::AudioConfigError> {
        let old = self.hardware_config().audio();
        // The guest may have read or copied BLASTER without touching an audio
        // port. Rebuilding its environment cannot invalidate that cached value.
        if Self::audio_environment_requires_restart(old, audio) {
            return Err(doslet_pc::AudioConfigError::RestartRequired(
                "The DOS BLASTER environment depends on these audio settings",
            ));
        }
        self.machine.configure_unused_audio(audio)?;
        let enabled = self.idle.enabled;
        self.set_idle_loops(false);
        self.set_idle_loops(enabled);
        Ok(())
    }
    pub fn flush_writes<E>(
        &mut self,
        write: impl FnMut(&[u8], doslet_dos::files::ReadView<'_>) -> Result<(), E>,
    ) -> Result<usize, E> {
        self.drive.flush_writes(write)
    }
    pub fn has_closed_writes(&self) -> bool {
        self.drive.has_closed_writes()
    }
    pub fn emulated_nanos(&self) -> u64 {
        self.machine.time().as_nanos()
    }
    pub fn cpu_halted(&self) -> bool {
        self.machine.cpu.is_halted()
    }
    /// Reference/debug execution can disable recognition and wait batching.
    pub fn set_vga_wait_acceleration(&mut self, enabled: bool) {
        self.machine.set_vga_wait_acceleration(enabled);
    }
    pub fn set_decode_cache(&mut self, enabled: bool) {
        self.machine.cpu.set_decode_cache(enabled);
    }
    pub fn set_dynamic_core(&mut self, enabled: bool) {
        self.machine.cpu.set_dynamic_core(enabled);
    }
    pub fn dynamic_core_enabled(&self) -> bool {
        self.machine.cpu.dynamic_core_enabled()
    }
    pub fn dynamic_stats(&self) -> doslet_x86::DynamicStats {
        self.machine.cpu.dynamic_stats()
    }
    /// Opt-in native-exit diagnostics. Configure before compiling native code;
    /// allocation is confined to this setup call, not guest execution.
    pub fn set_dynamic_exit_profiling(&mut self, enabled: bool) -> Result<(), &'static str> {
        self.machine.cpu.set_dynamic_exit_profiling(enabled)
    }
    pub fn dynamic_exit_profile(&self) -> &[doslet_x86::DynamicExitRecord] {
        self.machine.cpu.dynamic_exit_profile()
    }
    pub fn dynamic_exit_profile_dropped(&self) -> u64 {
        self.machine.cpu.dynamic_exit_profile_dropped()
    }
    pub fn dynamic_error(&self) -> Option<&str> {
        self.machine.cpu.dynamic_error()
    }
    /// Native compiler maintenance. May allocate; call outside advance() and
    /// audio callbacks. Unsupported targets/builds return zero.
    pub fn prepare_dynamic(&mut self, limit: usize) -> usize {
        self.machine.cpu.prepare_dynamic(limit)
    }
    #[cfg(all(
        feature = "dynamic-core",
        not(target_arch = "wasm32"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn take_dynamic_compilation(
        &mut self,
        limit: usize,
    ) -> Option<doslet_x86::DynamicCompilation> {
        self.machine.cpu.take_dynamic_compilation(limit)
    }
    #[cfg(all(
        feature = "dynamic-core",
        not(target_arch = "wasm32"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn finish_dynamic_compilation(&mut self, job: doslet_x86::DynamicCompilation) -> usize {
        self.machine.cpu.finish_dynamic_compilation(job)
    }
    pub fn load_dynamic_profile(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        self.machine.cpu.load_dynamic_profile(bytes)
    }
    pub fn write_dynamic_profile(&self, output: &mut [u8]) -> usize {
        self.machine.cpu.write_dynamic_profile(output)
    }
    /// Experimental caller-selected memory polling sites; no guest code patches.
    pub fn set_memory_wait_sites(&mut self, operand: u32, starts: [u32; 3]) {
        if self.analyzer.controller {
            self.set_adaptive_cpu(false);
        }
        self.machine
            .set_memory_wait_sites(Some(doslet_pc::MemoryWaitSites { operand, starts }));
    }
    pub fn memory_wait_steps(&self) -> u64 {
        self.machine.memory_wait_steps()
    }
    #[cfg(feature = "profiling")]
    pub fn decode_cache_stats(&self) -> doslet_x86::DecodeCacheStats {
        self.machine.cpu.decode_cache_stats()
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
    /// Successful guest data reads, including cached files and raw CD sectors.
    pub fn disk_read_bytes(&self) -> u64 {
        self.disk_read_bytes
    }
    pub fn midi_file_request(&self) -> Option<(&[u8], u8, u16)> {
        self.drive.midi_request()
    }
    pub fn video_mode(&self) -> doslet_vga::VideoMode {
        self.machine.bus.vga.mode()
    }
    /// Register-derived guest mode, independent of host scaling and filters.
    /// Returns small values; formatting is left to the frontend boundary.
    pub fn video_mode_label(&self) -> VideoModeLabel {
        let dimensions = self.frame().map(|frame| (frame.width, frame.height));
        VideoModeLabel::new(self.video_mode(), dimensions)
    }
    pub fn video_memory_configuration(&self) -> (u8, u8, u8, u16, u8, u8) {
        self.machine.bus.vga.memory_configuration()
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
    pub fn console_truncated(&self) -> bool {
        self.console_truncated
    }
    fn console_output(&mut self, byte: u8) -> Result<(), RuntimeError> {
        if self.console.len() == CONSOLE_CAPACITY {
            self.console.copy_within(CONSOLE_CAPACITY / 2.., 0);
            self.console.truncate(CONSOLE_CAPACITY / 2);
            self.console_truncated = true;
        }
        self.console.push(byte);
        if self.video_mode() == doslet_vga::VideoMode::Text3 {
            self.teletype(byte)?;
        }
        Ok(())
    }
    pub fn frame(&self) -> Option<IndexedFrame<'_>> {
        self.machine.bus.video_frame()
    }
    pub fn presentation_frame(&self) -> doslet_vga::scanout::PresentationFrame<'_> {
        use doslet_vga::scanout::{PresentationFrame, TextFrame};
        if let Some(mut frame) = self.frame() {
            frame.revision = frame.revision.wrapping_add(self.mouse.revision);
            if self.mouse.visible >= 0 {
                let dimensions = self.pointer_dimensions();
                return PresentationFrame::Cursor {
                    x: i32::from(self.mouse.x) * frame.width as i32 / i32::from(dimensions.0)
                        - i32::from(self.mouse.hotspot.0),
                    y: i32::from(self.mouse.y) * frame.height as i32 / i32::from(dimensions.1)
                        - i32::from(self.mouse.hotspot.1),
                    masks: &self.mouse.masks,
                    frame,
                };
            }
            return PresentationFrame::Indexed(frame);
        }
        let bda = self.ram_view(0x400..0x488).expect("BIOS data RAM");
        let page = usize::from(bda[0x62].min(7));
        let position = u16::from_le_bytes([bda[0x50 + page * 2], bda[0x51 + page * 2]]);
        let blink = (self.emulated_nanos() / 250_000_000) & 1 == 0;
        PresentationFrame::Text(TextFrame {
            cells: self
                .ram_view(0xB8000 + page * 0x1000..0xB8000 + page * 0x1000 + 4000)
                .expect("text RAM"),
            palette: self.machine.bus.vga.palette(),
            revision: self.machine.bus.text_revision().wrapping_mul(2) + u64::from(blink),
            cursor: (position >> 8) * 80 + (position & 255),
            cursor_shape: u16::from_le_bytes([bda[0x60], bda[0x61]]),
            blink,
        })
    }
    /// Read-only physical video memory for differential diagnostics.
    pub fn vram_view(&self) -> &[u8] {
        self.machine.bus.vram_view()
    }
    pub fn start(&mut self) {
        if matches!(self.status, SessionStatus::Ready | SessionStatus::Paused) {
            self.status = SessionStatus::Running;
        }
    }
    pub fn pause(&mut self) {
        if self.status == SessionStatus::Running {
            self.status = SessionStatus::Paused;
            self.input.release_all();
            let _ = self.release_keys();
        }
    }
    /// No wall-clock checks or allocations. A proved whole computation may
    /// finish past this soft step budget, returning its actual completed work.
    pub fn advance(&mut self, budget: u32) -> u32 {
        self.advance_with(budget, false)
    }
    /// With `stop_at_entries`, return once execution arrives at a registered
    /// prepared entry after completing work, for a foreign engine to dispatch.
    pub(crate) fn advance_with(&mut self, budget: u32, stop_at_entries: bool) -> u32 {
        if self.status == SessionStatus::Running
            && budget != 0
            && self.patches_applied < self.patches.len()
        {
            self.check_patches();
            // Poll pending plans at most 4096 guest steps apart, without adding
            // signature checks to the per-instruction execution path.
            let remaining = self
                .patches
                .get(self.patches_applied)
                .map_or(u64::MAX, |p| p.timeout_steps.saturating_sub(self.steps));
            let completed = self.advance_inner(
                budget
                    .min(4096)
                    .min(remaining.min(u64::from(u32::MAX)) as u32),
                stop_at_entries,
            );
            if self.status == SessionStatus::Running {
                self.check_patches();
            }
            completed
        } else {
            self.advance_inner(budget, stop_at_entries)
        }
    }
    fn advance_inner(&mut self, budget: u32, stop_at_entries: bool) -> u32 {
        // Leave enough room in the u32 result for one complete computation
        // beyond the requested budget. This is a representation bound, not an
        // instruction checkpoint inside generated code.
        #[cfg(feature = "wasm-procedures")]
        let budget = budget.min(u32::MAX - doslet_pc::wasm_procedures::MAX_ATOMIC_PROCEDURE_WORK);
        self.timeslice_yielded = false;
        self.apply_input();
        let mut completed = 0;
        while completed < budget
            && self.status == SessionStatus::Running
            && !self.storage_wait
            && !self.timeslice_yielded
            && !(stop_at_entries
                && completed != 0
                && self.machine.cpu.prepared_entry_index().is_some())
        {
            let now = self.steps + u64::from(completed);
            let idle = self
                .analyzer_advance(now, budget - completed)
                .and_then(|result| {
                    if result.is_some() {
                        Ok(result)
                    } else {
                        self.synchronization_advance(now, budget - completed)
                    }
                })
                .and_then(|trace| {
                    if trace.is_some() {
                        Ok(trace)
                    } else {
                        self.menu_idle_advance(budget - completed)
                    }
                })
                .and_then(|result| {
                    if result.is_some() {
                        Ok(result)
                    } else {
                        self.idle_advance(now, budget - completed)
                    }
                });
            let step = match idle {
                Ok(Some(consumed)) => Ok(consumed),
                Ok(None) => self.execute_step(budget - completed),
                Err(error) => Err(error),
            };
            match step {
                Ok(consumed) => {
                    completed += consumed;
                }
                // A DPMI client's exception handler receives its faults.
                Err(error) if self.dpmi_exception(&error) => completed += 1,
                Err(error) => {
                    self.fault = Some(error);
                    self.status = SessionStatus::Faulted;
                }
            }
        }
        self.steps += u64::from(completed);
        self.observe_idle();
        self.analyzer_control();
        completed
    }
    pub fn set_execution_tiers(
        &mut self,
        labels: Option<alloc::vec::Vec<doslet_x86::ExecutionLabel>>,
        rates: [Option<u16>; 3],
    ) -> Result<(), &'static str> {
        let idle = self.idle.enabled;
        self.set_idle_loops(false);
        let result = self.machine.set_execution_tiers(labels, rates);
        self.set_idle_loops(idle);
        result
    }
    pub fn replace_execution_labels(
        &mut self,
        labels: alloc::vec::Vec<doslet_x86::ExecutionLabel>,
    ) -> Result<(), &'static str> {
        let idle = self.idle.enabled;
        self.set_idle_loops(false);
        let result = self.machine.cpu.replace_execution_labels(labels);
        self.set_idle_loops(idle);
        result
    }
    /// Actual instruction/microstep counts, excluding proven skipped passes.
    pub fn executed_tier_counts(&self) -> [u64; 4] {
        self.machine.cpu.executed_tier_counts()
    }
    pub fn execution_tier_stats(&self) -> ([u64; 4], [u64; 4]) {
        self.machine.execution_tier_stats()
    }

    /// Install an offline module once; native interpretation and WASM borrow
    /// the same guest RAM and generation table thereafter. Runtime bindings are
    /// intentionally excluded from portable states and must be reinstalled.
    #[cfg(all(feature = "wasm-procedures", not(target_arch = "wasm32")))]
    pub fn install_wasm_procedures(
        &mut self,
        wasm: &[u8],
        bindings: alloc::vec::Vec<doslet_pc::wasm_procedures::Binding>,
    ) -> Result<(), alloc::string::String> {
        self.machine.install_wasm_procedures(wasm, bindings)
    }

    /// Browser exports borrow this worker's one linear memory synchronously.
    #[cfg(all(feature = "wasm-procedures", target_arch = "wasm32"))]
    pub fn install_browser_wasm_procedures(
        &mut self,
        callback: doslet_pc::wasm_procedures::BrowserCall,
        bindings: alloc::vec::Vec<doslet_pc::wasm_procedures::Binding>,
    ) -> Result<(), alloc::string::String> {
        self.machine
            .install_browser_wasm_procedures(callback, bindings)
    }

    /// As `install_wasm_procedures`, reusing compatible native code from the host.
    #[cfg(all(feature = "wasm-procedures", not(target_arch = "wasm32")))]
    pub fn install_wasm_procedures_cached(
        &mut self,
        wasm: &[u8],
        bindings: alloc::vec::Vec<doslet_pc::wasm_procedures::Binding>,
        cache: &mut dyn doslet_pc::wasm_procedures::CompiledCache,
    ) -> Result<(), alloc::string::String> {
        self.machine
            .install_wasm_procedures_cached(wasm, bindings, cache)
    }

    #[cfg(feature = "wasm-procedures")]
    pub fn install_interpreted_procedure_boundaries(
        &mut self,
        bindings: alloc::vec::Vec<doslet_pc::wasm_procedures::Binding>,
    ) -> Result<(), alloc::string::String> {
        self.machine
            .install_interpreted_procedure_boundaries(bindings)
    }

    /// Select an opt-in diagnostic interpreter variant without replacing the
    /// installed procedure plan or shared memory owner.
    #[cfg(all(feature = "wasm-procedure-diagnostics", not(target_arch = "wasm32")))]
    pub fn set_wasm_procedure_diagnostic_mode(
        &mut self,
        mode: doslet_pc::wasm_procedures::DiagnosticMode,
    ) -> Result<(), &'static str> {
        self.machine.set_wasm_procedure_diagnostic_mode(mode)
    }

    #[cfg(feature = "wasm-procedures")]
    pub fn wasm_procedure_stats(&self) -> Option<doslet_pc::wasm_procedures::Stats> {
        self.machine.wasm_procedure_stats()
    }

    #[cfg(feature = "wasm-procedures")]
    pub fn wasm_procedure_entry_count(&self) -> usize {
        self.machine.wasm_procedure_entry_count()
    }

    #[cfg(feature = "wasm-procedures")]
    pub fn wasm_procedure_copy_tier_entries(&self) -> impl Iterator<Item = u32> + '_ {
        self.machine.wasm_procedure_copy_tier_entries()
    }

    /// Stop or resume calls into installed procedures.
    #[cfg(feature = "wasm-procedures")]
    pub fn set_wasm_procedures_suspended(&mut self, suspended: bool) -> Result<(), &'static str> {
        self.machine.set_wasm_procedures_suspended(suspended)
    }

    /// Procedures are installed and not suspended.
    #[cfg(feature = "wasm-procedures")]
    pub fn wasm_procedures_active(&self) -> bool {
        self.machine.wasm_procedures_active()
    }

    /// Procedures can be installed at this batch boundary.
    #[cfg(feature = "wasm-procedures")]
    pub fn wasm_procedures_installable(&self) -> bool {
        self.machine.wasm_procedures_installable()
    }

    #[cfg(feature = "wasm-procedures")]
    pub fn set_wasm_procedure_tier_accounting(
        &mut self,
        enabled: bool,
    ) -> Result<(), &'static str> {
        self.machine.set_wasm_procedure_tier_accounting(enabled)
    }

    #[cfg(feature = "wasm-procedures")]
    pub fn wasm_procedure_last_error(&self) -> Option<&str> {
        self.machine.wasm_procedure_last_error()
    }

    /// Live CPU throughput change; devices and accumulated guest time continue.
    pub fn set_cpu_mhz(&mut self, mhz: u16) -> Result<(), RuntimeError> {
        self.machine.set_cpu_mhz(mhz)?;
        self.analyzer.ceiling = mhz;
        let enabled = self.idle.enabled;
        self.set_idle_loops(false);
        self.set_idle_loops(enabled);
        Ok(())
    }
    /// Whether the most recent `advance` ended on INT 2Fh/1680h. Frontends
    /// should return to their event loop before starting another batch.
    pub fn timeslice_yielded(&self) -> bool {
        self.timeslice_yielded
    }
    pub fn single_step(&mut self) {
        if matches!(self.status, SessionStatus::Ready | SessionStatus::Paused) {
            self.start();
            self.advance(1);
            self.pause();
        }
    }
    fn execute_step(&mut self, budget: u32) -> Result<u32, RuntimeError> {
        self.execute_step_mode::<false>(budget, None)
    }
    fn execute_step_observed(
        &mut self,
        observation: &mut doslet_pc::StepObservation,
    ) -> Result<u32, RuntimeError> {
        self.execute_step_mode::<true>(1, Some(observation))
    }
    fn execute_step_mode<const OBSERVE: bool>(
        &mut self,
        budget: u32,
        mut observation: Option<&mut doslet_pc::StepObservation>,
    ) -> Result<u32, RuntimeError> {
        let before_service_writes = if OBSERVE {
            self.machine.ram_writes()
        } else {
            0
        };
        let before_service_devices = if OBSERVE {
            self.machine.device_accesses()
        } else {
            0
        };
        self.apply_input();
        if self.continuation.is_none() && self.dpmi.is_some() {
            // Observed execution needs the actual real-mode instruction and
            // its bus trace. Retain the configured mode for subsequent calls.
            let mode = self.dpmi.as_ref().unwrap().mode;
            if OBSERVE {
                self.dpmi.as_mut().unwrap().mode = dpmi::Mode::Reflect;
            }
            let result = self.dpmi_dispatch();
            if OBSERVE {
                self.dpmi.as_mut().unwrap().mode = mode;
            }
            result?;
            if self.status != SessionStatus::Running {
                return Ok(1);
            }
            if self.dpmi.as_ref().is_some_and(|h| h.direct_return_pending) {
                self.machine.advance_service_step()?;
                return Ok(1);
            }
        }
        if self.continuation.is_some() {
            if !self.resume_storage()? {
                return Ok(1);
            }
        } else {
            self.dispatch_mouse()?;
            if !self.machine.cpu.protected_mode()
                && self.machine.cpu.registers.cs == bios::SHIM_SEGMENT
            {
                let ip = self.machine.cpu.registers.ip;
                if let Some(selector) = bios::service_at(&mut self.machine.bus, ip)? {
                    let vector = selector.vector();
                    self.active_service = vector;
                    self.service(vector)?;
                    if self.continuation.is_some()
                        && (self.storage_wait || !self.resume_storage()?)
                    {
                        return Ok(1);
                    }
                    if self.status != SessionStatus::Running {
                        return Ok(1);
                    }
                }
            }
        }
        let (outcome, consumed) = if OBSERVE {
            let changed = self.machine.ram_writes() != before_service_writes;
            let device_accesses = self.machine.device_accesses() != before_service_devices;
            let observation = observation.as_mut().unwrap();
            let result = self.machine.step_observed(observation)?;
            observation.service_writes_unobserved = changed;
            observation.service_device_accesses_unobserved = device_accesses;
            result
        } else if let Some(head_ip) = self.idle_yield_ip() {
            self.machine.set_batch_yield_ip(Some(head_ip));
            let result = self.machine.step_with_idle_budget(budget);
            self.machine.set_batch_yield_ip(None);
            result?
        } else {
            self.machine.step_with_idle_budget(budget)?
        };
        if outcome == StepOutcome::Halted {
            self.status = SessionStatus::Halted;
        }
        Ok(consumed)
    }
    fn service(&mut self, vector: u8) -> Result<(), RuntimeError> {
        let ax = self.machine.cpu.registers.get(Gpr16::Ax);
        // Host services can mutate device state directly, outside port counters.
        self.analyzer_recurrence_service(vector, ax);
        self.idle_service(vector, ax);
        if vector == 0x10 && ax >> 8 == 0x12 && self.machine.cpu.registers.get(Gpr16::Bx) == 0x80 {
            // Cirrus Logic VGA-type inquiry; no Cirrus extension BIOS is installed.
            return Ok(());
        }
        if vector == 0x10
            && matches!(ax, 0x6F00 | 0x5F00)
            && self.machine.cpu.registers.get(Gpr16::Bx) == 0
        {
            // No Video7/VEGA or Chips & Technologies extension BIOS signatures.
            return Ok(());
        }
        if vector == 0x2F && ax == 0xBC00 && self.machine.cpu.registers.get(Gpr16::Bx) == 0x3F3F {
            // MediaVision MVSOUND.SYS installation check; that driver is absent.
            return Ok(());
        }
        match (vector, ax >> 8) {
            (0x09, _) => self.keyboard_irq()?,
            (0x33, _) => self.mouse_service(ax)?,
            (0x10, 0x4F) => self.vesa_service(ax as u8)?,
            (0x10, 0x10) => self.palette_service(ax)?,
            (0x16, _) => self.keyboard_service(ax)?,
            (0x21, 0x01 | 0x06 | 0x07 | 0x08 | 0x0B | 0x0C) => self.dos_keyboard_service(ax)?,
            (0x21, 0x02) => {
                let byte = self.registers().get_byte(2);
                if byte == 9 {
                    let page = self.machine.bus.read_u8(0x462)?;
                    let position = self.machine.bus.read_u16(0x450 + u32::from(page) * 2)?;
                    for _ in 0..8 - (position & 7) {
                        self.console_output(b' ')?;
                    }
                    self.machine.cpu.registers.set_byte(0, b' ');
                } else {
                    self.console_output(byte)?;
                    self.machine.cpu.registers.set_byte(0, byte);
                }
            }
            (0x1A, 0) => {
                let low = self.machine.bus.read_u16(0x46C)?;
                let high = self.machine.bus.read_u16(0x46E)?;
                let rollover = self.machine.bus.read_u8(0x470)?;
                self.machine.cpu.registers.set(Gpr16::Cx, high);
                self.machine.cpu.registers.set(Gpr16::Dx, low);
                self.machine.cpu.registers.set_byte(0, rollover);
                self.machine.bus.write_u8(0x470, 0)?;
            }
            (0x1A, 2) => {
                // Read the installed CMOS clock, including its update-in-progress
                // flag. This BIOS service does not introduce a host wall clock.
                let cmos = &mut self.machine.bus.cmos;
                let selected = cmos.selected();
                cmos.select(0x0A);
                let updating = cmos.read() & 0x80 != 0;
                if !updating {
                    let mut time = [0; 4];
                    for (value, register) in time.iter_mut().zip([4, 2, 0, 0x0B]) {
                        cmos.select(register);
                        *value = cmos.read();
                    }
                    let r = &mut self.machine.cpu.registers;
                    r.set(Gpr16::Cx, u16::from_be_bytes([time[0], time[1]]));
                    r.set(Gpr16::Dx, u16::from_be_bytes([time[2], time[3] & 1]));
                }
                cmos.select(selected);
                self.set_carry(updating)?;
            }
            (0x10, 0x08..=0x0A | 0x0E | 0x06 | 0x07 | 0x01 | 0x05) => self.text_service(ax)?,
            (0x10, 2 | 3) if self.machine.cpu.registers.get_byte(7) < 8 => {
                let page = self.machine.cpu.registers.get_byte(7);
                let position_address = 0x450 + u32::from(page) * 2;
                if ax >> 8 == 2 {
                    let position = self.machine.cpu.registers.get(Gpr16::Dx);
                    self.machine.bus.write_u16(position_address, position)?;
                    if page == self.machine.bus.read_u8(0x462)?
                        && self.machine.bus.vga.mode() == doslet_vga::VideoMode::Text3
                    {
                        let cell =
                            (position >> 8) * 80 + (position & 0xFF) + u16::from(page) * 0x800;
                        let port = self.machine.bus.vga.input_status_port() - 6;
                        self.machine.bus.vga.write_port(port, 0x0E);
                        self.machine.bus.vga.write_port(port + 1, (cell >> 8) as u8);
                        self.machine.bus.vga.write_port(port, 0x0F);
                        self.machine.bus.vga.write_port(port + 1, cell as u8);
                    }
                } else {
                    let position = self.machine.bus.read_u16(position_address)?;
                    let shape = self.machine.bus.read_u16(0x460)?;
                    self.machine.cpu.registers.set(Gpr16::Dx, position);
                    self.machine.cpu.registers.set(Gpr16::Cx, shape);
                }
            }
            (0x10, 0x1A) if ax as u8 == 0 => {
                self.machine.cpu.registers.set_byte(0, 0x1A);
                self.machine.cpu.registers.set(Gpr16::Bx, 8); // VGA with analog color display.
            }
            (0x10, 0x0F) => {
                let (mode, columns) = match self.machine.bus.vga.mode() {
                    doslet_vga::VideoMode::Mode13 => (0x13, 40),
                    doslet_vga::VideoMode::Text3 => {
                        (3 | u16::from(self.machine.bus.read_u8(0x487)? & 0x80), 80)
                    }
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
            // Boot drive C:, and the true version 5.0 that 30h reports.
            (0x21, 0x33) if ax as u8 == 5 => self.machine.cpu.registers.set_byte(2, 3),
            (0x21, 0x33) if ax as u8 == 6 => {
                self.machine.cpu.registers.set(Gpr16::Bx, 0x0005);
                self.machine.cpu.registers.set(Gpr16::Dx, 0);
            }
            (0x08, _) => {
                // BIOS floppy-motor timeout is also used as a one-tick delay
                // by CIV's graphics driver. The counter must advance with IRQ0.
                let motor_timeout = self.machine.bus.read_u8(0x440)?;
                if motor_timeout != 0 {
                    self.machine.bus.write_u8(0x440, motor_timeout - 1)?;
                }
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
            (0x2F, _) if ax == 0x1680 => {
                self.machine.cpu.registers.set_byte(0, 0);
                self.timeslice_yielded = true;
            }
            (0x2F, _) if ax == 0x1687 => {
                self.dpmi_probe();
            }
            (0x2F, _) if ax == 0x1600 => {
                self.machine.cpu.registers.set_byte(0, 0); // Windows is absent.
            }
            // Without Windows, the version query leaves AX unchanged.
            (0x2F, _) if ax == 0x160A => {}
            (0x2F, _) if ax == 0x1500 => {
                self.mscdex_service(ax)?;
            }
            (0x2F, _) if ax >> 8 == 0x15 => {
                self.mscdex_service(ax)?;
            }
            (0x2F, _) if ax == 0x4300 => {
                self.machine.cpu.registers.set_byte(0, 0); // No external XMS driver installed.
            }
            (0x15, 0x88) => {
                let kilobytes = (self.machine.bus.ram_size() - 0x100000) / 1024;
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
            (0x10, 0) if ax as u8 & 0x7F == 3 => {
                self.machine.bus.vga.set_mode_3h();
                self.mouse_video_mode_changed();
                bios::set_text_mode(&mut self.machine.bus, ax as u8 & 0x80 != 0)?;
            }
            (0x10, 0) if ax as u8 == 0x13 => {
                self.machine.bus.set_video_mode_13h();
                self.mouse_video_mode_changed();
            }
            (0x20, _) => {
                self.finish_exit(0)?;
            }
            (0x21, 0x4C) => {
                self.finish_exit(ax as u8)?;
            }
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
            (0x21, 0x36) => {
                let drive = self.machine.cpu.registers.get_byte(2); // DL: 0 default, 1 A:, 3 C:.
                let space = if drive == 0 || drive == 3 {
                    Some(self.drive.disk_space())
                } else {
                    self.cdrom
                        .as_ref()
                        .filter(|cd| drive == cd.drive - b'A' + 1)
                        .map(|cd| (0, cd.length() / 512))
                };
                if let Some((free, total)) = space {
                    // Keep the whole volume representable in DOS's 16-bit counts.
                    let sectors = total.div_ceil(65535).max(1).next_power_of_two().min(32768);
                    let r = &mut self.machine.cpu.registers;
                    r.set(Gpr16::Ax, sectors as u16);
                    r.set(Gpr16::Bx, (free / sectors).min(65535) as u16);
                    r.set(Gpr16::Cx, 512);
                    r.set(Gpr16::Dx, total.div_ceil(sectors).min(65535) as u16);
                } else {
                    self.machine.cpu.registers.set(Gpr16::Ax, 0xFFFF);
                }
            }
            (0x21, 0x2A) => {
                // Same deterministic boot date as the installed CMOS.
                self.machine.cpu.registers.set(Gpr16::Cx, 2026);
                self.machine.cpu.registers.set(Gpr16::Dx, 0x0A0E);
                self.machine.cpu.registers.set_byte(0, 3);
            }
            (0x21, 0x2C) => {
                let centiseconds = self.dos_time_centis();
                self.machine.cpu.registers.set(
                    Gpr16::Cx,
                    (((centiseconds / 360000) % 24) << 8 | ((centiseconds / 6000) % 60)) as u16,
                );
                self.machine.cpu.registers.set(
                    Gpr16::Dx,
                    (((centiseconds / 100) % 60) << 8 | (centiseconds % 100)) as u16,
                );
            }
            (0x21, 0x2D) => self.set_dos_time()?,
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
            (0x21, 0x11 | 0x12) => {
                let r = self.registers();
                let mut address = linear_address(r.ds, r.get(Gpr16::Dx));
                let extended = self.machine.bus.read_u8(address)? == 255;
                if extended {
                    address += 7;
                }
                let mut pattern = [0; 11];
                for (i, byte) in pattern.iter_mut().enumerate() {
                    *byte = self.machine.bus.read_u8(address + 1 + i as u32)?;
                }
                let drive = self.machine.bus.read_u8(address)?;
                let start = if ax >> 8 == 0x12 {
                    self.machine.bus.read_u32(address + 24)? as usize
                } else {
                    0
                };
                if matches!(drive, 0 | 3)
                    && let Some((next, name, length)) = self.drive.find_fcb(&pattern, start)
                {
                    self.machine.bus.write_u32(address + 24, next as u32)?;
                    let mut result = [0; 44];
                    let offset = if extended {
                        result[0] = 255;
                        result[6] = 0x20;
                        7
                    } else {
                        0
                    };
                    result[offset] = 3;
                    result[offset + 1..offset + 12].copy_from_slice(&name);
                    result[offset + 12] = 0x20;
                    result[offset + 29..offset + 33].copy_from_slice(&length.to_le_bytes());
                    let dta = linear_address(self.dta.0, self.dta.1);
                    for (i, byte) in result[..offset + 37].iter().enumerate() {
                        self.machine.bus.write_u8(dta + i as u32, *byte)?;
                    }
                    self.machine.cpu.registers.set_byte(0, 0);
                } else {
                    self.machine.cpu.registers.set_byte(0, 255);
                }
            }
            (0x21, 0x4E | 0x4F) => {
                let dta = linear_address(self.dta.0, self.dta.1);
                let first = ax >> 8 == 0x4E;
                let search = if first {
                    if let Some((path, length)) = self.read_path()? {
                        self.drive
                            .start_search(&path[..length], self.registers().get(Gpr16::Cx))
                    } else {
                        return Ok(());
                    }
                } else {
                    let mut state = [0; 21];
                    for (i, byte) in state.iter_mut().enumerate() {
                        *byte = self.machine.bus.read_u8(dta + i as u32)?;
                    }
                    self.drive.resume_search(&state)
                };
                match search {
                    Ok(mut search) => {
                        if first {
                            for (i, byte) in search.state().iter().enumerate() {
                                self.machine.bus.write_u8(dta + i as u32, *byte)?;
                            }
                        }
                        if let Some(entry) = self.drive.next_search(&mut search) {
                            let mut result = [0; 43];
                            result[..21].copy_from_slice(&search.state());
                            result[21] = entry.attributes;
                            result[22..24].copy_from_slice(&0x6000u16.to_le_bytes());
                            result[24..26].copy_from_slice(&0x5D4Eu16.to_le_bytes()); // 2026-10-14.
                            result[26..30].copy_from_slice(&entry.length.to_le_bytes());
                            result[30..].copy_from_slice(&entry.name);
                            for (i, byte) in result.iter().enumerate() {
                                self.machine.bus.write_u8(dta + i as u32, *byte)?;
                            }
                            self.machine.cpu.registers.set(Gpr16::Ax, 0);
                            self.set_carry(false)?;
                        } else {
                            self.dos_error(if first { 2 } else { 18 })?;
                        }
                    }
                    Err(code) => self.dos_error(code)?,
                }
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
                if handle <= 2 || self.file_is_open(handle) {
                    let info = if handle <= 2 {
                        0x80D3
                    } else if self
                        .backend_handle(handle)
                        .is_ok_and(CdromImage::owns_handle)
                    {
                        u16::from(self.cdrom.as_ref().unwrap().drive - b'A')
                    } else {
                        2
                    };
                    self.machine.cpu.registers.set(Gpr16::Dx, info);
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
                let cd = self
                    .cdrom
                    .as_mut()
                    .filter(|cd| cd.matches_path(&name[..length]));
                let result = if let Some(cd) = cd {
                    if ax >> 8 == 0x3D {
                        cd.open(&name[..length], ax as u8)
                    } else if ax as u8 == 0 {
                        cd.attributes(&name[..length])
                    } else {
                        Err(5)
                    }
                } else if ax >> 8 == 0x3D {
                    self.drive.open(&name[..length], ax as u8)
                } else if ax as u8 == 0 {
                    self.drive.attributes(&name[..length])
                } else {
                    Err(5)
                };
                let result = if ax >> 8 == 0x3D {
                    result.and_then(|handle| self.register_handle(handle, ax as u8))
                } else {
                    result
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
            (0x21, 0x3C) => {
                if !self.handles.has_capacity() {
                    self.dos_error(4)?;
                    return Ok(());
                }
                let Some((name, length)) = self.read_path()? else {
                    return Ok(());
                };
                match self
                    .drive
                    .create(&name[..length], self.registers().get(Gpr16::Cx))
                {
                    Ok(handle) => {
                        let handle = match self.register_handle(handle, 2) {
                            Ok(handle) => handle,
                            Err(code) => {
                                self.dos_error(code)?;
                                return Ok(());
                            }
                        };
                        self.machine.cpu.registers.set(Gpr16::Ax, handle);
                        self.begin_commit()?;
                        self.set_carry(false)?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x57) if ax as u8 == 0 => {
                if self.file_is_open(self.registers().get(Gpr16::Bx)) {
                    self.machine.cpu.registers.set(Gpr16::Cx, 0x6000); // Deterministic noon boot timestamp.
                    self.machine
                        .cpu
                        .registers
                        .set(Gpr16::Dx, (46 << 9) | (10 << 5) | 14);
                    self.set_carry(false)?;
                } else {
                    self.dos_error(6)?;
                }
            }
            (0x21, 0x3E) => {
                let public = self.machine.cpu.registers.get(Gpr16::Bx);
                let result = self.close_handle(public);
                match result {
                    Ok(()) => {
                        self.set_carry(false)?;
                        self.begin_commit()?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x3F) => {
                let r = &self.machine.cpu.registers;
                let (ds, dx, count, public) =
                    (r.ds, r.get(Gpr16::Dx), r.get(Gpr16::Cx), r.get(Gpr16::Bx));
                let handle = match self.backend_handle(public) {
                    Ok(h) => h,
                    Err(code) => {
                        self.dos_error(code)?;
                        return Ok(());
                    }
                };
                let result = if CdromImage::owns_handle(handle) {
                    self.cdrom
                        .as_mut()
                        .ok_or(6u16)
                        .and_then(|cd| cd.prepare_read(handle, count))
                } else {
                    self.drive.prepare_read(handle, count)
                };
                match result {
                    Ok((position, length)) => {
                        self.continuation = Some(storage::Continuation::ReadStage {
                            handle,
                            position,
                            target: storage::CopyTarget::Segment(ds, dx),
                            length,
                            done: 0,
                        })
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x40) => {
                let r = &self.machine.cpu.registers;
                let count = usize::from(r.get(Gpr16::Cx));
                let public = r.get(Gpr16::Bx);
                let backend = self.backend_handle(public);
                if backend.is_ok_and(CdromImage::owns_handle) {
                    self.dos_error(if self.file_is_open(r.get(Gpr16::Bx)) {
                        5
                    } else {
                        6
                    })?;
                } else if !matches!(r.get(Gpr16::Bx), 1 | 2) {
                    let (ds, dx) = (r.ds, r.get(Gpr16::Dx));
                    let handle = match backend {
                        Ok(handle) => handle,
                        Err(code) => {
                            self.dos_error(code)?;
                            return Ok(());
                        }
                    };
                    match self.drive.write_buffer(handle, r.get(Gpr16::Cx)) {
                        Ok(_) => {
                            if count == 0 {
                                self.machine.cpu.registers.set(Gpr16::Ax, 0);
                                self.set_carry(false)?;
                            } else {
                                self.continuation = Some(storage::Continuation::WriteStage {
                                    handle,
                                    segment: ds,
                                    offset: dx,
                                    length: count,
                                    done: 0,
                                });
                            }
                        }
                        Err(code) => self.dos_error(code)?,
                    }
                } else {
                    let (ds, dx) = (r.ds, r.get(Gpr16::Dx));
                    for offset in 0..count {
                        let byte = self
                            .machine
                            .bus
                            .read_u8(linear_address(ds, dx.wrapping_add(offset as u16)))?;
                        self.console_output(byte)?;
                    }
                    self.machine.cpu.registers.set(Gpr16::Ax, count as u16);
                    self.set_carry(false)?;
                }
            }
            (0x21, 0x42) => {
                let r = &self.machine.cpu.registers;
                let displacement =
                    ((u32::from(r.get(Gpr16::Cx)) << 16) | u32::from(r.get(Gpr16::Dx))) as i32;
                let handle = match self.backend_handle(r.get(Gpr16::Bx)) {
                    Ok(handle) => handle,
                    Err(code) => {
                        self.dos_error(code)?;
                        return Ok(());
                    }
                };
                let result = if CdromImage::owns_handle(handle) {
                    self.cdrom
                        .as_mut()
                        .ok_or(6u16)
                        .and_then(|cd| cd.seek(handle, ax as u8, displacement))
                } else {
                    self.drive.seek(handle, ax as u8, displacement)
                };
                match result {
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
            (0x21, 0x39 | 0x3A) => {
                let Some((name, length)) = self.read_path()? else {
                    return Ok(());
                };
                let result = if ax >> 8 == 0x39 {
                    self.drive.make_directory(&name[..length])
                } else {
                    self.drive.remove_directory(&name[..length])
                };
                match result {
                    Ok(()) => {
                        self.set_carry(false)?;
                        self.begin_commit()?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x41) => {
                let Some((name, length)) = self.read_path()? else {
                    return Ok(());
                };
                match self.drive.delete(&name[..length]) {
                    Ok(()) => {
                        self.set_carry(false)?;
                        self.begin_commit()?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x56) => {
                let Some((from, length)) = self.read_path()? else {
                    return Ok(());
                };
                let r = &self.machine.cpu.registers;
                let (es, di) = (r.es, r.get(Gpr16::Di));
                let mut to = [0; 128];
                let mut end = None;
                for (i, byte) in to.iter_mut().enumerate() {
                    *byte = self
                        .machine
                        .bus
                        .read_u8(linear_address(es, di.wrapping_add(i as u16)))?;
                    if *byte == 0 {
                        end = Some(i);
                        break;
                    }
                }
                let result = if let Some(end) = end {
                    let source_drive = if from.get(1) == Some(&b':') {
                        from[0].to_ascii_uppercase()
                    } else {
                        b'C'
                    };
                    let target_drive = if to.get(1) == Some(&b':') {
                        to[0].to_ascii_uppercase()
                    } else {
                        b'C'
                    };
                    if source_drive != target_drive {
                        Err(17)
                    } else if source_drive != b'C' {
                        Err(5)
                    } else {
                        self.drive.rename(&from[..length], &to[..end])
                    }
                } else {
                    Err(3)
                };
                match result {
                    Ok(()) => {
                        self.set_carry(false)?;
                        self.begin_commit()?;
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x45 | 0x46) => {
                let source = self.registers().get(Gpr16::Bx);
                let result = if ax >> 8 == 0x45 {
                    self.handles.duplicate(source)
                } else {
                    let target = self.registers().get(Gpr16::Cx);
                    self.force_duplicate_handle(source, target).map(|()| target)
                };
                match result {
                    Ok(handle) => {
                        self.machine.cpu.registers.set(Gpr16::Ax, handle);
                        self.set_carry(false)?;
                        if ax >> 8 == 0x46 {
                            self.begin_commit()?;
                        }
                    }
                    Err(code) => self.dos_error(code)?,
                }
            }
            (0x21, 0x68 | 0x6A) => {
                if self.file_is_open(self.registers().get(Gpr16::Bx)) {
                    self.set_carry(false)?;
                    self.begin_commit()?;
                } else {
                    self.dos_error(6)?;
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
            // Without upper memory blocks, first fit is the only allocation
            // strategy and the UMB link stays off; requests to change either
            // succeed without effect.
            (0x21, 0x58) => match ax as u8 {
                0 => {
                    self.machine.cpu.registers.set(Gpr16::Ax, 0);
                    self.set_carry(false)?;
                }
                1 | 3 => self.set_carry(false)?,
                2 => {
                    self.machine.cpu.registers.set_byte(0, 0);
                    self.set_carry(false)?;
                }
                _ => self.dos_error(1)?,
            },
            (0x21, 9) => {
                let ds = self.machine.cpu.registers.ds;
                let mut offset = self.machine.cpu.registers.get(Gpr16::Dx);
                for _ in 0..65536 {
                    let byte = self.machine.bus.read_u8(linear_address(ds, offset))?;
                    if byte == b'$' {
                        break;
                    }
                    self.console_output(byte)?;
                    offset = offset.wrapping_add(1);
                }
                self.machine.cpu.registers.set_byte(0, b'$');
            }
            _ => return Err(RuntimeError::UnsupportedService { vector, ax }),
        }
        Ok(())
    }
    fn mscdex_service(&mut self, ax: u16) -> Result<(), RuntimeError> {
        let installed = self.cdrom.is_some();
        if ax == 0x1500 {
            self.machine
                .cpu
                .registers
                .set(Gpr16::Bx, u16::from(installed));
            self.machine.cpu.registers.set(
                Gpr16::Cx,
                self.cdrom
                    .as_ref()
                    .map_or(0, |cd| u16::from(cd.drive - b'A')),
            );
            if installed {
                self.machine.cpu.registers.set_byte(0, 0xFF);
            }
            self.set_carry(false)?;
            return Ok(());
        }
        let cdrom = self.cdrom.as_ref();
        let drive_number = cdrom.map_or(0, |cd| u16::from(cd.drive - b'A'));
        let es = self.machine.cpu.registers.es;
        let bx = self.machine.cpu.registers.get(Gpr16::Bx);
        let buffer = linear_address(es, bx);
        match ax {
            0x1501 => {
                self.machine.cpu.registers.set(Gpr16::Ax, 1);
                self.set_carry(true)?;
            }
            0x1502..=0x1504 | 0x1505 | 0x1508 => {
                let drive = self.machine.cpu.registers.get(Gpr16::Cx);
                if self
                    .cdrom
                    .as_ref()
                    .is_none_or(|cd| u16::from(cd.drive - b'A') != drive)
                {
                    self.dos_error(15)?;
                    return Ok(());
                }
                let result = if matches!(ax, 0x1502..=0x1504) {
                    let cd = self.cdrom.as_mut().unwrap();
                    match cd.primary() {
                        Ok(primary) => cd
                            .raw(
                                primary + 702 + u64::from((ax as u8).saturating_sub(2)) * 37,
                                &mut self.staging[..37],
                            )
                            .map(|_| {
                                let length = self.staging[..37]
                                    .iter()
                                    .position(|b| matches!(b, 0 | b' '))
                                    .unwrap_or(37);
                                self.staging[length] = 0;
                                (length + 1, ax)
                            }),
                        Err(code) => Err(code),
                    }
                } else if ax == 0x1505 {
                    let volume = self.machine.cpu.registers.get(Gpr16::Dx);
                    self.cdrom
                        .as_mut()
                        .unwrap()
                        .raw((16 + u64::from(volume)) * 2048, &mut self.staging[..2048])
                        .map(|count| (count, u16::from(self.staging[0])))
                } else {
                    let r = &self.machine.cpu.registers;
                    let count = usize::from(r.get(Gpr16::Dx));
                    if count > 32 {
                        return Err(RuntimeError::UnsupportedService { vector: 0x2F, ax });
                    }
                    let sector = (u64::from(r.get(Gpr16::Si)) << 16) | u64::from(r.get(Gpr16::Di));
                    if count > 2 {
                        self.continuation = Some(storage::Continuation::RawStage {
                            offset: sector * 2048,
                            target: storage::CopyTarget::Linear(buffer),
                            length: count * 2048,
                            done: 0,
                            ax: 0,
                        });
                        return Ok(());
                    }
                    self.cdrom
                        .as_mut()
                        .unwrap()
                        .raw(sector * 2048, &mut self.staging[..count * 2048])
                        .map(|count| (count, 0))
                };
                match result {
                    Ok((length, value)) => {
                        self.begin_copy(storage::CopyTarget::Linear(buffer), length, value)
                    }
                    Err(code) => self.dos_error(if ax == 0x1508 && code != cdrom::PENDING {
                        15
                    } else {
                        code
                    })?,
                }
            }
            0x150B => {
                let drive = self.machine.cpu.registers.get(Gpr16::Cx);
                let valid = cdrom.is_some() && drive == drive_number;
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Ax, if valid { 0x5AD8 } else { 0 });
                self.machine.cpu.registers.set(Gpr16::Bx, 0xADAD);
                self.set_carry(false)?;
            }
            0x150C => {
                self.machine.cpu.registers.set(Gpr16::Bx, 0x0217);
                self.set_carry(false)?;
            }
            0x150D => {
                if let Some(cd) = cdrom {
                    self.machine.bus.write_u8(buffer, cd.drive - b'A')?;
                }
                self.set_carry(false)?;
            }
            0x150E => {
                let drive = self.machine.cpu.registers.get(Gpr16::Cx);
                let operation = self.machine.cpu.registers.get(Gpr16::Bx);
                if cdrom.is_some() && drive == drive_number && operation <= 1 {
                    if operation == 0 {
                        self.machine.cpu.registers.set(Gpr16::Dx, 0x0100);
                    } else if self.machine.cpu.registers.get_byte(6) != 1 {
                        self.machine.cpu.registers.set(Gpr16::Ax, 1);
                        self.set_carry(true)?;
                        return Ok(());
                    }
                    self.set_carry(false)?;
                } else {
                    self.machine
                        .cpu
                        .registers
                        .set(Gpr16::Ax, if cdrom.is_some() { 1 } else { 15 });
                    self.set_carry(true)?;
                }
            }
            0x1509 => {
                self.machine.cpu.registers.set(Gpr16::Ax, 1);
                self.set_carry(true)?;
            }
            0x1506 | 0x1507 | 0x150A => self.set_carry(false)?,
            _ => {
                self.machine.cpu.registers.set(Gpr16::Ax, 1);
                self.set_carry(true)?;
            }
        }
        Ok(())
    }

    fn file_is_open(&self, handle: u16) -> bool {
        self.handles.get(handle).is_ok()
    }

    fn set_carry(&mut self, carry: bool) -> Result<(), BusError> {
        if let Some(flags) = &mut self.service_flags {
            *flags = (*flags & !1) | u16::from(carry);
            return Ok(());
        }
        let r = &self.machine.cpu.registers;
        let address = linear_address(r.ss, r.get(Gpr16::Sp).wrapping_add(4));
        let flags = self.machine.bus.read_u16(address)?;
        self.machine
            .bus
            .write_u16(address, (flags & !1) | u16::from(carry))
    }
    fn dos_error(&mut self, code: u16) -> Result<(), BusError> {
        if code == cdrom::PENDING {
            self.continuation = Some(storage::Continuation::Resolve(self.active_service));
            self.storage_wait = true;
            return Ok(());
        }
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
    use doslet_x86::Gpr32;

    /// Run a COM program for `steps` and return a fingerprint of its state.
    fn run_fingerprint(
        program: &[u8],
        steps: u32,
        idle_loops: bool,
    ) -> (Vec<u8>, [u32; 9], u64, u64, IdleLoopStats) {
        let mut session = Session::new(program).unwrap();
        session.set_idle_loops(idle_loops);
        session.start();
        let mut remaining = steps;
        while remaining != 0 && session.status() == SessionStatus::Running {
            remaining -= session.advance(remaining.min(10_000));
        }
        let r = session.registers();
        let registers = [
            r.get32(Gpr32::Eax),
            r.get32(Gpr32::Ecx),
            r.get32(Gpr32::Edx),
            r.get32(Gpr32::Ebx),
            r.get32(Gpr32::Esp),
            r.get32(Gpr32::Ebp),
            r.get32(Gpr32::Esi),
            r.get32(Gpr32::Edi),
            r.ip,
        ];
        let ram = session.machine.bus.ram_view(0..0xA0000).unwrap().to_vec();
        (
            ram,
            registers,
            session.steps(),
            session.emulated_nanos(),
            session.idle_loop_stats(),
        )
    }

    #[test]
    fn proven_idle_loops_skip_passes_without_changing_guest_state() {
        // Wait for a counter the timer interrupt never changes, polling the
        // keyboard through INT 16h/01h on each pass.
        let program = [
            0xB4, 0x01, // mov ah,1
            0xCD, 0x16, // int 16h
            0xA1, 0x00, 0x02, // mov ax,[0200h]
            0x85, 0xC0, // test ax,ax
            0x74, 0xF5, // jz 0100h
            0xF4, // hlt
        ];
        let skipped = run_fingerprint(&program, 3_000_000, true);
        let reference = run_fingerprint(&program, 3_000_000, false);
        assert_eq!(skipped.0, reference.0, "guest RAM");
        assert_eq!(skipped.1, reference.1, "registers");
        assert_eq!(
            (skipped.2, skipped.3),
            (reference.2, reference.3),
            "steps and time"
        );
        assert!(
            skipped.4.proofs > 0 && skipped.4.skipped_steps > 1_000_000,
            "{:?}",
            skipped.4
        );
        assert_eq!(reference.4.skipped_steps, 0);
        assert!(skipped.4.skipped_nanos > 0);
        assert!(skipped.4.skipped_nanos <= skipped.3);
        assert_eq!(reference.4.skipped_nanos, 0);
    }

    #[test]
    fn idle_proofs_reject_passes_that_change_memory() {
        // Increment a word on each pass: never a fixed point.
        let program = [
            0xFF, 0x06, 0x00, 0x02, // inc word [0200h]
            0xEB, 0xFA, // jmp 0100h
        ];
        let skipped = run_fingerprint(&program, 500_000, true);
        let reference = run_fingerprint(&program, 500_000, false);
        assert_eq!(skipped.0, reference.0);
        assert_eq!(skipped.1, reference.1);
        assert_eq!(skipped.4.skipped_steps, 0);
    }
    #[test]
    fn dos_disk_space_uses_overlay_budget_default_drive_and_invalid_drive_sentinel() {
        let mut drive = ReadOnlyDrive::default().with_write_overlay(1024);
        drive.mount(b"HEROES.CFG", alloc::vec![0; 137]).unwrap();
        let mut session = Session::new(&[0x90]).unwrap();
        session.drive = drive;
        for disk in [0, 3] {
            session.machine.cpu.registers.set(Gpr16::Dx, disk);
            session.machine.cpu.flags |= doslet_x86::flags::CARRY;
            session.machine.cpu.registers.set(Gpr16::Ax, 0x3603);
            session.service(0x21).unwrap();
            assert_eq!(session.registers().get(Gpr16::Ax), 1);
            assert_eq!(session.registers().get(Gpr16::Bx), 2);
            assert_eq!(session.registers().get(Gpr16::Cx), 512);
            assert_eq!(session.registers().get(Gpr16::Dx), 4);
            assert_ne!(session.flags() & doslet_x86::flags::CARRY as u16, 0);
        }
        let handle = session.drive.open(b"HEROES.CFG", 2).unwrap();
        session.drive.write_buffer(handle, 513).unwrap().fill(1);
        session.machine.cpu.registers.set(Gpr16::Dx, 3);
        session.machine.cpu.registers.set(Gpr16::Ax, 0x3603);
        session.service(0x21).unwrap();
        assert_eq!(session.registers().get(Gpr16::Bx), 0);
        for disk in [1, 2, 4, 26, 255] {
            session.machine.cpu.registers.set(Gpr16::Dx, disk);
            session.machine.cpu.registers.set(Gpr16::Ax, 0x3600);
            session.service(0x21).unwrap();
            assert_eq!(session.registers().get(Gpr16::Ax), 0xFFFF);
        }
        session.drive = ReadOnlyDrive::default();
        session.machine.cpu.registers.set(Gpr16::Dx, 0);
        session.machine.cpu.registers.set(Gpr16::Ax, 0x3600);
        session.service(0x21).unwrap();
        assert_eq!(session.registers().get(Gpr16::Bx), 0);
    }
    #[test]
    fn relative_mouse_retains_fractions_and_raw_motion_at_guest_edges() {
        let mut session = Session::new(&[0x90]).unwrap();
        session.machine.cpu.registers.set(Gpr16::Bx, 0x101);
        session.vesa_service(2).unwrap();
        for _ in 0..4 {
            session.pointer_relative(0.25, -0.25);
        }
        assert_eq!(session.pointer_position(), (321, 239));
        session.mouse_service(0xB).unwrap();
        assert_eq!(session.registers().get(Gpr16::Cx) as i16, 1);
        assert_eq!(session.registers().get(Gpr16::Dx) as i16, -1);
        session.pointer_relative(1000.25, -1000.25);
        assert_eq!(session.pointer_position(), (639, 0));
        session.mouse_service(0xB).unwrap();
        assert_eq!(session.registers().get(Gpr16::Cx) as i16, 1000);
        assert_eq!(session.registers().get(Gpr16::Dx) as i16, -1000);
        session.pointer_relative(20.0, -20.0);
        session.mouse_service(0xB).unwrap();
        assert_eq!(session.registers().get(Gpr16::Cx) as i16, 20);
        assert_eq!(session.registers().get(Gpr16::Dx) as i16, -20);
        session.pointer_relative(-1.0, 1.0);
        assert_eq!(session.pointer_position(), (638, 1));
        session.pointer_relative(f64::NAN, 3.0);
        assert_eq!(session.pointer_position(), (638, 1));
        session.pointer_move(320, 240);
        session.pointer_relative(0.75, 0.75);
        assert_eq!(session.pointer_position(), (320, 240));
    }
    #[test]
    fn video_mode_changes_update_mouse_bounds_and_preserve_relative_position() {
        let mut session = Session::new(&[0x90]).unwrap();
        session.machine.cpu.registers.set(Gpr16::Bx, 0x101);
        session.vesa_service(2).unwrap();
        assert_eq!(session.pointer_position(), (320, 240));
        assert_eq!(session.pointer_ranges(), ((0, 639), (0, 479)));
        session.pointer_move(500, 370);
        session.mouse_service(3).unwrap();
        assert_eq!(session.registers().get(Gpr16::Cx), 500);
        assert_eq!(session.registers().get(Gpr16::Dx), 370);
        session.machine.bus.vga.set_mode_13h();
        session.mouse_video_mode_changed();
        assert_eq!(session.pointer_ranges().1, (0, 199));
        assert!(session.pointer_position().1 < 200);
    }
    #[test]
    fn absent_vendor_video_bios_probes_keep_signatures_and_flags_unchanged() {
        let mut session = Session::new(&[0x90]).unwrap();
        for (ax, bx) in [(0x6F00, 0), (0x5F00, 0), (0x12FF, 0x80)] {
            session.machine.cpu.registers.set(Gpr16::Ax, ax);
            session.machine.cpu.registers.set(Gpr16::Bx, bx);
            session.machine.cpu.registers.set(Gpr16::Cx, 0x1234);
            let flags = session.flags();
            session.service(0x10).unwrap();
            assert_eq!(session.registers().get(Gpr16::Ax), ax);
            assert_eq!(session.registers().get(Gpr16::Bx), bx);
            assert_eq!(session.registers().get(Gpr16::Cx), 0x1234);
            assert_eq!(session.flags(), flags);
        }
        session.machine.cpu.registers.set(Gpr16::Ax, 0x6F05);
        assert!(matches!(
            session.service(0x10),
            Err(RuntimeError::UnsupportedService { .. })
        ));
    }
    #[test]
    fn bios_palette_and_overscan_write_six_bit_attribute_registers() {
        use doslet_bus::PortBus;
        let mut session = Session::new(&[0x90]).unwrap();
        for (ax, bx, index, expected) in [(0x1000, 0x7F05, 5, 0x3F), (0x1001, 0x1000, 0x11, 0x10)] {
            session.machine.cpu.registers.set(Gpr16::Bx, bx);
            session.palette_service(ax).unwrap();
            session.machine.bus.read_port_u8(0x3DA).unwrap();
            session.machine.bus.write_port_u8(0x3C0, index).unwrap();
            assert_eq!(session.machine.bus.read_port_u8(0x3C1), Ok(expected));
        }
    }
    #[test]
    fn mouse_state_size_save_and_restore_round_trip() {
        let mut session = Session::new(&[0x90]).unwrap();
        session.mouse_service(0x15).unwrap();
        assert_eq!(session.registers().get(Gpr16::Bx), 160);
        session.machine.cpu.registers.es = 0x1000;
        session.machine.cpu.registers.set(Gpr16::Dx, 0x400);
        session.pointer_move(42, 34);
        session.pointer_button(0, true);
        session.mouse_service(0x16).unwrap();
        session.pointer_move(100, 150);
        session.pointer_button(0, false);
        session.mouse_service(0x17).unwrap();
        assert_eq!(
            (session.mouse.x, session.mouse.y, session.mouse.buttons),
            (42, 34, 1)
        );
        assert_eq!(session.mouse.presses[0].count, 1);
        assert_eq!(session.mouse.releases[0].count, 0);
    }
    #[test]
    fn mouse_callback_receives_events_and_restores_registers_and_stack() {
        let mut session = Session::new(&[0xFB, 0x90, 0xEB, 0xFD]).unwrap();
        // Callback records AX, overwrites a register, and returns with RETF.
        for (i, byte) in [0xA3, 0x00, 0x06, 0xB8, 0x34, 0x12, 0xCB]
            .into_iter()
            .enumerate()
        {
            session
                .machine
                .bus
                .write_u8(0x10200 + i as u32, byte)
                .unwrap();
        }
        session.machine.cpu.registers.es = 0x1000;
        session.machine.cpu.registers.set(Gpr16::Ax, 0x0C);
        session.machine.cpu.registers.set(Gpr16::Cx, 7);
        session.machine.cpu.registers.set(Gpr16::Dx, 0x200);
        session.mouse_service(0x0C).unwrap();
        session.machine.cpu.registers.set(Gpr16::Ax, 0xABCD);
        let stack = session.registers().get(Gpr16::Sp);
        session.start();
        session.advance(10);
        session.pointer_move(42, 34);
        session.pointer_button(0, true);
        session.advance(100);
        assert_eq!(session.fault(), None);
        assert_eq!(session.machine.bus.read_u16(0x10600).unwrap(), 3);
        assert_eq!(session.registers().get(Gpr16::Ax), 0xABCD);
        assert_eq!(session.registers().get(Gpr16::Sp), stack);
        session.pointer_button(0, false);
        session.advance(100);
        assert_eq!(session.machine.bus.read_u16(0x10600).unwrap(), 4);
        assert!(!session.mouse.callback_active);
    }
    #[test]
    fn bios_writes_graphics_glyph_without_advancing_cursor() {
        let mut session = Session::new(&[0x90]).unwrap();
        session.machine.bus.vga.set_mode_13h();
        session.machine.cpu.registers.set(Gpr16::Ax, 0x0941);
        session.machine.cpu.registers.set(Gpr16::Bx, 12);
        session.machine.cpu.registers.set(Gpr16::Cx, 1);
        session.text_service(0x0941).unwrap();
        let frame = session.frame().unwrap();
        assert!(frame.pixels[..2560].contains(&12));
        assert_eq!(session.machine.bus.read_u16(0x450).unwrap(), 0);
    }
    #[test]
    fn bios_rtc_time_reads_cmos_and_reports_updates_through_returned_carry() {
        for updating in [false, true] {
            let program = [
                0xB9, 0xAA, 0xAA, 0xBA, 0xBB, 0xBB, // Sentinels in CX/DX.
                0xF9, 0xB8, 0, 2, 0xCD, 0x1A, // STC; BIOS read RTC time.
                0x9C, 0x5B, // Save returned FLAGS in BX.
                0xB8, 0, 0x4C, 0xCD, 0x21,
            ];
            let mut session = Session::new(&program).unwrap();
            let cmos = &mut session.machine.bus.cmos;
            for (register, value) in [(4, 0x23), (2, 0x58), (0, 0x59), (0x0B, 3)] {
                cmos.select(register);
                cmos.write(value);
            }
            if updating {
                cmos.select(0x0A);
                cmos.write(0xA6);
            }
            cmos.select(0x97);
            session.start();
            session.advance(100);
            assert_eq!(session.status(), SessionStatus::Exited(0));
            assert_eq!(session.registers().get(Gpr16::Bx) & 1, u16::from(updating));
            assert_eq!(
                session.registers().get(Gpr16::Cx),
                if updating { 0xAAAA } else { 0x2358 }
            );
            assert_eq!(
                session.registers().get(Gpr16::Dx),
                if updating { 0xBBBB } else { 0x5901 }
            );
            assert_eq!(session.machine.bus.cmos.selected(), 0x97);
        }
    }

    #[test]
    fn bios_timer_decrements_civs_floppy_timeout_delay() {
        let mut session = Session::new(&[0x90]).unwrap();
        session.machine.bus.write_u8(0x440, 2).unwrap();
        for expected in [1, 0, 0] {
            session.service(8).unwrap();
            assert_eq!(session.machine.bus.read_u8(0x440).unwrap(), expected);
        }
        assert_eq!(session.machine.bus.read_u16(0x46C).unwrap(), 3);
    }
    #[test]
    fn bios_palette_single_and_block_services_return_six_bit_components() {
        let mut session = Session::new(&[0x90]).unwrap();
        session.machine.cpu.registers.set(Gpr16::Ax, 0x1010);
        session.machine.cpu.registers.set(Gpr16::Bx, 7);
        session.machine.cpu.registers.set(Gpr16::Cx, 0x203F);
        session.machine.cpu.registers.set(Gpr16::Dx, 0x1000);
        session.service(0x10).unwrap();
        session.machine.cpu.registers.set(Gpr16::Ax, 0x1015);
        session.service(0x10).unwrap();
        assert_eq!(session.registers().get(Gpr16::Cx), 0x203F);
        assert_eq!(session.registers().get(Gpr16::Dx) >> 8, 0x10);
        session.machine.cpu.registers.es = 0x1000;
        session.machine.cpu.registers.set(Gpr16::Ax, 0x1017);
        session.machine.cpu.registers.set(Gpr16::Cx, 1);
        session.machine.cpu.registers.set(Gpr16::Dx, 0x200);
        session.service(0x10).unwrap();
        assert_eq!(
            session.ram_view(0x10200..0x10203).unwrap(),
            &[0x10, 0x20, 0x3F]
        );
    }
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
    fn cursor_shape_ignores_bh_and_updates_bios_and_crtc_state() {
        let mut session = Session::new(&[0x90]).unwrap();
        for bx in [0, 0x0700, 0xA008, 0xFFFF] {
            for shape in [0x2000, 0x000F, 0x0D0F] {
                session.machine.cpu.registers.set(Gpr16::Ax, 0x0100);
                session.machine.cpu.registers.set(Gpr16::Bx, bx);
                session.machine.cpu.registers.set(Gpr16::Cx, shape);
                session.machine.cpu.registers.set(Gpr16::Dx, 0x1234);
                let flags = session.flags();
                session.service(0x10).unwrap();
                assert_eq!(session.machine.bus.read_u16(0x460).unwrap(), shape);
                assert_eq!(session.registers().get(Gpr16::Ax), 0x0100);
                assert_eq!(session.registers().get(Gpr16::Bx), bx);
                assert_eq!(session.registers().get(Gpr16::Cx), shape);
                assert_eq!(session.registers().get(Gpr16::Dx), 0x1234);
                assert_eq!(session.flags(), flags);
                session.machine.bus.vga.write_port(0x3D4, 0x0A);
                assert_eq!(
                    session.machine.bus.vga.read_port(0x3D5),
                    Some((shape >> 8) as u8)
                );
                session.machine.bus.vga.write_port(0x3D4, 0x0B);
                assert_eq!(session.machine.bus.vga.read_port(0x3D5), Some(shape as u8));
                session.machine.cpu.registers.set(Gpr16::Ax, 0x0300);
                session.machine.cpu.registers.set(Gpr16::Bx, 0);
                session.machine.cpu.registers.set(Gpr16::Cx, 0);
                session.service(0x10).unwrap();
                assert_eq!(session.registers().get(Gpr16::Cx), shape);
            }
        }
    }

    #[test]
    fn guest_can_hide_cursor_with_rotts_unrelated_bx_value() {
        let mut session = Session::new(&[
            0xBB, 0x08, 0xA0, // MOV BX,A008h (not an input to AH=01h).
            0xB9, 0x00, 0x20, // MOV CX,2000h (hide cursor).
            0xB8, 0x00, 0x01, // MOV AX,0100h.
            0xCD, 0x10, 0xB8, 0x00, 0x4C, 0xCD, 0x21,
        ])
        .unwrap();
        session.start();
        session.advance(100);
        assert_eq!(session.status(), SessionStatus::Exited(0));
        assert_eq!(session.machine.bus.read_u16(0x460).unwrap(), 0x2000);
    }

    #[test]
    fn active_text_cursor_updates_crtc_and_inactive_page_keeps_it() {
        let mut session = Session::new(&[0x90]).unwrap();
        session.machine.cpu.registers.set(Gpr16::Ax, 3);
        session.service(0x10).unwrap();
        session.machine.cpu.registers.set(Gpr16::Ax, 0x0200);
        session.machine.cpu.registers.set(Gpr16::Bx, 0);
        session.machine.cpu.registers.set(Gpr16::Dx, 0x184F);
        session.service(0x10).unwrap();
        session.machine.bus.vga.write_port(0x3D4, 0x0E);
        assert_eq!(session.machine.bus.vga.read_port(0x3D5), Some(7));
        session.machine.bus.vga.write_port(0x3D4, 0x0F);
        assert_eq!(session.machine.bus.vga.read_port(0x3D5), Some(0xCF));
        session.machine.cpu.registers.set(Gpr16::Bx, 0x0700);
        session.machine.cpu.registers.set(Gpr16::Dx, 0x0102);
        session.service(0x10).unwrap();
        assert_eq!(session.machine.bus.vga.read_port(0x3D5), Some(0xCF));
        assert_eq!(session.machine.bus.read_u16(0x45E).unwrap(), 0x0102);
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

#[cfg(feature = "save-state")]
#[path = "session_saved.rs"]
mod saved_state;
