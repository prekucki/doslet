//! VGA register banks and selected 256-color memory/scanout controls.

pub(crate) struct Registers {
    pub misc: u8,
    sequencer_index: u8,
    sequencer: [u8; 5],
    graphics_index: u8,
    graphics: [u8; 9],
    crtc_index: u8,
    crtc: [u8; 25],
    attribute: [u8; 32],
    attribute_index: u8,
    attribute_data: bool,
}
impl Default for Registers {
    fn default() -> Self {
        Self {
            misc: 0x67,
            sequencer_index: 0,
            sequencer: [3, 0, 3, 0, 2],
            graphics_index: 0,
            graphics: [0, 0, 0, 0, 0, 0x10, 0x0E, 0, 0xFF],
            crtc_index: 0,
            crtc: [
                0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0, 0x4F, 0x0D, 0x0E, 0, 0, 0, 0,
                0x9C, 0x8E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF,
            ],
            attribute: [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0x0C, 0, 15, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
            ],
            attribute_index: 0x20,
            attribute_data: false,
        }
    }
}
impl Registers {
    pub fn chain4(&self) -> bool {
        self.sequencer[4] & 8 != 0
    }
    pub fn plane_mask(&self) -> u8 {
        self.sequencer[2]
    }
    pub fn read_plane(&self) -> usize {
        usize::from(self.graphics[4] & 3)
    }
    pub fn write_mode(&self) -> u8 {
        self.graphics[5] & 3
    }
    pub fn graphics_pipeline(&self) -> (u8, u8, u8, u8, u8) {
        (
            self.graphics[0],
            self.graphics[1],
            self.graphics[3],
            self.graphics[8],
            self.graphics[2],
        )
    }
    pub fn read_compare(&self) -> Option<u8> {
        (self.graphics[5] & 8 != 0).then_some(self.graphics[7])
    }
    pub fn layout_256(&self) -> (usize, usize, usize, usize) {
        let width = (usize::from(self.crtc[1]) + 1) * 4;
        let lines = usize::from(self.crtc[0x12])
            | (usize::from(self.crtc[7] & 2) << 7)
            | (usize::from(self.crtc[7] & 0x40) << 3);
        let repeat =
            (usize::from(self.crtc[9] & 31) + 1) * if self.crtc[9] & 0x80 != 0 { 2 } else { 1 };
        let height = (lines + 1).div_ceil(repeat);
        let start = usize::from(u16::from_be_bytes([self.crtc[12], self.crtc[13]]));
        let pitch = usize::from(self.crtc[0x13]) * 2;
        (width, height, start, pitch)
    }
    pub fn memory_config(&self) -> (u8, u8, u8, u16, u8, u8) {
        (
            self.sequencer[2],
            self.sequencer[4],
            self.graphics[5],
            u16::from_be_bytes([self.crtc[12], self.crtc[13]]),
            self.crtc[20],
            self.crtc[23],
        )
    }
    pub fn reset_attribute_latch(&mut self) {
        self.attribute_data = false;
    }
    pub fn mode_13h(&mut self) {
        self.misc = 0x63;
        self.attribute[0x10] = 0x41;
        self.sequencer = [3, 1, 15, 0, 14];
        self.graphics = [0, 0, 0, 0, 0, 0x40, 5, 15, 0xFF];
        self.crtc = [
            0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0, 0x41, 0, 0, 0, 0, 0, 0, 0x9C, 0x8E,
            0x8F, 0x28, 0x40, 0x96, 0xB9, 0xA3, 0xFF,
        ];
    }
    fn active_crtc(&self, port: u16) -> bool {
        port & 0xFFF0 == if self.misc & 1 != 0 { 0x3D0 } else { 0x3B0 }
    }
    pub fn read(&self, port: u16) -> Option<u8> {
        match port {
            0x3C0 => Some(self.attribute_index),
            0x3C1 => Some(self.attribute[usize::from(self.attribute_index & 31)]),
            0x3CC => Some(self.misc),
            0x3C4 => Some(self.sequencer_index),
            0x3CE => Some(self.graphics_index),
            0x3CF => Some(
                self.graphics
                    .get(usize::from(self.graphics_index))
                    .copied()
                    .unwrap_or(0xFF),
            ),
            0x3C5 => Some(
                self.sequencer
                    .get(usize::from(self.sequencer_index))
                    .copied()
                    .unwrap_or(0xFF),
            ),
            0x3B4 | 0x3D4 if self.active_crtc(port) => Some(self.crtc_index),
            0x3B5 | 0x3D5 if self.active_crtc(port) => Some(
                self.crtc
                    .get(usize::from(self.crtc_index))
                    .copied()
                    .unwrap_or(0xFF),
            ),
            _ => None,
        }
    }
    pub fn write(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x3C0 => {
                if self.attribute_data {
                    self.attribute[usize::from(self.attribute_index & 31)] = value;
                } else {
                    self.attribute_index = value & 0x3F;
                }
                self.attribute_data = !self.attribute_data;
            }
            0x3C2 => self.misc = value,
            0x3C4 => self.sequencer_index = value,
            0x3CE => self.graphics_index = value,
            0x3CF => {
                if let Some(register) = self.graphics.get_mut(usize::from(self.graphics_index)) {
                    let mask =
                        [15, 15, 15, 0x1F, 3, 0x7B, 15, 15, 0xFF][usize::from(self.graphics_index)];
                    *register = value & mask;
                }
            }
            0x3C5 => {
                if let Some(register) = self.sequencer.get_mut(usize::from(self.sequencer_index)) {
                    let mask = [3, 0x3D, 15, 0x3F, 14][usize::from(self.sequencer_index)];
                    *register = value & mask;
                }
            }
            0x3B4 | 0x3D4 if self.active_crtc(port) => self.crtc_index = value,
            0x3B5 | 0x3D5 if self.active_crtc(port) => {
                let index = usize::from(self.crtc_index);
                if index < self.crtc.len() {
                    if self.crtc[0x11] & 0x80 == 0 || index > 7 {
                        self.crtc[index] = value;
                    } else if index == 7 {
                        self.crtc[index] = (self.crtc[index] & !0x10) | (value & 0x10);
                    }
                }
            }
            _ => return false,
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequencer_probes_do_not_invent_svga_extension_registers() {
        let mut registers = Registers::default();
        registers.write(0x3C4, 6);
        assert_eq!(registers.read(0x3C5), Some(0xFF));
        registers.write(0x3C5, 0x12);
        assert_eq!(registers.read(0x3C5), Some(0xFF));
        registers.write(0x3C4, 2);
        registers.write(0x3C5, 0xFF);
        assert_eq!(registers.read(0x3C5), Some(15));
    }
    #[test]
    fn crtc_decode_and_protection_follow_misc_and_vertical_retrace_bits() {
        let mut registers = Registers::default();
        registers.write(0x3D4, 0);
        registers.write(0x3D5, 0x22);
        assert_eq!(registers.read(0x3D5), Some(0x5F));
        registers.write(0x3D4, 0x11);
        registers.write(0x3D5, 0);
        registers.write(0x3D4, 0);
        registers.write(0x3D5, 0x22);
        assert_eq!(registers.read(0x3D5), Some(0x22));
        registers.write(0x3C2, 0x66);
        assert_eq!(registers.read(0x3D4), None);
        registers.write(0x3B4, 0);
        assert_eq!(registers.read(0x3B5), Some(0x22));
    }
}

#[cfg(feature = "save-state")]
#[path = "registers_saved.rs"]
mod saved_state;
