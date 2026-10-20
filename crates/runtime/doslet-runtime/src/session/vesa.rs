//! VBE 1.2 BIOS adapter for the machine's single banked indexed mode.
use super::{RuntimeError, Session};
use crate::bios;
use doslet_bus::MemoryBus;
use doslet_vga::{APERTURE_LEN, VRAM_LEN, VideoMode};
use doslet_x86::{Gpr16, linear_address};

impl Session {
    pub(super) fn vesa_service(&mut self, function: u8) -> Result<(), RuntimeError> {
        let r = &self.machine.cpu.registers;
        let (bx, cx, dx) = (r.get(Gpr16::Bx), r.get(Gpr16::Cx), r.get(Gpr16::Dx));
        let mut info = [0u8; 256];
        let success = match function {
            0 => {
                info[..4].copy_from_slice(b"VESA");
                word(&mut info, 4, 0x0102); // VBE 1.2, without LFB/PM interface.
                far_pointer(&mut info, 6, bios::VBE_OEM_OFFSET);
                far_pointer(&mut info, 14, bios::VBE_MODES_OFFSET);
                word(&mut info, 18, (VRAM_LEN / APERTURE_LEN) as u16);
                self.write_vbe_info(&info)?;
                true
            }
            1 if cx == 0x101 => {
                word(&mut info, 0, 0x1B); // Supported, full info, color, graphics.
                info[2] = 7; // Window A exists and is readable/writable.
                word(&mut info, 4, 64); // Bank granularity in KiB.
                word(&mut info, 6, 64); // Window size in KiB.
                word(&mut info, 8, 0xA000);
                far_pointer(&mut info, 12, bios::VBE_BANK_OFFSET);
                word(&mut info, 16, 640); // Bytes per scanline.
                word(&mut info, 18, 640);
                word(&mut info, 20, 480);
                info[22..30].copy_from_slice(&[8, 16, 1, 8, 1, 4, 64, 0]);
                info[30] = 1; // Reserved field required by VBE 1.x.
                self.write_vbe_info(&info)?;
                true
            }
            2 if bx & !0x8000 == 0x101 => {
                self.machine.bus.set_video_vbe_101h(bx & 0x8000 != 0);
                self.mouse_video_mode_changed();
                true
            }
            3 => {
                let mode = match self.machine.bus.vga.mode() {
                    VideoMode::Text3 => 3,
                    VideoMode::Mode13 => 0x13,
                    VideoMode::Vbe101 => 0x101,
                };
                self.machine.cpu.registers.set(Gpr16::Bx, mode);
                true
            }
            5 if bx == 0 => self.machine.bus.vga.set_bank(dx),
            5 if bx == 0x100 && self.machine.bus.vga.mode() == VideoMode::Vbe101 => {
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Dx, self.machine.bus.vga.bank());
                true
            }
            _ => false, // Optional functions/other modes explicitly fail through the VBE ABI.
        };
        self.machine
            .cpu
            .registers
            .set(Gpr16::Ax, if success { 0x004F } else { 0x014F });
        Ok(())
    }
    fn write_vbe_info(&mut self, info: &[u8; 256]) -> Result<(), RuntimeError> {
        let r = &self.machine.cpu.registers;
        for (offset, byte) in info.iter().enumerate() {
            self.machine.bus.write_u8(
                linear_address(r.es, r.get(Gpr16::Di).wrapping_add(offset as u16)),
                *byte,
            )?;
        }
        Ok(())
    }
}
fn word(info: &mut [u8], offset: usize, value: u16) {
    info[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn far_pointer(info: &mut [u8], offset: usize, target: u16) {
    word(info, offset, target);
    word(info, offset + 2, bios::SHIM_SEGMENT);
}
