//! i386 core plus x87, without advertising unimplemented SSE registers.
use gdbstub::arch::{Arch, RegId, Registers};
use std::num::NonZeroUsize;

pub enum I386 {}
impl Arch for I386 {
    type Usize = u32;
    type Registers = Regs;
    type RegId = Register;
    type BreakpointKind = usize;
    fn target_description_xml() -> Option<&'static str> {
        Some(include_str!("i386.xml"))
    }
}
#[derive(Debug)]
pub struct Register(pub usize);
impl RegId for Register {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        (id < 32).then(|| {
            (
                Self(id),
                NonZeroUsize::new(if (16..24).contains(&id) { 10 } else { 4 }),
            )
        })
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Regs {
    pub core: [u32; 16],
    pub x87: Option<[u8; 112]>,
}
impl Registers for Regs {
    type ProgramCounter = u32;
    fn pc(&self) -> u32 {
        self.core[8]
    }
    fn gdb_serialize(&self, mut emit: impl FnMut(Option<u8>)) {
        for value in self.core {
            for byte in value.to_le_bytes() {
                emit(Some(byte));
            }
        }
        for i in 0..112 {
            emit(self.x87.as_ref().map(|bytes| bytes[i]));
        }
    }
    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() != 176 {
            return Err(());
        }
        for (reg, bytes) in self.core.iter_mut().zip(bytes[..64].as_chunks::<4>().0) {
            *reg = u32::from_le_bytes(*bytes);
        }
        self.x87 = Some(bytes[64..].try_into().map_err(|_| ())?);
        Ok(())
    }
}
