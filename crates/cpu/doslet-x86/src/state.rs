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

use core::sync::atomic::{AtomicU32, Ordering};

// Setup-only lineage identities for `Registers` values; they contain no guest
// state and never wrap. Exhaustion leaves new values without a lineage.
static NEXT_LINEAGE: AtomicU32 = AtomicU32::new(1);

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
    /// Decode-cache hint, not guest state: a lineage unique to this value
    /// (0 when exhausted) and a version advanced on every hidden CS cache
    /// reload. `Registers` is not `Clone`, so no other value can reproduce a
    /// (lineage, version) pair with different CS state.
    pub(crate) cs_lineage: u32,
    pub(crate) cs_version: u64,
}
impl Default for Registers {
    fn default() -> Self {
        Self {
            general: [0; 8],
            cs: 0,
            ds: 0,
            es: 0,
            ss: 0,
            fs: 0,
            gs: 0,
            ip: 0,
            protected: false,
            caches: [SegmentCache::default(); 6],
            cpl: 0,
            cs_lineage: NEXT_LINEAGE
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .unwrap_or(0),
            cs_version: 0,
        }
    }
}
// Equality and Debug cover guest state only, excluding the decode-cache hint.
impl PartialEq for Registers {
    fn eq(&self, other: &Self) -> bool {
        self.general == other.general
            && self.cs == other.cs
            && self.ds == other.ds
            && self.es == other.es
            && self.ss == other.ss
            && self.fs == other.fs
            && self.gs == other.gs
            && self.ip == other.ip
            && self.protected == other.protected
            && self.caches == other.caches
            && self.cpl == other.cpl
    }
}
impl Eq for Registers {}
impl core::fmt::Debug for Registers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Registers")
            .field("general", &self.general)
            .field("cs", &self.cs)
            .field("ds", &self.ds)
            .field("es", &self.es)
            .field("ss", &self.ss)
            .field("fs", &self.fs)
            .field("gs", &self.gs)
            .field("ip", &self.ip)
            .field("protected", &self.protected)
            .field("caches", &self.caches)
            .field("cpl", &self.cpl)
            .finish()
    }
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

/// Offsets accepted by `Registers::address` for one segment and access kind.
/// Valid while the selector, its hidden cache and PE mode stay unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SegmentWindow {
    base: u32,
    first: u32,
    last: u32,
}
impl SegmentWindow {
    #[inline]
    pub(crate) fn address(self, offset: u32, length: u8) -> Option<u32> {
        let last = offset.checked_add(u32::from(length) - 1)?;
        (offset >= self.first && last <= self.last).then(|| self.base.wrapping_add(offset))
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
        if segment == SegmentRegister::Cs {
            self.cs_version = self.cs_version.saturating_add(1);
        }
    }
    pub(crate) fn sync_mode(&mut self, protected: bool) {
        if protected && !self.protected {
            for segment in SegmentRegister::ALL {
                self.caches[segment as usize] = self.segment_cache(segment);
            }
        }
        // Materialized caches always accompany a PE change, which decode-cache
        // keys record directly; the CS version tracks only CS reloads.
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
    /// Resolve the offset-independent checks of `address` once.
    pub(crate) fn window(&self, segment: SegmentRegister, access: Access) -> Option<SegmentWindow> {
        let cache = self.segment_cache(segment);
        if !cache.usable || (self.protected && cache.selector != self.selector(segment)) {
            return None;
        }
        if self.protected {
            let code = cache.access & 8 != 0;
            let valid = match access {
                Access::Execute => code,
                Access::Read => !code || cache.access & 2 != 0,
                Access::Write => !code && cache.access & 2 != 0,
            };
            if !valid {
                return None;
            }
        }
        let (first, last) = if cache.access & 0x1C == 0x14 {
            let upper = if cache.default32 { u32::MAX } else { 0xFFFF };
            (cache.limit.checked_add(1)?, upper)
        } else {
            (0, cache.limit)
        };
        Some(SegmentWindow {
            base: cache.base,
            first,
            last,
        })
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
    #[cfg(all(
        feature = "dynamic-core",
        not(target_arch = "wasm32"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(crate) fn native_state(&mut self) -> (&mut [u32; 8], &mut u32) {
        (&mut self.general, &mut self.ip)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn check(r: &Registers, segment: SegmentRegister) {
        let limit = r.caches[segment as usize].limit;
        let offsets = [
            0,
            1,
            limit.wrapping_sub(1),
            limit,
            limit.wrapping_add(1),
            0xFFFE,
            0xFFFF,
            0x10000,
            u32::MAX - 14,
            u32::MAX - 1,
            u32::MAX,
        ];
        for access in [Access::Read, Access::Write, Access::Execute] {
            let window = r.window(segment, access);
            for offset in offsets {
                for length in [1, 2, 4, 15] {
                    assert_eq!(
                        window.and_then(|w| w.address(offset, length)),
                        r.address(segment, offset, length, access).ok(),
                        "{r:?} {segment:?} {access:?} offset={offset:#x} length={length}"
                    );
                }
            }
        }
    }

    #[test]
    fn cs_versions_track_cs_reloads_without_affecting_equality() {
        let mut r = Registers::default();
        let other = Registers::default();
        assert!(r.cs_lineage != 0 && r.cs_lineage != other.cs_lineage);
        assert_eq!(r, other);
        r.set_segment(SegmentRegister::Ds, SegmentCache::real(1, false));
        assert_eq!(r.cs_version, 0);
        r.set_segment(SegmentRegister::Cs, SegmentCache::real(2, true));
        assert_eq!(r.cs_version, 1);
        // PE changes are keyed directly by the decode cache.
        r.sync_mode(true);
        r.sync_mode(false);
        assert_eq!(r.cs_version, 1);
        r.cs_version = u64::MAX;
        r.set_segment(SegmentRegister::Cs, SegmentCache::real(3, true));
        assert_eq!(r.cs_version, u64::MAX);
    }

    #[test]
    fn segment_windows_accept_exactly_the_offsets_of_address() {
        let limits = [0, 0xFFF, 0xFFFE, 0xFFFF, 0x10000, 0xFFFF_FFFE, 0xFFFF_FFFF];
        let accesses = [0x93, 0x91, 0x97, 0x95, 0x9B, 0x99, 0x9F];
        for (protected, usable, selector) in [
            (false, true, 0x28),
            (false, true, 0x30),
            (false, false, 0x28),
            (true, true, 0x28),
            (true, true, 0x30),
            (true, false, 0x28),
        ] {
            for access in accesses {
                for default32 in [false, true] {
                    for limit in limits {
                        for segment in [SegmentRegister::Cs, SegmentRegister::Ds] {
                            let mut r = Registers {
                                cs: 0x28,
                                ds: 0x28,
                                protected,
                                ..Registers::default()
                            };
                            r.caches[segment as usize] = SegmentCache {
                                selector,
                                base: 0xFFFF_F000,
                                limit,
                                access,
                                flags: 0,
                                default32,
                                usable,
                            };
                            check(&r, segment);
                        }
                    }
                }
            }
        }
    }
}
