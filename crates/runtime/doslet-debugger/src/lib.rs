//! Native GDB transport. Owned and borrowed targets keep a single Session owner.
mod arch;
mod server;
use arch::{I386, Register, Regs};
use doslet_runtime::{Session, SessionStatus, session::debug::GENERAL};
use doslet_x86::SegmentRegister;
use gdbstub::{
    common::Signal,
    conn::{Connection, ConnectionExt},
    stub::{
        DisconnectReason, GdbStub, SingleThreadStopReason,
        run_blocking::{BlockingEventLoop, Event, WaitForStopReasonError},
    },
    target::{
        Target, TargetError, TargetResult,
        ext::{
            base::{
                BaseOps,
                single_register_access::{SingleRegisterAccess, SingleRegisterAccessOps},
                singlethread::{SingleThreadBase, SingleThreadResume, SingleThreadSingleStep},
            },
            breakpoints::{Breakpoints, SwBreakpoint},
            monitor_cmd::{ConsoleOutput, MonitorCmd},
        },
    },
};
pub use server::{Debugger, Execution, Normal};
use std::ops::{Deref, DerefMut};
use std::{
    error::Error,
    io::{self, Read, Write},
    marker::PhantomData,
    net::{SocketAddr, TcpListener, TcpStream},
};

pub type HostError = Box<dyn Error>;
type Stop = SingleThreadStopReason<u32>;

pub trait SessionOwner: Deref<Target = Session> + DerefMut {}
impl<T: Deref<Target = Session> + DerefMut> SessionOwner for T {}

pub struct OwnedSession(Session);
impl Deref for OwnedSession {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.0
    }
}
impl DerefMut for OwnedSession {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.0
    }
}

pub type DebugSession = Guest<'static, OwnedSession>;
impl From<Session> for DebugSession {
    fn from(session: Session) -> Self {
        Guest::new(OwnedSession(session), None, u64::MAX)
    }
}
impl<S: SessionOwner> Deref for Guest<'_, S> {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}
impl<S: SessionOwner> DerefMut for Guest<'_, S> {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.session
    }
}
type Tick<'a> = &'a mut dyn FnMut(&mut Session) -> Result<(), HostError>;
const HOT_SLOTS: usize = 8192;

/// EOF-aware connection: an orderly disconnect must not look like pending data.
pub struct ConnectionStream(TcpStream);
impl Connection for ConnectionStream {
    type Error = io::Error;
    fn write(&mut self, byte: u8) -> io::Result<()> {
        self.write_all(&[byte])
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.set_nonblocking(false)?;
        Write::write_all(&mut self.0, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.0)
    }
    fn on_session_start(&mut self) -> io::Result<()> {
        self.0.set_nodelay(true)
    }
}
impl ConnectionExt for ConnectionStream {
    fn read(&mut self) -> io::Result<u8> {
        self.0.set_nonblocking(false)?;
        let mut byte = [0];
        self.0.read_exact(&mut byte)?;
        Ok(byte[0])
    }
    fn peek(&mut self) -> io::Result<Option<u8>> {
        self.0.set_nonblocking(true)?;
        let mut byte = [0];
        match self.0.peek(&mut byte) {
            Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(_) => Ok(Some(byte[0])),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Hot {
    pc: u32,
    cs: u16,
    wide: bool,
    count: u64,
}
pub struct Guest<'a, S: SessionOwner = &'a mut Session> {
    session: S,
    tick: Option<Tick<'a>>,
    breakpoints: [Option<u32>; 64],
    limit: u64,
    stop_at: Option<u64>,
    single_step: bool,
    skip_breakpoint: bool,
    breakpoint_stop: Option<u32>,
    sampling: bool,
    hot: Box<[Hot]>,
    dropped: u64,
}
impl<'a, S: SessionOwner> Guest<'a, S> {
    fn new(session: S, tick: Option<Tick<'a>>, limit: u64) -> Self {
        Self {
            session,
            tick,
            breakpoints: [None; 64],
            limit,
            stop_at: None,
            single_step: false,
            skip_breakpoint: false,
            breakpoint_stop: None,
            sampling: false,
            hot: Box::default(),
            dropped: 0,
        }
    }
    fn gdb_registers(&self) -> Regs {
        let r = self.session.registers();
        let mut regs = Regs::default();
        regs.core[..8].copy_from_slice(&GENERAL.map(|reg| r.get32(reg)));
        regs.core[8] = self.session.debug_pc();
        regs.core[9] = self.session.eflags();
        regs.core[10..].copy_from_slice(&[r.cs, r.ss, r.ds, r.es, r.fs, r.gs].map(u32::from));
        regs.x87 = self.session.debug_x87().map(|image| {
            let mut bytes = [0; 112];
            bytes[..80].copy_from_slice(&image[28..]);
            for (i, (offset, size)) in [
                (0, 2),
                (4, 2),
                (8, 2),
                (16, 2),
                (12, 4),
                (24, 2),
                (20, 4),
                (18, 2),
            ]
            .into_iter()
            .enumerate()
            {
                bytes[80 + i * 4..80 + i * 4 + size].copy_from_slice(&image[offset..offset + size]);
            }
            bytes
        });
        regs
    }
    fn resume(&mut self, single_step: bool, signal: Option<Signal>) -> Result<(), HostError> {
        if signal.is_some_and(|s| {
            !matches!(
                s,
                Signal::SIGTRAP
                    | Signal::SIGINT
                    | Signal::SIGXCPU
                    | Signal::SIGILL
                    | Signal::SIGSTOP
            )
        }) {
            return Err("guest signal injection is unsupported".into());
        }
        self.single_step = single_step;
        self.skip_breakpoint =
            single_step || self.breakpoint_stop.take() == Some(self.session.debug_pc());
        self.session.start();
        Ok(())
    }
    fn sample(&mut self) {
        if !self.sampling {
            return;
        }
        let pc = self.session.debug_pc();
        let cs = self.session.registers().cs;
        let wide = self.session.registers().code32();
        let at = (pc.wrapping_mul(0x9e3779b1) as usize) & (HOT_SLOTS - 1);
        for index in 0..64 {
            let slot = &mut self.hot[(at + index) & (HOT_SLOTS - 1)];
            if slot.count == 0 || (slot.pc, slot.cs, slot.wide) == (pc, cs, wide) {
                *slot = Hot {
                    pc,
                    cs,
                    wide,
                    count: slot.count.saturating_add(1),
                };
                return;
            }
        }
        self.dropped = self.dropped.saturating_add(1);
    }
    fn stop(&mut self, reason: Stop) -> Stop {
        self.session.debug_suspend();
        reason
    }
    fn run_step(&mut self) -> Result<Option<Stop>, HostError> {
        let terminal = match self.session.status() {
            SessionStatus::Exited(code) => Some(Stop::Exited(code)),
            SessionStatus::Faulted => Some(Stop::Signal(Signal::SIGILL)),
            SessionStatus::Halted => Some(Stop::Signal(Signal::SIGSTOP)),
            _ => None,
        };
        if let Some(reason) = terminal {
            return Ok(Some(self.stop(reason)));
        }
        if self.session.steps() >= self.limit {
            return Ok(Some(self.stop(Stop::Signal(Signal::SIGXCPU))));
        }
        if self.stop_at.is_some_and(|at| self.session.steps() >= at) {
            self.stop_at = None;
            return Ok(Some(self.stop(Stop::Signal(Signal::SIGTRAP))));
        }
        let pc = self.session.debug_pc();
        if !self.skip_breakpoint && self.breakpoints.contains(&Some(pc)) {
            self.breakpoint_stop = Some(pc);
            return Ok(Some(self.stop(Stop::SwBreak(()))));
        }
        self.skip_breakpoint = false;
        self.sample();
        let completed = self.session.advance(1);
        if let Some(tick) = &mut self.tick {
            tick(&mut self.session)?;
        }
        if completed == 0
            && self.session.status() == SessionStatus::Running
            && !self.session.storage_waiting()
        {
            return Err("debugger stalled waiting for storage completion".into());
        }
        if self.single_step && completed != 0 {
            return Ok(Some(self.stop(Stop::DoneStep)));
        }
        Ok(None)
    }
}
impl<S: SessionOwner> Target for Guest<'_, S> {
    type Arch = I386;
    type Error = HostError;
    #[inline(always)]
    fn base_ops(&mut self) -> BaseOps<'_, I386, HostError> {
        BaseOps::SingleThread(self)
    }
    #[inline(always)]
    fn support_breakpoints(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::BreakpointsOps<'_, Self>> {
        Some(self)
    }
    #[inline(always)]
    fn support_monitor_cmd(
        &mut self,
    ) -> Option<gdbstub::target::ext::monitor_cmd::MonitorCmdOps<'_, Self>> {
        Some(self)
    }
}
impl<S: SessionOwner> SingleThreadBase for Guest<'_, S> {
    fn read_registers(&mut self, regs: &mut Regs) -> TargetResult<(), Self> {
        *regs = self.gdb_registers();
        Ok(())
    }
    fn write_registers(&mut self, regs: &Regs) -> TargetResult<(), Self> {
        let old = self.gdb_registers();
        if regs.core[10..] != old.core[10..] || regs.x87 != old.x87 {
            return Err(TargetError::NonFatal);
        }
        let general = regs.core[..8].try_into().unwrap();
        self.session
            .debug_write_core(general, regs.core[8], regs.core[9])
            .then_some(())
            .ok_or(TargetError::NonFatal)
    }
    fn read_addrs(&mut self, address: u32, bytes: &mut [u8]) -> TargetResult<usize, Self> {
        self.session
            .debug_read_memory(address, bytes)
            .then_some(bytes.len())
            .ok_or(TargetError::NonFatal)
    }
    fn write_addrs(&mut self, address: u32, bytes: &[u8]) -> TargetResult<(), Self> {
        self.session
            .debug_write_memory(address, bytes)
            .then_some(())
            .ok_or(TargetError::NonFatal)
    }
    #[inline(always)]
    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }
    #[inline(always)]
    fn support_resume(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::singlethread::SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}
impl<S: SessionOwner> SingleRegisterAccess<()> for Guest<'_, S> {
    fn read_register(
        &mut self,
        _: (),
        reg: Register,
        bytes: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let regs = self.gdb_registers();
        if reg.0 < 16 {
            bytes[..4].copy_from_slice(&regs.core[reg.0].to_le_bytes());
            return Ok(4);
        }
        let Some(x87) = regs.x87 else {
            return Ok(0);
        };
        let (at, len) = if reg.0 < 24 {
            ((reg.0 - 16) * 10, 10)
        } else {
            (80 + (reg.0 - 24) * 4, 4)
        };
        bytes[..len].copy_from_slice(&x87[at..at + len]);
        Ok(len)
    }
    fn write_register(&mut self, _: (), reg: Register, bytes: &[u8]) -> TargetResult<(), Self> {
        if reg.0 >= 16 {
            return Err(TargetError::NonFatal);
        }
        let mut regs = self.gdb_registers();
        regs.core[reg.0] = u32::from_le_bytes(bytes.try_into().map_err(|_| TargetError::NonFatal)?);
        self.write_registers(&regs)
    }
}
impl<S: SessionOwner> SingleThreadResume for Guest<'_, S> {
    fn resume(&mut self, signal: Option<Signal>) -> Result<(), HostError> {
        Guest::resume(self, false, signal)
    }
    #[inline(always)]
    fn support_single_step(
        &mut self,
    ) -> Option<gdbstub::target::ext::base::singlethread::SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}
impl<S: SessionOwner> SingleThreadSingleStep for Guest<'_, S> {
    fn step(&mut self, signal: Option<Signal>) -> Result<(), HostError> {
        Guest::resume(self, true, signal)
    }
}
impl<S: SessionOwner> Breakpoints for Guest<'_, S> {
    #[inline(always)]
    fn support_sw_breakpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::SwBreakpointOps<'_, Self>> {
        Some(self)
    }
}
impl<S: SessionOwner> SwBreakpoint for Guest<'_, S> {
    fn add_sw_breakpoint(&mut self, address: u32, kind: usize) -> TargetResult<bool, Self> {
        if kind != 1 {
            return Ok(false);
        }
        if self.breakpoints.contains(&Some(address)) {
            return Ok(true);
        }
        let Some(slot) = self.breakpoints.iter_mut().find(|slot| slot.is_none()) else {
            return Ok(false);
        };
        *slot = Some(address);
        Ok(true)
    }
    fn remove_sw_breakpoint(&mut self, address: u32, _: usize) -> TargetResult<bool, Self> {
        if let Some(slot) = self
            .breakpoints
            .iter_mut()
            .find(|slot| **slot == Some(address))
        {
            *slot = None;
        }
        Ok(true)
    }
}
impl<S: SessionOwner> MonitorCmd for Guest<'_, S> {
    fn handle_monitor_cmd(
        &mut self,
        command: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), HostError> {
        let command = std::str::from_utf8(command).unwrap_or("");
        let parts: Vec<_> = command.split_whitespace().collect();
        match parts.as_slice() {
            ["state"] => {
                let r = self.session.registers();
                gdbstub::outputln!(
                    out,
                    "steps={} ns={} status={:?} CS:EIP={:04x}:{:08x} linear_pc={:08x} code={} A20={} flags={:08x}",
                    self.session.steps(),
                    self.session.emulated_nanos(),
                    self.session.status(),
                    r.cs,
                    r.ip,
                    self.session.debug_pc(),
                    if r.code32() { 32 } else { 16 },
                    self.session.a20_enabled(),
                    self.session.eflags()
                );
                for seg in SegmentRegister::ALL {
                    let c = r.segment_cache(seg);
                    gdbstub::outputln!(
                        out,
                        "{seg:?}: selector={:04x} base={:08x} limit={:08x} access={:02x} default32={}",
                        c.selector,
                        c.base,
                        c.limit,
                        c.access,
                        c.default32
                    );
                }
                gdbstub::outputln!(out, "fault={:?}", self.session.fault());
            }
            ["stop-at", at] => match at.parse::<u64>() {
                Ok(at) if at > self.session.steps() && at <= self.limit => {
                    self.stop_at = Some(at);
                    gdbstub::outputln!(out, "Will stop at step {at}; use continue.");
                }
                _ => gdbstub::outputln!(out, "Expected a future decimal step within --steps."),
            },
            ["key", scan, ascii] => match (
                u8::from_str_radix(scan.trim_start_matches("0x"), 16),
                u8::from_str_radix(ascii.trim_start_matches("0x"), 16),
            ) {
                (Ok(scan), Ok(ascii)) => {
                    self.session.push_key(scan, ascii)?;
                    gdbstub::outputln!(out, "Queued BIOS key {scan:02x}:{ascii:02x}.");
                }
                _ => gdbstub::outputln!(out, "key expects hexadecimal scan and ASCII bytes."),
            },
            ["hotspots", "on"] => {
                if self.hot.is_empty() {
                    self.hot = vec![Hot::default(); HOT_SLOTS].into_boxed_slice();
                }
                self.hot.fill(Hot::default());
                self.dropped = 0;
                self.sampling = true;
            }
            ["hotspots", "off"] => self.sampling = false,
            ["hotspots"] => {
                let mut hot: Vec<_> = self
                    .hot
                    .iter()
                    .filter(|slot| slot.count != 0)
                    .copied()
                    .collect();
                hot.sort_unstable_by_key(|slot| std::cmp::Reverse(slot.count));
                for slot in hot.iter().take(20) {
                    gdbstub::outputln!(
                        out,
                        "{:08x} CS={:04x} code={} hits={}",
                        slot.pc,
                        slot.cs,
                        if slot.wide { 32 } else { 16 },
                        slot.count
                    );
                }
                gdbstub::outputln!(
                    out,
                    "dropped={} (instruction-boundary PC samples, including IRQ boundaries)",
                    self.dropped
                );
            }
            _ => gdbstub::outputln!(
                out,
                "Commands: state; stop-at DECIMAL_STEP; key HEX_SCAN HEX_ASCII; hotspots on|off; hotspots"
            ),
        }
        Ok(())
    }
}
struct EventLoop<'a>(PhantomData<&'a ()>);
impl<'a> BlockingEventLoop for EventLoop<'a> {
    type Target = Guest<'a>;
    type Connection = ConnectionStream;
    type StopReason = Stop;
    fn wait_for_stop_reason(
        target: &mut Guest<'a>,
        connection: &mut ConnectionStream,
    ) -> Result<Event<Stop>, WaitForStopReasonError<HostError, io::Error>> {
        loop {
            if connection
                .peek()
                .map_err(WaitForStopReasonError::Connection)?
                .is_some()
            {
                let byte = connection
                    .read()
                    .map_err(WaitForStopReasonError::Connection)?;
                return Ok(Event::IncomingData(byte));
            }
            // Bound Ctrl-C latency without a socket syscall per guest step.
            for _ in 0..256 {
                if let Some(stop) = target.run_step().map_err(WaitForStopReasonError::Target)? {
                    return Ok(Event::TargetStopped(stop));
                }
            }
        }
    }
    fn on_interrupt(target: &mut Guest<'a>) -> Result<Option<Stop>, HostError> {
        Ok(Some(target.stop(Stop::Signal(Signal::SIGINT))))
    }
}

/// Returns false on GDB kill; detach resumes the ordinary headless runner.
pub fn serve(
    session: &mut Session,
    address: SocketAddr,
    limit: u64,
    tick: Tick<'_>,
) -> Result<bool, HostError> {
    let listener = TcpListener::bind(address)?;
    session.debug_suspend();
    eprintln!(
        "GDB listening on {}; stopped at step {}. target remote {}",
        listener.local_addr()?,
        session.steps(),
        listener.local_addr()?
    );
    let (stream, _) = listener.accept()?;
    session.set_dynamic_core(false);
    session.set_idle_loops(false);
    session.set_vga_wait_acceleration(false);
    serve_stream(session, stream, limit, tick)
}

fn serve_stream(
    session: &mut Session,
    stream: TcpStream,
    limit: u64,
    tick: Tick<'_>,
) -> Result<bool, HostError> {
    let mut target = Guest::new(session, Some(tick), limit);
    let result = GdbStub::new(ConnectionStream(stream)).run_blocking::<EventLoop<'_>>(&mut target);
    target.session.debug_suspend();
    let reason = result.map_err(|e| format!("GDB remote session: {e:?}"))?;
    eprintln!("GDB disconnected: {reason:?}");
    match reason {
        DisconnectReason::Kill => Ok(false),
        _ => {
            target.session.start();
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdbstub::arch::Registers;

    #[test]
    fn breakpoint_resume_single_step_and_budget_preserve_instruction_boundaries() {
        let mut session = Session::new(&[0x40, 0xEB, 0xFD]).unwrap();
        session.set_idle_loops(false);
        session.start();
        session.debug_suspend();
        let mut tick = |_: &mut Session| Ok(());
        let mut target = Guest::new(&mut session, Some(&mut tick), 3);
        assert!(matches!(target.add_sw_breakpoint(0x10100, 1), Ok(true)));
        Guest::resume(&mut target, false, None).unwrap();
        assert!(matches!(
            target.run_step().unwrap(),
            Some(Stop::SwBreak(()))
        ));
        assert_eq!(target.session.steps(), 0);
        Guest::resume(&mut target, false, None).unwrap();
        assert!(target.run_step().unwrap().is_none());
        assert!(target.run_step().unwrap().is_none());
        assert!(matches!(
            target.run_step().unwrap(),
            Some(Stop::SwBreak(()))
        ));
        assert_eq!(target.session.steps(), 2);
        Guest::resume(&mut target, true, None).unwrap();
        assert!(matches!(target.run_step().unwrap(), Some(Stop::DoneStep)));
        assert_eq!(target.session.steps(), 3);
        Guest::resume(&mut target, false, Some(Signal::SIGXCPU)).unwrap();
        assert!(matches!(
            target.run_step().unwrap(),
            Some(Stop::Signal(Signal::SIGXCPU))
        ));
        assert_eq!(target.session.steps(), 3);
        Guest::resume(&mut target, false, None).unwrap();
        assert!(matches!(
            target.run_step().unwrap(),
            Some(Stop::Signal(Signal::SIGXCPU))
        ));
        assert_eq!(target.session.steps(), 3);
    }

    #[test]
    fn register_and_memory_edits_are_atomic_and_x87_is_readonly() {
        let mut session = Session::new(&[0xD9, 0xE8, 0xEB, 0xFC]).unwrap();
        session.start();
        session.advance(1);
        session.debug_suspend();
        let mut tick = |_: &mut Session| Ok(());
        let mut target = Guest::new(&mut session, Some(&mut tick), 100);
        let before = target.gdb_registers();
        assert!(before.x87.is_some());
        let mut serialized = Vec::new();
        before.gdb_serialize(|byte| serialized.push(byte.unwrap()));
        let mut decoded = Regs::default();
        decoded.gdb_deserialize(&serialized).unwrap();
        assert_eq!(before, decoded);
        assert!(
            target
                .write_register((), Register(0), &42u32.to_le_bytes())
                .is_ok()
        );
        assert_eq!(target.session.registers().get32(doslet_x86::Gpr32::Eax), 42);
        let old = target.gdb_registers();
        let mut invalid = old.clone();
        invalid.core[10] ^= 8;
        invalid.core[0] = 123;
        assert!(target.write_registers(&invalid).is_err());
        assert_eq!(target.gdb_registers(), old);
        assert!(target.write_register((), Register(16), &[0; 10]).is_err());
        assert!(
            target
                .write_register((), Register(8), &u32::MAX.to_le_bytes())
                .is_err()
        );
        assert_eq!(target.gdb_registers(), old);
        assert!(target.write_addrs(0x9FFFF, &[1, 2]).is_err());
        assert!(target.write_addrs(0x10100, &[0x90]).is_ok());
        let mut bytes = [0; 1];
        assert!(target.read_addrs(0x10100, &mut bytes).is_ok());
        assert_eq!(bytes, [0x90]);
    }

    #[test]
    #[ignore = "requires installed GNU GDB and a localhost TCP socket"]
    fn local_gdb_client_exercises_registers_ram_breakpoints_step_and_detach() {
        use std::{
            process::Command,
            time::{Duration, Instant},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let began = Instant::now();
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            && began.elapsed() < Duration::from_secs(10) =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("GDB did not connect: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut session = Session::new(&[0x40, 0xEB, 0xFD]).unwrap();
            session.set_idle_loops(false);
            session.set_dynamic_core(false);
            session.set_vga_wait_acceleration(false);
            let mut tick = |_: &mut Session| Ok(());
            assert!(serve_stream(&mut session, stream, 100, &mut tick).unwrap());
            let mut bytes = [0; 3];
            assert!(session.debug_read_memory(0x10100, &mut bytes));
            (
                session.steps(),
                session.registers().get32(doslet_x86::Gpr32::Eax),
                bytes,
            )
        });
        let mut gdb = Command::new("gdb");
        gdb.args([
            "-nx",
            "-nh",
            "--batch",
            "-ex",
            "set pagination off",
            "-ex",
            "set confirm off",
            "-ex",
            &format!("target remote {address}"),
        ]);
        for command in [
            "monitor state",
            "set $eax = 0x1234",
            "stepi",
            "set {unsigned char}0x10100 = 0x48",
            "set $pc = 0x10100",
            "break *0x10101",
            "continue",
            "x/3bx 0x10100",
            "delete breakpoints",
            "x/3i 0x10100",
            "monitor hotspots on",
            "monitor stop-at 12",
            "continue",
            "monitor hotspots",
            "monitor state",
            "printf \"GDB_PROOF eax=%x pc=%x\\n\", $eax, $pc",
            "detach",
        ] {
            gdb.args(["-ex", command]);
        }
        let output = gdb.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("{stdout}\n{stderr}");
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(!stderr.contains("Architecture rejected"), "{stderr}");
        assert!(stderr.contains("steps=12"), "{stderr}");
        assert!(stdout.contains("GDB_PROOF"), "{stdout}");
        let (steps, eax, bytes) = server.join().unwrap();
        assert_eq!(steps, 12);
        assert_eq!(eax, 0x122F);
        assert_eq!(bytes, [0x48, 0xEB, 0xFD]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        drop(client);
        assert_eq!(
            ConnectionStream(peer).peek().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
