//! Nonblocking desktop transport. Polled only at host execution boundaries.
use super::*;
use gdbstub::stub::state_machine::GdbStubStateMachine;
use std::time::{Duration, Instant};

type Machine = GdbStubStateMachine<'static, DebugSession, ConnectionStream>;

/// Static dispatch keeps normal Session::advance free of debugger hooks.
pub trait Execution {
    const DEBUG: bool;
    fn advance(&mut self, session: &mut DebugSession, budget: u32) -> Result<u32, HostError>;
}
pub struct Normal;
impl Execution for Normal {
    const DEBUG: bool = false;
    #[inline]
    fn advance(&mut self, session: &mut DebugSession, budget: u32) -> Result<u32, HostError> {
        Ok(session.session.advance(budget))
    }
}

pub struct Debugger {
    listener: TcpListener,
    machine: Option<Machine>,
    settings: Option<doslet_runtime::session::debug::ExecutionSettings>,
    was_running: bool,
    revision: u64,
}
impl Debugger {
    pub fn listen(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        eprintln!(
            "GDB listening on {} (attach to stop guest)",
            listener.local_addr()?
        );
        Ok(Self {
            listener,
            machine: None,
            settings: None,
            was_running: false,
            revision: 0,
        })
    }
    pub fn address(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    pub fn connected(&self) -> bool {
        self.machine.is_some()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn disconnect(&mut self, target: &mut DebugSession, resume: bool) {
        self.machine = None;
        if self.settings.is_none() {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        target.session.debug_suspend();
        if let Some(settings) = self.settings.take() {
            target.session.debug_restore_accelerations(settings);
        }
        target.breakpoints.fill(None);
        target.breakpoint_stop = None;
        target.skip_breakpoint = false;
        target.stop_at = None;
        target.sampling = false;
        target.hot = Box::default();
        if resume {
            target.session.start();
        }
    }
    pub fn disable(&mut self, target: &mut DebugSession) {
        self.disconnect(target, self.was_running);
    }

    /// Process bounded available input. A client may leave a packet incomplete;
    /// no blocking read waits for its remaining bytes on the emulation owner.
    pub fn poll(&mut self, target: &mut DebugSession) -> Result<(), HostError> {
        let result = self.poll_inner(target);
        if result.is_err() {
            self.disconnect(target, false);
        }
        result
    }
    fn poll_inner(&mut self, target: &mut DebugSession) -> Result<(), HostError> {
        if self.machine.is_none() {
            match self.listener.accept() {
                Ok((stream, peer)) => {
                    stream.set_write_timeout(Some(Duration::from_millis(100)))?;
                    self.was_running = target.status() == SessionStatus::Running;
                    self.revision = self.revision.wrapping_add(1);
                    self.settings = Some(target.debug_disable_accelerations());
                    target.debug_suspend();
                    self.machine = Some(
                        GdbStub::new(ConnectionStream(stream))
                            .run_state_machine(target)
                            .map_err(|e| format!("GDB attach: {e:?}"))?,
                    );
                    eprintln!(
                        "GDB connected from {peer}; guest stopped at step {}",
                        target.steps()
                    );
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
        let began = Instant::now();
        for _ in 0..4096 {
            let Some(machine) = self.machine.take() else {
                break;
            };
            let next = match machine {
                GdbStubStateMachine::Idle(mut state) => match state.borrow_conn().peek()? {
                    Some(byte) => {
                        self.revision = self.revision.wrapping_add(1);
                        state.borrow_conn().read()?;
                        state.incoming_data(target, byte)
                    }
                    None => {
                        self.machine = Some(state.into());
                        break;
                    }
                },
                GdbStubStateMachine::Running(mut state) => {
                    let stopped = match target.status() {
                        SessionStatus::Paused => Some(Stop::Signal(Signal::SIGINT)),
                        SessionStatus::Faulted => Some(Stop::Signal(Signal::SIGILL)),
                        SessionStatus::Halted => Some(Stop::Signal(Signal::SIGSTOP)),
                        SessionStatus::Exited(code) => Some(Stop::Exited(code)),
                        _ => None,
                    };
                    if let Some(reason) = stopped {
                        state.report_stop(target, reason)
                    } else {
                        match state.borrow_conn().peek()? {
                            Some(byte) => {
                                self.revision = self.revision.wrapping_add(1);
                                state.borrow_conn().read()?;
                                state.incoming_data(target, byte)
                            }
                            None => {
                                self.machine = Some(state.into());
                                break;
                            }
                        }
                    }
                }
                GdbStubStateMachine::CtrlCInterrupt(state) => {
                    let reason = target.stop(Stop::Signal(Signal::SIGINT));
                    state.interrupt_handled(target, Some(reason))
                }
                GdbStubStateMachine::Disconnected(state) => {
                    let reason = state.get_reason();
                    eprintln!("GDB disconnected: {reason:?}");
                    let resume = matches!(reason, DisconnectReason::Disconnect) && self.was_running;
                    self.disconnect(target, resume);
                    break;
                }
            };
            self.machine = Some(next.map_err(|e| format!("GDB protocol: {e:?}"))?);
            if began.elapsed() >= Duration::from_millis(2) {
                break;
            }
        }
        Ok(())
    }
}
impl Execution for Debugger {
    const DEBUG: bool = true;
    fn advance(&mut self, target: &mut DebugSession, budget: u32) -> Result<u32, HostError> {
        self.poll(target)?;
        if !matches!(self.machine, Some(GdbStubStateMachine::Running(_))) {
            return Ok(0);
        }
        let before = target.steps();
        for _ in 0..budget.min(256) {
            let last = target.steps();
            if let Some(reason) = target.run_step()? {
                if let Some(GdbStubStateMachine::Running(state)) = self.machine.take() {
                    self.machine = Some(
                        state
                            .report_stop(target, reason)
                            .map_err(|e| format!("GDB stop: {e:?}"))?,
                    );
                }
                break;
            }
            if target.steps() == last || target.timeslice_yielded() {
                break;
            }
        }
        Ok((target.steps() - before) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_execution_allocates_no_debug_storage_and_matches_reference() {
        let code = [0x40, 0xA3, 0, 2, 0xEB, 0xFA];
        let mut source = Some(Session::new(&code).unwrap());
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            let mut target: DebugSession = source.take().unwrap().into();
            target.set_dynamic_core(false);
            target.start();
            assert_eq!(Normal.advance(&mut target, 10000).unwrap(), 10000);
            assert!(target.hot.is_empty());
            result = Some(target);
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(allocation.bytes_total, 0);
        let target = result.unwrap();
        let mut reference = Session::new(&code).unwrap();
        reference.set_decode_cache(false);
        reference.start();
        assert_eq!(reference.advance(10000), 10000);
        assert_eq!(target.steps(), reference.steps());
        assert_eq!(target.eflags(), reference.eflags());
        assert_eq!(target.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(target.registers().ip, reference.registers().ip);
        for r in GENERAL {
            assert_eq!(target.registers().get32(r), reference.registers().get32(r));
        }
        assert_eq!(target.ram_view(0..0xA0000), reference.ram_view(0..0xA0000));
    }

    #[test]
    fn listener_without_client_preserves_execution_and_an_incomplete_packet_yields() {
        let mut target: DebugSession = Session::new(&[0x40, 0xEB, 0xFD]).unwrap().into();
        target.set_dynamic_core(false);
        target.start();
        let mut server = Debugger::listen("127.0.0.1:0".parse().unwrap()).unwrap();
        server.poll(&mut target).unwrap();
        assert_eq!(target.status(), SessionStatus::Running);
        assert_eq!(target.steps(), 0);
        server.disable(&mut target);
        assert_eq!(target.status(), SessionStatus::Running);
        let mut client = TcpStream::connect(server.address().unwrap()).unwrap();
        Write::write_all(&mut client, b"$qSupported").unwrap();
        server.poll(&mut target).unwrap();
        assert!(server.connected());
        assert_eq!(target.status(), SessionStatus::Paused);
        let before = target.emulated_nanos();
        for _ in 0..4 {
            server.poll(&mut target).unwrap();
        }
        assert_eq!(target.emulated_nanos(), before);
        server.disable(&mut target);
        assert_eq!(target.status(), SessionStatus::Running);
        assert!(!target.dynamic_core_enabled()); // Preserve the user's interpreter selection.
        drop(client);
        // Listening remains usable for a second connection after explicit detach.
        let _client = TcpStream::connect(server.address().unwrap()).unwrap();
        server.poll(&mut target).unwrap();
        assert!(server.connected());
        assert_eq!(target.status(), SessionStatus::Paused);
        server.disable(&mut target);
    }

    #[test]
    fn remote_interrupt_stops_at_a_boundary_and_continue_resumes() {
        let mut target: DebugSession = Session::new(&[0x40, 0xEB, 0xFD]).unwrap().into();
        target.start();
        let mut server = Debugger::listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut client = TcpStream::connect(server.address().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        // Resume packet checksum is the ASCII value of c.
        Write::write_all(&mut client, b"$c#63").unwrap();
        server.poll(&mut target).unwrap();
        assert_eq!(server.advance(&mut target, 256).unwrap(), 256);
        let before = (target.steps(), target.emulated_nanos());
        Write::write_all(&mut client, &[3]).unwrap();
        server.poll(&mut target).unwrap();
        assert_eq!(target.status(), SessionStatus::Paused);
        assert_eq!((target.steps(), target.emulated_nanos()), before);
        let mut response = Vec::new();
        while !response.contains(&b'#') {
            let mut bytes = [0; 256];
            let count = Read::read(&mut client, &mut bytes).unwrap();
            assert_ne!(count, 0);
            response.extend_from_slice(&bytes[..count]);
        }
        assert!(
            response.windows(3).any(|s| s == b"S02" || s == b"T02"),
            "{response:?}"
        );
        Write::write_all(&mut client, b"+$c#63").unwrap();
        server.poll(&mut target).unwrap();
        assert_eq!(server.advance(&mut target, 128).unwrap(), 128);
        assert_eq!(target.steps(), before.0 + 128);
        server.disable(&mut target);
    }
}
