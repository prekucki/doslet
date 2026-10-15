//! Read-only observers for the local HoMM build; addresses are version-specific.
use doslet_runtime::Session;
use doslet_x86::{Gpr32, x87_decode};
use std::{collections::BTreeMap, io::Write, path::PathBuf};

fn dword(session: &Session, address: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        session.ram_view(address..address + 4)?.try_into().ok()?,
    ))
}
#[derive(Default)]
pub struct Audit {
    floats: BTreeMap<u32, (u64, u8, u8)>,
    copies: u64,
    scanouts: u64,
    watches: Option<[u8; 8]>,
    frames: Option<PathBuf>,
}
impl Audit {
    pub fn with_frames(directory: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&directory)?;
        Ok(Self {
            frames: Some(directory),
            ..Self::default()
        })
    }
    pub fn observe(&mut self, session: &Session, out: &mut impl Write) -> std::io::Result<()> {
        let r = session.registers();
        if r.cs != 0x180 {
            return Ok(());
        }
        if r.ip == 0x22551A {
            let stack = r.get32(Gpr32::Esp) as usize;
            if let Some(address) = dword(session, stack + 4)
                && let Some(bytes) = session.ram_view(address as usize..address as usize + 32)
            {
                write!(
                    out,
                    "PACKET step={} frame={:?} prefix=",
                    session.steps(),
                    dword(session, 0x68EFC + 0x678)
                )?;
                for byte in bytes {
                    write!(out, "{byte:02x}")?;
                }
                writeln!(out)?;
                if let Some(directory) = &self.frames
                    && let Some(packet) =
                        session.ram_view(address as usize..address as usize + 1024)
                {
                    std::fs::write(
                        directory.join(format!("{}-packet.bin", session.steps())),
                        packet,
                    )?;
                }
            }
        }
        if r.ip == 0x226460
            && let Some(directory) = &self.frames
        {
            let pixels = session.ram_view(0x1A690..0x1A690 + 640 * 480).unwrap();
            std::fs::write(directory.join(format!("{}.bin", session.steps())), pixels)?;
            writeln!(out, "FRAME_DONE step={}", session.steps())?;
        }
        if let Some(code) = session.ram_view(r.ip as usize..r.ip as usize + 2)
            && (0xD8..=0xDF).contains(&code[0])
        {
            self.floats.entry(r.ip).or_insert((0, code[0], code[1])).0 += 1;
        }
        if matches!(r.ip, 0x212638 | 0x212778) {
            let stack = r.get32(Gpr32::Esp);
            let mut args = [0; 9];
            for (i, arg) in args.iter_mut().enumerate() {
                *arg = dword(session, stack as usize + i * 4).unwrap_or(0);
            }
            writeln!(
                out,
                "RECT step={} pc={:08x} return={:08x} args={:08x?}",
                session.steps(),
                r.ip,
                args[0],
                &args[1..]
            )?;
            if r.ip == 0x212778 {
                self.copies += 1;
            } else {
                self.scanouts += 1;
            }
        }
        let mut watches = [0; 8];
        for (value, (x, y)) in watches.iter_mut().zip([
            (300, 0),
            (300, 5),
            (300, 10),
            (300, 20),
            (300, 30),
            (300, 50),
            (300, 150),
            (300, 180),
        ]) {
            *value = session
                .ram_view(0x1A690 + y * 640 + x..0x1A690 + y * 640 + x + 1)
                .unwrap()[0];
        }
        if self.watches != Some(watches) {
            writeln!(
                out,
                "PIXEL step={} next={:08x} values={watches:?}",
                session.steps(),
                r.ip
            )?;
            self.watches = Some(watches);
        }
        Ok(())
    }
    pub fn finish(&self, out: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            out,
            "SUMMARY ram_copies={} scanouts={} x87_sites={}",
            self.copies,
            self.scanouts,
            self.floats.len()
        )?;
        for (ip, (count, opcode, modrm)) in &self.floats {
            writeln!(
                out,
                "X87 pc={ip:08x} count={count} instruction={:?}",
                x87_decode::decode(*opcode, *modrm)
            )?;
        }
        Ok(())
    }
}
