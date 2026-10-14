use super::{Cpu, CpuError};
use crate::{Access, SegmentCache, SegmentRegister, decode, flags};
use doslet_bus::MemoryBus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorError {
    NullSelector,
    TableBounds,
    NotPresent,
    WrongType,
    Privilege,
    LocalTableUnavailable,
    InvalidGate,
}

impl Cpu {
    pub fn protected_mode(&self) -> bool {
        self.control.cr0 & 1 != 0
    }
    pub fn stack_pointer(&self) -> u32 {
        if self.registers.stack32() {
            self.registers.get32(crate::Gpr32::Esp)
        } else {
            u32::from(self.registers.get(crate::Gpr16::Sp))
        }
    }
    pub(super) fn set_stack_pointer(&mut self, value: u32) {
        if self.registers.stack32() {
            self.registers.set32(crate::Gpr32::Esp, value);
        } else {
            self.registers.set(crate::Gpr16::Sp, value as u16);
        }
    }
    pub(super) fn adjust_stack(&mut self, amount: u32) {
        self.set_stack_pointer(self.stack_pointer().wrapping_add(amount));
    }

    pub(super) fn descriptor<B: MemoryBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> Result<SegmentCache, CpuError> {
        let cache = self.descriptor_raw(bus, selector)?;
        if cache.access & 0x80 == 0 {
            return Err(CpuError::Descriptor {
                selector,
                reason: DescriptorError::NotPresent,
            });
        }
        Ok(cache)
    }
    pub(super) fn descriptor_raw<B: MemoryBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
    ) -> Result<SegmentCache, CpuError> {
        let fail = |reason| CpuError::Descriptor { selector, reason };
        if selector & !3 == 0 {
            return Err(fail(DescriptorError::NullSelector));
        }
        let table = if selector & 4 == 0 {
            self.control.gdtr
        } else {
            self.control
                .ldtr
                .ok_or_else(|| fail(DescriptorError::LocalTableUnavailable))?
        };
        let offset = u32::from(selector & !7);
        if offset + 7 > u32::from(table.limit) {
            return Err(fail(DescriptorError::TableBounds));
        }
        let address = table.base.wrapping_add(offset);
        let mut bytes = [0; 8];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = bus.read_u8(address.wrapping_add(index as u32))?;
        }
        let base = u32::from(bytes[2])
            | (u32::from(bytes[3]) << 8)
            | (u32::from(bytes[4]) << 16)
            | (u32::from(bytes[7]) << 24);
        let limit =
            u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[6] & 15) << 16);
        let limit = if bytes[6] & 0x80 != 0 {
            (limit << 12) | 0xFFF
        } else {
            limit
        };
        Ok(SegmentCache {
            selector,
            base,
            limit,
            access: bytes[5],
            flags: bytes[6] & 0xF0,
            default32: bytes[6] & 0x40 != 0,
            usable: true,
        })
    }
    fn validated_segment<B: MemoryBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentRegister,
        selector: u16,
    ) -> Result<SegmentCache, CpuError> {
        if !self.protected_mode() {
            return Ok(SegmentCache::real(selector, segment == SegmentRegister::Cs));
        }
        let fail = |reason| CpuError::Descriptor { selector, reason };
        if selector & !3 == 0 && !matches!(segment, SegmentRegister::Cs | SegmentRegister::Ss) {
            return Ok(SegmentCache {
                usable: false,
                ..SegmentCache::real(selector, false)
            });
        }
        let cache = self.descriptor(bus, selector)?;
        if cache.access & 0x10 == 0 {
            return Err(fail(DescriptorError::WrongType));
        }
        let code = cache.access & 8 != 0;
        let dpl = (cache.access >> 5) & 3;
        let rpl = (selector & 3) as u8;
        let cpl = self.registers.cpl;
        let conforms = code && cache.access & 4 != 0;
        match segment {
            SegmentRegister::Cs => {
                if !code {
                    return Err(fail(DescriptorError::WrongType));
                }
                if (conforms && dpl > cpl) || (!conforms && (dpl != cpl || rpl > cpl)) {
                    return Err(fail(DescriptorError::Privilege));
                }
            }
            SegmentRegister::Ss => {
                if code || cache.access & 2 == 0 {
                    return Err(fail(DescriptorError::WrongType));
                }
                if dpl != cpl || rpl != cpl {
                    return Err(fail(DescriptorError::Privilege));
                }
            }
            _ => {
                if code && cache.access & 2 == 0 {
                    return Err(fail(DescriptorError::WrongType));
                }
                if !conforms && cpl.max(rpl) > dpl {
                    return Err(fail(DescriptorError::Privilege));
                }
            }
        }
        if cache.access & 1 == 0 {
            let table = if selector & 4 == 0 {
                self.control.gdtr
            } else {
                self.control.ldtr.unwrap()
            };
            bus.write_u8(
                table.base.wrapping_add(u32::from(selector & !7) + 5),
                cache.access | 1,
            )?;
        }
        Ok(SegmentCache {
            access: cache.access | 1,
            ..cache
        })
    }
    pub fn load_segment<B: MemoryBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentRegister,
        selector: u16,
    ) -> Result<(), CpuError> {
        let cache = self.validated_segment(bus, segment, selector)?;
        self.registers.set_segment(segment, cache);
        if segment == SegmentRegister::Ss {
            self.interrupt_shadow = 1;
        }
        Ok(())
    }
    pub(super) fn far_jump<B: MemoryBus>(
        &mut self,
        bus: &mut B,
        selector: u16,
        offset: u32,
    ) -> Result<(), CpuError> {
        let cache = self.validated_segment(bus, SegmentRegister::Cs, selector)?;
        if offset > cache.limit {
            return Err(CpuError::SegmentAccess {
                segment: SegmentRegister::Cs,
                selector,
                offset,
                length: 1,
                access: Access::Execute,
            });
        }
        self.registers.set_segment(SegmentRegister::Cs, cache);
        self.registers.ip = offset;
        Ok(())
    }
    pub(super) fn near_jump(&mut self, target: u32) -> Result<(), CpuError> {
        self.registers
            .address(SegmentRegister::Cs, target, 1, Access::Execute)?;
        self.registers.ip = target;
        Ok(())
    }
    pub(super) fn interrupt_protected<B: MemoryBus>(
        &mut self,
        bus: &mut B,
        vector: u8,
    ) -> Result<(), CpuError> {
        let fail = |reason| CpuError::Descriptor {
            selector: u16::from(vector) * 8,
            reason,
        };
        let offset = u32::from(vector) * 8;
        if offset + 7 > u32::from(self.control.idtr.limit) {
            return Err(fail(DescriptorError::TableBounds));
        }
        let address = self.control.idtr.base.wrapping_add(offset);
        let low = decode::read_dword(bus, address)?;
        let high = decode::read_dword(bus, address + 4)?;
        let selector = (low >> 16) as u16;
        let attributes = (high >> 8) as u8;
        let kind = attributes & 31;
        if attributes & 0x80 == 0 {
            return Err(fail(DescriptorError::NotPresent));
        }
        if !matches!(kind, 6 | 7 | 14 | 15) {
            return Err(fail(DescriptorError::InvalidGate));
        }
        if (attributes >> 5) & 3 < self.registers.cpl {
            return Err(fail(DescriptorError::Privilege));
        }
        let cache = self.validated_segment(bus, SegmentRegister::Cs, selector)?;
        let target = if kind >= 14 {
            (low & 0xFFFF) | (high & 0xFFFF0000)
        } else {
            low & 0xFFFF
        };
        if target > cache.limit {
            return Err(fail(DescriptorError::TableBounds));
        }
        if kind >= 14 {
            self.push32(bus, self.flags)?;
            self.push32(bus, u32::from(self.registers.cs))?;
            self.push32(bus, self.registers.ip)?;
        } else {
            self.push(bus, self.flags as u16)?;
            self.push(bus, self.registers.cs)?;
            self.push(bus, self.registers.ip as u16)?;
        }
        self.flags &= !(flags::TRAP | flags::RESUME | (1 << 14));
        if kind & 1 == 0 {
            self.flags &= !flags::INTERRUPT;
        }
        self.registers.set_segment(SegmentRegister::Cs, cache);
        self.registers.ip = target;
        Ok(())
    }
}
