//! Explicit, offline relocation binding for a deliberately narrow DOS-flat model.
//!
//! The caller supplies actual load bases and selector properties; preferred LE
//! bases never choose a layout. Normal code/data32 views have base zero, so their
//! EIP/offset origin is the object's linear base. Normal code/data16 views have
//! the object's linear base and fit the first 64 KiB, so their offset origin is
//! zero. Optional alias views use that same 16-bit, object-relative model.
//! An object may declare only its linear address, with no normal selector, when
//! no normal selector fixup needs it. This does not establish a code context.
//!
//! `Offset32` encodes target linear base plus the symbolic target addend.
//! `SelfRelative32` in a default32 source additionally subtracts the source
//! linear address plus four. These formulas intentionally use modular u32
//! arithmetic; target addends are not dereferenced or constrained to the object.
//! `Selector16` emits the declared target normal selector, or its explicit alias
//! when requested by the record. Other source kinds are unsupported.
//!
//! This validates declarations, not CPU state. It neither allocates descriptors
//! nor verifies their presence, type, privilege, hidden caches or live code.
//! Runtime integration must verify those actual bindings and code independently.
//! `encode_object` writes a caller-owned offline buffer, never guest RAM.
//!
//! The supported 32-bit field formulas follow the DOS/32A loader's `apply_fixups`
//! and `fix_32selfrel` routines, and its `create_selector` flat/code16 distinction:
//! <https://github.com/amindlost/dos32a/blob/master/src/dos32a/loader.asm>.
//! Source kinds and alias flags follow Open Watcom's `exeflat.h`:
//! <https://github.com/open-watcom/open-watcom-v2/blob/master/bld/watcom/h/exeflat.h>.
//! This is not a general OS/2 LE loader. Narrow offsets, far pointers and the
//! DOS/32A special code16 self-relative encoding remain explicit unsupported cases.

use alloc::vec::Vec;
use core::{fmt, mem::size_of, ops::Range};
use doslet_executable::LeFixupSourceType;

use crate::{DefaultBitness, ObjectAddress, Relocation, SymbolicImage};

/// Caller-declared properties of an existing selector, with an inclusive byte
/// limit after descriptor granularity expansion. Rights and CPU cache state are
/// outside this offline model. RPL is retained in emitted selector values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DosFlatSelector {
    pub value: u16,
    pub base: u32,
    pub limit: u32,
    pub default_bitness: DefaultBitness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DosFlatObjectBinding {
    pub object_number: u32,
    /// Actual object origin. The final byte may be at `u32::MAX`; the exclusive
    /// end is checked in u64 against 2^32. Loaded object extents cannot wrap or
    /// overlap other nonempty objects.
    pub linear_base: u32,
    /// A caller-supplied normal view. `None` declares only the object's address;
    /// normal `Selector16` fixups targeting it are then rejected.
    pub normal: Option<DosFlatSelector>,
    /// A real caller-supplied object-relative 16-bit view, never synthesized.
    /// This supported subset requires the whole object to fit within 64 KiB.
    pub alias: Option<DosFlatSelector>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingLimits {
    pub max_patches: usize,
    /// Requested vector storage, including temporary range and selector indexes.
    /// Allocator bookkeeping is excluded; reservations are charged before
    /// allocation and never refunded during construction.
    pub max_owned_bytes: usize,
}

impl Default for BindingLimits {
    fn default() -> Self {
        Self {
            max_patches: 1_000_000,
            max_owned_bytes: 128 * 1024 * 1024,
        }
    }
}

/// One completely encoded canonical write. Bytes have a fixed upper bound and
/// are immutable through the plan. Currently supported encodings use 2 or 4 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelocationPatch {
    pub source: ObjectAddress,
    pub kind: LeFixupSourceType,
    bytes: [u8; 6],
    byte_len: u8,
    object_offset: usize,
}

impl RelocationPatch {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.byte_len)]
    }
}

#[derive(Debug)]
struct BoundObject {
    binding: DosFlatObjectBinding,
    patches: Range<usize>,
}

/// Published only after validating the complete binding and encoding every
/// patch. Borrows original unrelocated bytes; no guest memory is owned or loaded.
#[derive(Debug)]
pub struct DosFlatBindingPlan<'image> {
    image: &'image SymbolicImage,
    objects: Vec<BoundObject>,
    patches: Vec<RelocationPatch>,
}

impl<'image> DosFlatBindingPlan<'image> {
    pub fn new(
        image: &'image SymbolicImage,
        bindings: &[DosFlatObjectBinding],
        limits: BindingLimits,
    ) -> Result<Self, BindingError> {
        let mut budget = Budget(limits.max_owned_bytes);
        let mut objects = Vec::new();
        budget.reserve(&mut objects, bindings.len(), BindingStorage::Objects)?;
        for &binding in bindings {
            if image.object(binding.object_number).is_none() {
                return Err(BindingError::UnknownObject(binding.object_number));
            }
            objects.push(BoundObject {
                binding,
                patches: 0..0,
            });
        }
        objects.sort_unstable_by_key(|object| object.binding.object_number);
        for adjacent in objects.windows(2) {
            if adjacent[0].binding.object_number == adjacent[1].binding.object_number {
                return Err(BindingError::DuplicateObject(
                    adjacent[0].binding.object_number,
                ));
            }
        }
        for (index, object) in image.objects().iter().enumerate() {
            if objects
                .get(index)
                .map(|object| object.binding.object_number)
                != Some(object.object_number())
            {
                return Err(BindingError::MissingObject(object.object_number()));
            }
        }
        validate_layout(image, &objects, &mut budget)?;

        let count = image.relocations().len();
        if count > limits.max_patches {
            return Err(BindingError::PatchLimit);
        }
        let mut patches = Vec::new();
        budget.reserve(&mut patches, count, BindingStorage::Patches)?;
        for relocation in image.relocations() {
            // SymbolicImage guarantees full source widths, valid target object
            // IDs, nonoverlapping writes and deterministic canonical ordering.
            let source_index = object_index(relocation.source.object_number)?;
            let target_index = object_index(relocation.target.object_number)?;
            let source = objects[source_index].binding;
            let target = objects[target_index].binding;
            let patch = encode_patch(image, relocation, source, target)?;
            objects[source_index].patches.end += 1;
            patches.push(patch);
        }
        let mut start = 0;
        for object in &mut objects {
            let end = start + object.patches.end; // Sum is the reserved patch count.
            object.patches = start..end;
            start = end;
        }
        Ok(Self {
            image,
            objects,
            patches,
        })
    }

    pub fn image(&self) -> &'image SymbolicImage {
        self.image
    }

    pub fn object_binding(&self, object_number: u32) -> Option<DosFlatObjectBinding> {
        Some(self.objects.get(object_index(object_number).ok()?)?.binding)
    }

    /// Ordered by source object number, then offset, as in the symbolic image.
    pub fn patches(&self) -> &[RelocationPatch] {
        &self.patches
    }

    pub fn object_patches(&self, object_number: u32) -> Option<&[RelocationPatch]> {
        let object = self.objects.get(object_index(object_number).ok()?)?;
        Some(&self.patches[object.patches.clone()])
    }

    /// Copies original bytes and applies preencoded patches to exactly one
    /// caller-owned offline object buffer. All fallible checks finish before
    /// the first write, so an invalid object or length leaves `output` unchanged.
    /// This is not a MemoryBus or runtime loader interface.
    pub fn encode_object(&self, object_number: u32, output: &mut [u8]) -> Result<(), BindingError> {
        let object = self
            .image
            .object(object_number)
            .ok_or(BindingError::UnknownObject(object_number))?;
        if output.len() != object.bytes().len() {
            return Err(BindingError::OutputLength {
                object_number,
                expected: object.bytes().len(),
                actual: output.len(),
            });
        }
        // The plan was fully validated at construction; these accesses cannot
        // fail, and the image and all patch ranges remain immutably borrowed.
        let patches = self
            .object_patches(object_number)
            .ok_or(BindingError::UnknownObject(object_number))?;
        output.copy_from_slice(object.bytes());
        for patch in patches {
            let start = patch.object_offset;
            output[start..start + patch.bytes().len()].copy_from_slice(patch.bytes());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectorView {
    Normal,
    Alias,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectorProblem {
    Null,
    Bitness,
    Base,
    /// The full object must fit the view's inclusive effective byte limit.
    Coverage,
    /// A code/data16 or alias view must stay inside the first 64 KiB.
    Window16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingStorage {
    Objects,
    RangeIndex,
    SelectorIndex,
    Patches,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingError {
    UnknownObject(u32),
    MissingObject(u32),
    DuplicateObject(u32),
    ObjectExtentOverflow(u32),
    OverlappingObjects {
        first: u32,
        second: u32,
    },
    Selector {
        object_number: u32,
        view: SelectorView,
        problem: SelectorProblem,
    },
    InconsistentSelector {
        /// Descriptor identity includes TI and index; RPL bits are stripped.
        identity: u16,
        first_object: u32,
        second_object: u32,
    },
    MissingNormal {
        source: ObjectAddress,
        target_object: u32,
    },
    MissingAlias {
        source: ObjectAddress,
        target_object: u32,
    },
    UnsupportedKind {
        source: ObjectAddress,
        kind: LeFixupSourceType,
    },
    /// DOS/32A uses a special narrow encoding for this kind in code16 sources.
    UnsupportedCode16SelfRelative {
        source: ObjectAddress,
    },
    OutputLength {
        object_number: u32,
        expected: usize,
        actual: usize,
    },
    PatchLimit,
    OwnedByteLimit(BindingStorage),
    Allocation(BindingStorage),
    Arithmetic(&'static str),
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownObject(number) => write!(f, "unknown DOS-flat object {number}"),
            Self::MissingObject(number) => {
                write!(f, "missing DOS-flat binding for object {number}")
            }
            Self::DuplicateObject(number) => {
                write!(f, "duplicate DOS-flat binding for object {number}")
            }
            Self::ObjectExtentOverflow(number) => {
                write!(
                    f,
                    "DOS-flat object {number} exceeds the 32-bit linear address space"
                )
            }
            Self::OverlappingObjects { first, second } => {
                write!(f, "DOS-flat objects {first} and {second} overlap")
            }
            Self::Selector {
                object_number,
                view,
                problem,
            } => {
                write!(
                    f,
                    "DOS-flat object {object_number} {view:?} selector: {problem:?}"
                )
            }
            Self::InconsistentSelector {
                identity,
                first_object,
                second_object,
            } => {
                write!(
                    f,
                    "DOS-flat selector {identity:#x} has inconsistent views in objects {first_object} and {second_object}"
                )
            }
            Self::MissingNormal {
                source,
                target_object,
            } => {
                write!(
                    f,
                    "DOS-flat fixup at {source:?} requires an explicit normal selector for object {target_object}"
                )
            }
            Self::MissingAlias {
                source,
                target_object,
            } => {
                write!(
                    f,
                    "DOS-flat fixup at {source:?} requires an explicit alias for object {target_object}"
                )
            }
            Self::UnsupportedKind { source, kind } => {
                write!(f, "unsupported DOS-flat fixup {kind:?} at {source:?}")
            }
            Self::UnsupportedCode16SelfRelative { source } => {
                write!(
                    f,
                    "unsupported DOS-flat code16 self-relative fixup at {source:?}"
                )
            }
            Self::OutputLength {
                object_number,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "offline object {object_number} output length {actual}, expected {expected}"
                )
            }
            Self::PatchLimit => f.write_str("DOS-flat patch-count limit"),
            Self::OwnedByteLimit(storage) => write!(f, "DOS-flat owned-byte limit: {storage:?}"),
            Self::Allocation(storage) => write!(f, "cannot reserve DOS-flat {storage:?}"),
            Self::Arithmetic(field) => write!(f, "DOS-flat arithmetic overflow: {field}"),
        }
    }
}

impl core::error::Error for BindingError {}

struct Budget(usize);

impl Budget {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        count: usize,
        storage: BindingStorage,
    ) -> Result<(), BindingError> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(BindingError::OwnedByteLimit(storage))?;
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(BindingError::OwnedByteLimit(storage))?;
        values
            .try_reserve_exact(count)
            .map_err(|_| BindingError::Allocation(storage))
    }
}

#[derive(Clone, Copy)]
struct LoadedRange {
    object_number: u32,
    start: u32,
    end: u64,
}

#[derive(Clone, Copy)]
struct SelectorRecord {
    object_number: u32,
    selector: DosFlatSelector,
}

fn validate_layout(
    image: &SymbolicImage,
    objects: &[BoundObject],
    budget: &mut Budget,
) -> Result<(), BindingError> {
    let mut ranges = Vec::new();
    budget.reserve(&mut ranges, objects.len(), BindingStorage::RangeIndex)?;
    let selector_count = objects
        .iter()
        .filter(|object| object.binding.normal.is_some())
        .count()
        .checked_add(
            objects
                .iter()
                .filter(|object| object.binding.alias.is_some())
                .count(),
        )
        .ok_or(BindingError::Arithmetic("selector count"))?;
    let mut selectors = Vec::new();
    budget.reserve(
        &mut selectors,
        selector_count,
        BindingStorage::SelectorIndex,
    )?;
    for (object, bound) in image.objects().iter().zip(objects) {
        let binding = bound.binding;
        let size = object.metadata().virtual_size;
        let end = u64::from(binding.linear_base) + u64::from(size);
        if end > 1u64 << 32 {
            return Err(BindingError::ObjectExtentOverflow(binding.object_number));
        }
        if let Some(normal) = binding.normal {
            validate_selector(
                binding,
                size,
                object.default_bitness(),
                SelectorView::Normal,
                normal,
            )?;
            selectors.push(SelectorRecord {
                object_number: binding.object_number,
                selector: normal,
            });
        }
        if let Some(alias) = binding.alias {
            validate_selector(
                binding,
                size,
                DefaultBitness::Bits16,
                SelectorView::Alias,
                alias,
            )?;
            selectors.push(SelectorRecord {
                object_number: binding.object_number,
                selector: alias,
            });
        }
        if size != 0 {
            ranges.push(LoadedRange {
                object_number: binding.object_number,
                start: binding.linear_base,
                end,
            });
        }
    }
    ranges.sort_unstable_by_key(|range| (range.start, range.object_number));
    for adjacent in ranges.windows(2) {
        if adjacent[0].end > u64::from(adjacent[1].start) {
            return Err(BindingError::OverlappingObjects {
                first: adjacent[0].object_number,
                second: adjacent[1].object_number,
            });
        }
    }
    selectors.sort_unstable_by_key(|record| (record.selector.value & !3, record.object_number));
    for adjacent in selectors.windows(2) {
        let first = adjacent[0];
        let second = adjacent[1];
        let identity = first.selector.value & !3;
        if identity == second.selector.value & !3
            && (
                first.selector.base,
                first.selector.limit,
                first.selector.default_bitness,
            ) != (
                second.selector.base,
                second.selector.limit,
                second.selector.default_bitness,
            )
        {
            return Err(BindingError::InconsistentSelector {
                identity,
                first_object: first.object_number,
                second_object: second.object_number,
            });
        }
    }
    Ok(())
}

fn validate_selector(
    binding: DosFlatObjectBinding,
    size: u32,
    bitness: DefaultBitness,
    view: SelectorView,
    selector: DosFlatSelector,
) -> Result<(), BindingError> {
    let fail = |problem| BindingError::Selector {
        object_number: binding.object_number,
        view,
        problem,
    };
    if selector.value & !3 == 0 {
        return Err(fail(SelectorProblem::Null));
    }
    if selector.default_bitness != bitness {
        return Err(fail(SelectorProblem::Bitness));
    }
    let expected_base = match bitness {
        DefaultBitness::Bits32 => 0,
        DefaultBitness::Bits16 => binding.linear_base,
    };
    if selector.base != expected_base {
        return Err(fail(SelectorProblem::Base));
    }
    if bitness == DefaultBitness::Bits16 && (size > 0x1_0000 || selector.limit > 0xffff) {
        return Err(fail(SelectorProblem::Window16));
    }
    let origin = match bitness {
        DefaultBitness::Bits32 => binding.linear_base,
        DefaultBitness::Bits16 => 0,
    };
    if size != 0 && origin + (size - 1) > selector.limit {
        // The object's exclusive linear end was checked before this function.
        return Err(fail(SelectorProblem::Coverage));
    }
    Ok(())
}

fn encode_patch(
    image: &SymbolicImage,
    relocation: Relocation<'_>,
    source: DosFlatObjectBinding,
    target: DosFlatObjectBinding,
) -> Result<RelocationPatch, BindingError> {
    let object_offset = usize::try_from(relocation.source.offset)
        .map_err(|_| BindingError::Arithmetic("patch object offset"))?;
    let mut patch = RelocationPatch {
        source: relocation.source,
        kind: relocation.kind,
        bytes: [0; 6],
        byte_len: relocation.kind.width(),
        object_offset,
    };
    match relocation.kind {
        LeFixupSourceType::Offset32 | LeFixupSourceType::SelfRelative32 => {
            if relocation.kind == LeFixupSourceType::SelfRelative32
                && image
                    .object(source.object_number)
                    .unwrap()
                    .default_bitness()
                    == DefaultBitness::Bits16
            {
                return Err(BindingError::UnsupportedCode16SelfRelative {
                    source: relocation.source,
                });
            }
            // Arbitrary u32 target addends, including one-past-object values,
            // are intentional modular field arithmetic, never dereferenced.
            let target_linear = target
                .linear_base
                .wrapping_add(relocation.target.offset.unwrap());
            let value = if relocation.kind == LeFixupSourceType::SelfRelative32 {
                let source_linear = source.linear_base.wrapping_add(relocation.source.offset);
                target_linear.wrapping_sub(source_linear.wrapping_add(4))
            } else {
                target_linear
            };
            patch.bytes[..4].copy_from_slice(&value.to_le_bytes());
        }
        LeFixupSourceType::Selector16 => {
            let selector = if relocation.is_alias {
                target.alias.ok_or(BindingError::MissingAlias {
                    source: relocation.source,
                    target_object: target.object_number,
                })?
            } else {
                target.normal.ok_or(BindingError::MissingNormal {
                    source: relocation.source,
                    target_object: target.object_number,
                })?
            };
            patch.bytes[..2].copy_from_slice(&selector.value.to_le_bytes());
        }
        kind => {
            return Err(BindingError::UnsupportedKind {
                source: relocation.source,
                kind,
            });
        }
    }
    Ok(patch)
}

fn object_index(number: u32) -> Result<usize, BindingError> {
    usize::try_from(
        number
            .checked_sub(1)
            .ok_or(BindingError::UnknownObject(number))?,
    )
    .map_err(|_| BindingError::Arithmetic("object index"))
}

#[cfg(test)]
mod tests;
