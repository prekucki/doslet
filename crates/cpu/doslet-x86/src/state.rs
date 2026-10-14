#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Gpr16 {
    Ax,
    Cx,
    Dx,
    Bx,
    Sp,
    Bp,
    Si,
    Di,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Gpr32 {
    Eax,
    Ecx,
    Edx,
    Ebx,
    Esp,
    Ebp,
    Esi,
    Edi,
}
impl Gpr32 {
    pub(crate) fn decode(code: u8) -> Self {
        match code & 7 {
            0 => Self::Eax,
            1 => Self::Ecx,
            2 => Self::Edx,
            3 => Self::Ebx,
            4 => Self::Esp,
            5 => Self::Ebp,
            6 => Self::Esi,
            _ => Self::Edi,
        }
    }
}

/// i386 control-register state. This machine currently has no x87 coprocessor.
#[derive(Debug, PartialEq, Eq)]
pub struct ControlRegisters {
    pub cr0: u32,
    pub cr2: u32,
    pub cr3: u32,
    pub gdtr: DescriptorTable,
    pub idtr: DescriptorTable,
    pub ldtr: Option<DescriptorTable>,
    pub ldtr_selector: u16,
    pub task: Option<SegmentCache>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DescriptorTable {
    pub base: u32,
    pub limit: u16,
}
impl Default for ControlRegisters {
    fn default() -> Self {
        Self {
            cr0: 0x14,
            cr2: 0,
            cr3: 0,
            gdtr: DescriptorTable {
                base: 0,
                limit: 0xFFFF,
            },
            idtr: DescriptorTable {
                base: 0,
                limit: 0x3FF,
            },
            ldtr: None,
            ldtr_selector: 0,
            task: None,
        }
    } // ET | EM, PE/PG clear.
}

impl Gpr16 {
    pub(crate) fn decode(code: u8) -> Self {
        match code & 7 {
            0 => Self::Ax,
            1 => Self::Cx,
            2 => Self::Dx,
            3 => Self::Bx,
            4 => Self::Sp,
            5 => Self::Bp,
            6 => Self::Si,
            _ => Self::Di,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Registers {
    general: [u32; 8],
    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
    pub fs: u16,
    pub gs: u16,
    pub ip: u32,
    pub(crate) protected: bool,
    pub(crate) caches: [SegmentCache; 6],
    pub(crate) cpl: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SegmentRegister {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}
impl SegmentRegister {
    pub const ALL: [Self; 6] = [Self::Es, Self::Cs, Self::Ss, Self::Ds, Self::Fs, Self::Gs];
    pub(crate) fn decode(value: u8) -> Option<Self> {
        Self::ALL.get(value as usize).copied()
    }
}

/// Small hidden segment state, cached on a segment-register load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentCache {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    pub access: u8,
    pub flags: u8,
    pub default32: bool,
    pub usable: bool,
}
impl Default for SegmentCache {
    fn default() -> Self {
        Self::real(0, false)
    }
}
impl SegmentCache {
    pub const fn real(selector: u16, code: bool) -> Self {
        Self {
            selector,
            base: selector as u32 * 16,
            limit: 0xFFFF,
            access: if code { 0x9B } else { 0x93 },
            flags: 0,
            default32: false,
            usable: true,
        }
    }
}

impl Registers {
    pub fn selector(&self, segment: SegmentRegister) -> u16 {
        match segment {
            SegmentRegister::Es => self.es,
            SegmentRegister::Cs => self.cs,
            SegmentRegister::Ss => self.ss,
            SegmentRegister::Ds => self.ds,
            SegmentRegister::Fs => self.fs,
            SegmentRegister::Gs => self.gs,
        }
    }
    pub fn segment_cache(&self, segment: SegmentRegister) -> SegmentCache {
        let cache = self.caches[segment as usize];
        let selector = self.selector(segment);
        if !self.protected
            && (cache.selector != selector
                || (segment == SegmentRegister::Cs && cache.access & 8 == 0))
        {
            SegmentCache::real(selector, segment == SegmentRegister::Cs)
        } else {
            cache
        }
    }
    pub(crate) fn set_segment(&mut self, segment: SegmentRegister, cache: SegmentCache) {
        match segment {
            SegmentRegister::Es => self.es = cache.selector,
            SegmentRegister::Cs => self.cs = cache.selector,
            SegmentRegister::Ss => self.ss = cache.selector,
            SegmentRegister::Ds => self.ds = cache.selector,
            SegmentRegister::Fs => self.fs = cache.selector,
            SegmentRegister::Gs => self.gs = cache.selector,
        }
        self.caches[segment as usize] = cache;
    }
    pub(crate) fn sync_mode(&mut self, protected: bool) {
        if protected && !self.protected {
            for segment in SegmentRegister::ALL {
                self.caches[segment as usize] = self.segment_cache(segment);
            }
        }
        self.protected = protected;
    }
    pub fn code32(&self) -> bool {
        self.segment_cache(SegmentRegister::Cs).default32
    }
    pub fn stack32(&self) -> bool {
        self.segment_cache(SegmentRegister::Ss).default32
    }
    pub fn address(
        &self,
        segment: SegmentRegister,
        offset: u32,
        length: u8,
        access: Access,
    ) -> Result<u32, crate::CpuError> {
        let cache = self.segment_cache(segment);
        let fault = || crate::CpuError::SegmentAccess {
            segment,
            selector: self.selector(segment),
            offset,
            length,
            access,
        };
        if !cache.usable || (self.protected && cache.selector != self.selector(segment)) {
            return Err(fault());
        }
        let last = offset
            .checked_add(u32::from(length) - 1)
            .ok_or_else(fault)?;
        let expand_down = cache.access & 0x1C == 0x14;
        if expand_down {
            let upper = if cache.default32 { u32::MAX } else { 0xFFFF };
            if offset <= cache.limit || last > upper {
                return Err(fault());
            }
        } else if last > cache.limit {
            return Err(fault());
        }
        if self.protected {
            let code = cache.access & 8 != 0;
            let valid = match access {
                Access::Execute => code,
                Access::Read => !code || cache.access & 2 != 0,
                Access::Write => !code && cache.access & 2 != 0,
            };
            if !valid {
                return Err(fault());
            }
        }
        Ok(cache.base.wrapping_add(offset))
    }
    pub fn get(&self, register: Gpr16) -> u16 {
        self.general[register as usize] as u16
    }
    pub fn set(&mut self, register: Gpr16, value: u16) {
        let target = &mut self.general[register as usize];
        *target = (*target & 0xFFFF0000) | u32::from(value);
    }
    pub fn get32(&self, register: Gpr32) -> u32 {
        self.general[register as usize]
    }
    pub fn set32(&mut self, register: Gpr32, value: u32) {
        self.general[register as usize] = value;
    }
    pub fn get_byte(&self, code: u8) -> u8 {
        let value = self.general[usize::from(code & 3)];
        if code & 4 == 0 {
            value as u8
        } else {
            (value >> 8) as u8
        }
    }
    pub fn set_byte(&mut self, code: u8, value: u8) {
        let target = &mut self.general[usize::from(code & 3)];
        *target = if code & 4 == 0 {
            (*target & 0xFFFFFF00) | u32::from(value)
        } else {
            (*target & 0xFFFF00FF) | (u32::from(value) << 8)
        };
    }
}
