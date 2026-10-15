//! 387 environment images, selected by operand size and processor mode.
use crate::{X87, finite::Finite};
#[derive(Clone, Copy, Debug)]
pub(super) struct Pointers {
    ip: u32,
    cs: u16,
    data: u32,
    ds: u16,
    opcode: u16,
}
impl Pointers {
    pub const fn new() -> Self {
        Self {
            ip: 0,
            cs: 0,
            data: 0,
            ds: 0,
            opcode: 0,
        }
    }
}

fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn dword(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn put_word(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put_dword(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
impl X87 {
    /// CPU-supplied pointer offsets/selectors in protected mode, linear pointers
    /// in real mode. Control/environment instructions do not overwrite these.
    pub fn record_instruction(&mut self, ip: u32, cs: u16, opcode: u16) {
        self.pointers.ip = ip;
        self.pointers.cs = cs;
        self.pointers.opcode = opcode & 0x7FF;
    }
    pub fn record_operand(&mut self, offset: u32, selector: u16) {
        self.pointers.data = offset;
        self.pointers.ds = selector;
    }
    /// Produce the 14/28-byte environment and optionally eight logical ST(i)
    /// registers. Saving is separate from FNSTENV masking and FNSAVE reset so
    /// the CPU adapter can complete the memory write first.
    pub fn save_environment(&self, wide: bool, protected: bool, registers: bool) -> [u8; 108] {
        let mut bytes = [0_u8; 108];
        let stride = if wide { 4 } else { 2 };
        put_word(&mut bytes, 0, self.control);
        put_word(&mut bytes, stride, self.status);
        put_word(&mut bytes, 2 * stride, self.tags);
        let p = self.pointers;
        if wide {
            // Reserved high words of CW/SW/TW/FDS are ones on the 387.
            for offset in [2, 6, 10, 26] {
                put_word(&mut bytes, offset, 0xFFFF);
            }
            if protected {
                put_dword(&mut bytes, 12, p.ip);
                put_word(&mut bytes, 16, p.cs);
                put_word(&mut bytes, 18, p.opcode);
                put_dword(&mut bytes, 20, p.data);
                put_word(&mut bytes, 24, p.ds);
            } else {
                put_dword(&mut bytes, 12, p.ip & 0xFFFF);
                put_dword(
                    &mut bytes,
                    16,
                    ((p.ip & 0xFFFF_0000) >> 4) | u32::from(p.opcode),
                );
                put_dword(&mut bytes, 20, p.data & 0xFFFF);
                put_dword(&mut bytes, 24, (p.data & 0xFFFF_0000) >> 4);
            }
        } else {
            put_word(&mut bytes, 6, p.ip as u16);
            put_word(&mut bytes, 10, p.data as u16);
            put_word(
                &mut bytes,
                8,
                if protected {
                    p.cs
                } else {
                    ((p.ip >> 4) as u16 & 0xF000) | p.opcode
                },
            );
            put_word(
                &mut bytes,
                12,
                if protected {
                    p.ds
                } else {
                    (p.data >> 4) as u16 & 0xF000
                },
            );
        }
        if registers {
            let start = if wide { 28 } else { 14 };
            let top = ((self.status >> 11) & 7) as usize;
            for i in 0..8 {
                bytes[start + i * 10..start + (i + 1) * 10]
                    .copy_from_slice(&self.raw((top + i) & 7).to_le_bytes()[..10]);
            }
        }
        bytes
    }
    /// Load raw saved registers without executing FLD: NaNs, empty tags and
    /// exception flags must survive a context switch unchanged.
    pub fn restore_environment(
        &mut self,
        bytes: &[u8; 108],
        wide: bool,
        protected: bool,
        registers: bool,
    ) {
        let stride = if wide { 4 } else { 2 };
        self.control = word(bytes, 0);
        self.status = word(bytes, stride);
        self.tags = word(bytes, 2 * stride);
        let p = &mut self.pointers;
        if wide && protected {
            p.ip = dword(bytes, 12);
            p.cs = word(bytes, 16);
            p.opcode = word(bytes, 18) & 0x7FF;
            p.data = dword(bytes, 20);
            p.ds = word(bytes, 24);
        } else if wide {
            p.ip = (dword(bytes, 12) & 0xFFFF) | ((dword(bytes, 16) & 0x0FFF_F000) << 4);
            p.data = (dword(bytes, 20) & 0xFFFF) | ((dword(bytes, 24) & 0x0FFF_F000) << 4);
            p.opcode = word(bytes, 16) & 0x7FF;
            p.cs = 0;
            p.ds = 0;
        } else {
            p.ip = u32::from(word(bytes, 6));
            p.data = u32::from(word(bytes, 10));
            if protected {
                p.cs = word(bytes, 8);
                p.ds = word(bytes, 12);
            } else {
                p.ip |= u32::from(word(bytes, 8) & 0xF000) << 4;
                p.data |= u32::from(word(bytes, 12) & 0xF000) << 4;
                p.opcode = word(bytes, 8) & 0x7FF;
                p.cs = 0;
                p.ds = 0;
            }
        }
        self.status = (self.status & !0x8080)
            | if self.status & !self.control & 0x3F != 0 {
                0x8080
            } else {
                0
            };
        if registers {
            let start = if wide { 28 } else { 14 };
            let top = ((self.status >> 11) & 7) as usize;
            for i in 0..8 {
                let slot = (top + i) & 7;
                let mut storage = [0; 16];
                storage[..10].copy_from_slice(&bytes[start + i * 10..start + (i + 1) * 10]);
                let raw = u128::from_le_bytes(storage);
                self.encodings[slot] = raw;
                let exponent = ((raw >> 64) & 0x7FFF) as i32;
                let mantissa = raw as u64;
                let negative = raw & (1 << 79) != 0;
                self.finite[slot] = if exponent < 0x7FFF && (exponent == 0 || mantissa >> 63 != 0) {
                    let shift = if mantissa == 0 {
                        0
                    } else {
                        mantissa.leading_zeros()
                    };
                    Some(Finite {
                        negative,
                        exponent: exponent.max(1) - 16383 - shift as i32,
                        mantissa: mantissa << shift,
                    })
                } else {
                    None
                };
                self.values[slot] = self.finite[slot].map(Finite::to_f64).unwrap_or_else(|| {
                    f64::from_bits(
                        (u64::from(negative) << 63)
                            | 0x7FF0_0000_0000_0000
                            | ((mantissa & ((1 << 63) - 1)) >> 11),
                    )
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initialization_empties_tags_without_erasing_physical_registers() {
        let mut fpu = X87::new();
        fpu.push_integer(i64::MAX).unwrap();
        let raw = fpu.raw(7);
        fpu.initialize();
        assert_eq!(fpu.tags(), 0xFFFF);
        assert_eq!(fpu.status(), 0);
        assert_eq!(fpu.raw(7), raw);
        // Restoring only the prior environment makes the same data visible.
        let mut env = fpu.save_environment(false, true, false);
        env[2..4].copy_from_slice(&0x3800_u16.to_le_bytes());
        env[4..6].copy_from_slice(&0x3FFF_u16.to_le_bytes());
        fpu.restore_environment(&env, false, true, false);
        assert_eq!(fpu.store_integer(64), Ok(i64::MAX as u64));
    }
    #[test]
    fn environment_roundtrips_all_layouts_top_tags_and_full_registers() {
        for wide in [false, true] {
            for protected in [false, true] {
                for top in 0..8 {
                    let mut source = X87::new();
                    source.record_instruction(0xABCDE, 0x1234, 0x456);
                    source.record_operand(0xFEDCB, 0x5678);
                    source.status = top << 11;
                    source.push_integer(i64::MAX).unwrap();
                    source.load_constant(crate::Constant::Pi).unwrap();
                    source.push(f64::INFINITY).unwrap();
                    source.push(-0.).unwrap();
                    source.free(2);
                    source.load_control(0xB7F);
                    let bytes = source.save_environment(wide, protected, true);
                    let mut destination = X87::new();
                    destination.restore_environment(&bytes, wide, protected, true);
                    assert_eq!(destination.save_environment(wide, protected, true), bytes);
                    assert_eq!(destination.tags(), source.tags());
                    assert_eq!(destination.status(), source.status());
                    for index in 0..8 {
                        assert_eq!(destination.raw(index), source.raw(index));
                    }
                    // FLDENV changes tags/TOP/control, not register contents.
                    let raw = core::array::from_fn::<_, 8, _>(|i| destination.raw(i));
                    let mut environment = bytes;
                    environment[0] = 0x7F;
                    destination.restore_environment(&environment, wide, protected, false);
                    assert_eq!(core::array::from_fn::<_, 8, _>(|i| destination.raw(i)), raw);
                }
            }
        }
    }
    #[test]
    fn protected_pointer_offsets_and_real_pointer_packing_match_intel_layouts() {
        let mut fpu = X87::new();
        fpu.record_instruction(0xABCDE, 0x1234, 0x456);
        fpu.record_operand(0xFEDCB, 0x5678);
        let p = fpu.save_environment(true, true, false);
        assert_eq!(&p[12..20], &[0xDE, 0xBC, 0x0A, 0, 0x34, 0x12, 0x56, 4]);
        assert_eq!(&p[20..26], &[0xCB, 0xED, 0x0F, 0, 0x78, 0x56]);
        let r = fpu.save_environment(false, false, false);
        assert_eq!(&r[6..14], &[0xDE, 0xBC, 0x56, 0xA4, 0xCB, 0xED, 0, 0xF0]);
        let r = fpu.save_environment(true, false, false);
        assert_eq!(
            &r[12..28],
            &[
                0xDE, 0xBC, 0, 0, 0x56, 0xA4, 0, 0, 0xCB, 0xED, 0, 0, 0, 0xF0, 0, 0
            ]
        );
    }
}
