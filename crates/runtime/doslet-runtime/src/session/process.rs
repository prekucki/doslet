//! DOS processes: EXEC (4B00h) runs a child while its parent waits inside
//! that INT 21h, and the child's termination returns there.
use super::*;
use doslet_dos::{load_com_at, tables};
use doslet_vfs::{HANDLE_COUNT, MAX_PROCESS_DEPTH};
use doslet_x86::{CpuContext, SegmentRegister as Seg};

/// What a parent resumes with when its child terminates.
#[derive(Clone)]
pub(super) struct Parent {
    pub(super) psp: u16,
    pub(super) dta: (u16, u16),
    /// The parent at its EXEC call, inside the INT 21h handler.
    pub(super) context: CpuContext,
}

pub(super) fn no_parents() -> [Option<Parent>; MAX_PROCESS_DEPTH] {
    core::array::from_fn(|_| None)
}

/// Variables end with an empty string; DOS limits them to 32 KiB.
const ENVIRONMENT_BYTES: u32 = 0x8000;

impl Session {
    fn far_pointer(&mut self, at: u32) -> Result<u32, BusError> {
        let offset = self.machine.bus.read_u16(at)?;
        let segment = self.machine.bus.read_u16(at + 2)?;
        Ok(linear_address(segment, offset))
    }

    /// DOS EXEC (4B00h): load a COM or MZ program from C: into the largest
    /// free block, with a copy of the environment, and start it while the
    /// caller waits in this INT 21h. An LE/LX program runs its DOS stub, as at
    /// launch. The inner result is a DOS error code.
    pub(super) fn exec(
        &mut self,
        path: &[u8],
        parameters: u32,
    ) -> Result<Result<(), u16>, RuntimeError> {
        let depth = self.handles.depth();
        if depth == MAX_PROCESS_DEPTH || self.machine.cpu.protected_mode() {
            return Ok(Err(8));
        }
        let mut full = [0; 128];
        full[..3].copy_from_slice(b"C:\\");
        let name = match self.drive.canonical(path, &mut full[3..127]) {
            Ok(length) => 3 + length,
            Err(code) => return Ok(Err(code)),
        };
        if let Err(code) = self.drive.file(path) {
            return Ok(Err(code));
        }
        let mut source = self.machine.bus.read_u16(parameters)?;
        if source == 0 {
            source = self.machine.bus.read_u16(u32::from(self.psp) * 16 + 0x2C)?;
        }
        let source = u32::from(source) * 16;
        let mut variables = 0;
        if self.machine.bus.read_u8(source)? == 0 {
            variables = 1;
        } else {
            while self.machine.bus.read_u16(source + variables)? != 0 {
                variables += 1;
                if variables + 2 > ENVIRONMENT_BYTES {
                    return Ok(Err(10));
                }
            }
            variables += 2;
        }
        let size = variables + 2 + name as u32 + 1;
        let environment = match self.memory.allocate(size.div_ceil(16) as u16, self.psp) {
            Ok(segment) => segment,
            Err(failure) => return Ok(Err(failure.code)),
        };
        let target = u32::from(environment) * 16;
        for offset in 0..variables {
            let byte = self.machine.bus.read_u8(source + offset)?;
            self.machine.bus.write_u8(target + offset, byte)?;
        }
        // DOS 3+ appends a count of one and the program's full path.
        self.machine.bus.write_u16(target + variables, 1)?;
        for (offset, &byte) in full[..name].iter().chain(&[0]).enumerate() {
            self.machine
                .bus
                .write_u8(target + variables + 2 + offset as u32, byte)?;
        }
        let available = self.memory.largest_free();
        let child = match self.memory.allocate(available, self.psp) {
            Ok(segment) if available > 0x10 => segment,
            result => {
                if let Ok(segment) = result {
                    let _ = self.memory.free(segment);
                }
                let _ = self.memory.free(environment);
                self.memory.write_mcbs(&mut self.machine.bus)?;
                return Ok(Err(8));
            }
        };
        let memory = ProcessMemory {
            psp_segment: child,
            end_segment: child + available,
        };
        let program = self.drive.file(path).expect("checked above");
        let loaded = if program.starts_with(b"MZ") || program.starts_with(b"ZM") {
            load_mz_stub(&mut self.machine.bus, program, memory).map_err(|error| match error {
                MzError::Bus(error) => Err(error),
                MzError::InsufficientMemory => Ok(8),
                _ => Ok(11),
            })
        } else {
            load_com_at(&mut self.machine.bus, program, memory).map_err(|error| match error {
                ComError::Bus(error) => Err(error),
                ComError::TooLarge => Ok(8),
                ComError::Empty => Ok(11),
            })
        };
        let registers = match loaded {
            Ok(registers) => registers,
            Err(error) => {
                let _ = self.memory.free(child);
                let _ = self.memory.free(environment);
                self.memory.write_mcbs(&mut self.machine.bus)?;
                return Ok(Err(error?));
            }
        };
        let base = u32::from(child) * 16;
        let end = self.machine.bus.read_u16(base + 2)?;
        let _ = self.memory.resize(child, end - child);
        self.memory.set_owner(environment, child).ok();
        self.memory.set_owner(child, child).ok();
        self.memory.write_mcbs(&mut self.machine.bus)?;
        self.machine.bus.write_u16(base + 0x16, self.psp)?;
        self.machine.bus.write_u16(base + 0x2C, environment)?;
        // The child's PSP saves the vectors its termination restores.
        for (vector, offset) in [(0x22u32, 0x0A), (0x23, 0x0E), (0x24, 0x12)] {
            let value = self.machine.bus.read_u32(vector * 4)?;
            self.machine.bus.write_u32(base + offset, value)?;
        }
        let tail = self.far_pointer(parameters + 2)?;
        for offset in 0..128 {
            let byte = self.machine.bus.read_u8(tail + offset)?;
            self.machine.bus.write_u8(base + 0x80 + offset, byte)?;
        }
        for (pointer, at) in [(parameters + 6, 0x5C), (parameters + 10, 0x6C)] {
            let fcb = self.far_pointer(pointer)?;
            for offset in 0..16 {
                let byte = self.machine.bus.read_u8(fcb + offset)?;
                self.machine.bus.write_u8(base + at + offset, byte)?;
            }
        }
        tables::install_child(&mut self.machine.bus, child)?;
        self.handles.begin_child().expect("depth checked above");
        self.parents[depth] = Some(Parent {
            psp: self.psp,
            dta: self.dta,
            context: self.machine.cpu.context(),
        });
        self.psp = child;
        self.dta = (child, 0x80);
        for handle in 5..5 + HANDLE_COUNT as u16 {
            self.sync_handle(handle, None)?;
        }
        let cpu = &mut self.machine.cpu;
        for register in [
            Gpr16::Ax,
            Gpr16::Bx,
            Gpr16::Cx,
            Gpr16::Dx,
            Gpr16::Si,
            Gpr16::Di,
            Gpr16::Bp,
        ] {
            cpu.registers.set(register, 0);
        }
        for (segment, selector) in [
            (Seg::Cs, registers.cs),
            (Seg::Ds, registers.ds),
            (Seg::Es, registers.es),
            (Seg::Ss, registers.ss),
        ] {
            cpu.load_segment(&mut self.machine.bus, segment, selector)?;
        }
        cpu.registers.ip = registers.ip;
        cpu.registers.set(Gpr16::Sp, registers.get(Gpr16::Sp));
        cpu.flags = 0x202;
        Ok(Ok(()))
    }

    /// The child terminated with `code`. Its files close unless its parents
    /// still use them, its memory is freed, the vectors saved in its PSP
    /// return, a DPMI client it ran leaves the host, and its parent resumes
    /// after the EXEC call with the carry flag clear.
    pub(super) fn end_child(&mut self, code: u8) -> Result<(), RuntimeError> {
        let depth = self.handles.depth();
        let parent = self.parents[depth - 1]
            .take()
            .expect("a running child has a parent");
        let child = self.psp;
        if self.dpmi_client_psp == child {
            self.dpmi_reset();
        }
        let base = u32::from(child) * 16;
        for (vector, offset) in [(0x22u32, 0x0A), (0x23, 0x0E), (0x24, 0x12)] {
            let value = self.machine.bus.read_u32(base + offset)?;
            self.machine.bus.write_u32(vector * 4, value)?;
        }
        let mut closed = [None; HANDLE_COUNT];
        let mut count = 0;
        self.handles.end_child(|file| {
            closed[count] = Some(file);
            count += 1;
        });
        for file in closed[..count].iter().flatten() {
            if !self.handles.any_open(file.mount, file.node) {
                let backend = file.node.0 as u16;
                if file.mount.0 == 1 {
                    if let Some(cd) = &mut self.cdrom {
                        let _ = cd.close(backend);
                    }
                } else {
                    let _ = self.drive.close(backend);
                }
            }
        }
        self.memory.free_owned(child);
        self.memory.write_mcbs(&mut self.machine.bus)?;
        self.psp = parent.psp;
        self.dta = parent.dta;
        self.machine.cpu.restore_context(&parent.context);
        self.return_code = u16::from(code);
        self.sync_descriptions()?;
        self.set_carry(false)?;
        self.begin_commit()
    }
}
