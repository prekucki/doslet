//! Concrete RAM owners, borrowed by both interpreted and native execution.
use alloc::{boxed::Box, vec};

use super::PAGE_SIZE;

/// Keep backing selection static: execution borrows the selected owner's actual
/// bytes and page generations without allocating or copying guest memory.
pub(crate) enum Storage {
    Owned {
        bytes: Box<[u8]>,
        generations: Box<[u64]>,
    },
    #[cfg(feature = "wasm-procedures")]
    Wasm(Box<crate::wasm_procedures::SharedProcedureMemory>),
    #[cfg(feature = "v86")]
    V86(Box<crate::v86::Memory>),
}

impl Storage {
    pub(crate) fn new(size: usize) -> Self {
        Self::from_bytes(vec![0; size].into_boxed_slice())
    }

    /// Adopt bytes during setup or state restoration, with fresh generations.
    pub(crate) fn from_bytes(bytes: Box<[u8]>) -> Self {
        let generations = vec![0; bytes.len() / PAGE_SIZE].into_boxed_slice();
        Self::Owned { bytes, generations }
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Owned { bytes, .. } => bytes.len(),
            #[cfg(feature = "v86")]
            Self::V86(owner) => owner.cpu.physical_ram().len(),
            #[cfg(feature = "wasm-procedures")]
            Self::Wasm(owner) => owner.layout().ram_bytes,
        }
    }

    #[inline]
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Owned { bytes, .. } => bytes,
            #[cfg(feature = "v86")]
            Self::V86(owner) => owner.cpu.physical_ram(),
            #[cfg(feature = "wasm-procedures")]
            Self::Wasm(owner) => owner.ram(),
        }
    }

    #[inline]
    pub(crate) fn generations(&self) -> &[u64] {
        match self {
            Self::Owned { generations, .. } => generations,
            #[cfg(feature = "v86")]
            Self::V86(owner) => &owner.generations,
            #[cfg(feature = "wasm-procedures")]
            Self::Wasm(owner) => owner.generations(),
        }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn generations_mut(&mut self) -> &mut [u64] {
        match self {
            Self::Owned { generations, .. } => generations,
            #[cfg(feature = "v86")]
            Self::V86(owner) => &mut owner.generations,
            #[cfg(feature = "wasm-procedures")]
            Self::Wasm(owner) => owner.generations_mut(),
        }
    }

    /// One exclusive borrow yields disjoint backing slices for direct stores.
    #[inline]
    pub(crate) fn parts_mut(&mut self) -> (&mut [u8], &mut [u64]) {
        match self {
            Self::Owned { bytes, generations } => (bytes, generations),
            #[cfg(feature = "v86")]
            Self::V86(owner) => {
                owner.host_writes = true;
                (owner.cpu.physical_ram_mut(), &mut owner.generations)
            }
            #[cfg(feature = "wasm-procedures")]
            Self::Wasm(owner) => owner.parts_mut(),
        }
    }

    #[cfg(feature = "wasm-procedures")]
    pub(crate) fn parts_with_video_mut(&mut self) -> Option<(&mut [u8], &mut [u64], &mut [u8])> {
        match self {
            Self::Owned { .. } => None,
            #[cfg(feature = "v86")]
            Self::V86(_) => None,
            Self::Wasm(owner) => Some(owner.parts_with_video_mut()),
        }
    }

    #[inline]
    pub(crate) fn video(&self) -> Option<&[u8]> {
        match self {
            Self::Owned { .. } => None,
            #[cfg(feature = "v86")]
            Self::V86(_) => None,
            #[cfg(feature = "wasm-procedures")]
            Self::Wasm(owner) => Some(owner.video()),
        }
    }

    #[inline]
    pub(crate) fn video_mut(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::Owned { .. } => None,
            #[cfg(feature = "v86")]
            Self::V86(_) => None,
            #[cfg(feature = "wasm-procedures")]
            Self::Wasm(owner) => Some(owner.video_mut()),
        }
    }
}
