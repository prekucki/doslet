//! Conservative, profile-independent traversal of a symbolic image.
//!
//! Only the LE entry and caller-supplied trusted entries establish reachability.
//! Neither preferred linear bases nor relocation pointers seed this graph.
//! Unbound nodes describe original file bytes; bound nodes describe explicitly
//! patched windows. Runtime code/segment validation and an interpreter fallback
//! remain necessary in either case.
//!
//! Reachable x86 instructions may overlap in the source bytes. Each valid start
//! remains a separate node, and verified intersecting pairs are recorded under
//! explicit limits. Future code generation must identify instruction streams by
//! their nodes and edges; it must not assign exclusive ownership of source bytes.
//!
//! In unbound analysis, a 32-bit relative displacement cancels an unknown object
//! origin when its destination stays within that object without wrapping. A
//! 16-bit near transfer truncates the *runtime EIP*, whose origin is unknown here,
//! even in code16.
//! Such targets remain unresolved. Sequential code16 traversal is restricted to
//! the first 64 KiB and never wraps; it describes the contiguous-byte path subject
//! to runtime segment bounds. A future binding must validate that path as well.
//!
//! Explicit bound traversal instead decodes small patched windows under a
//! caller-declared full flat32 CS. Near transfers retain that source CS, including
//! when they reach compatible data32 objects. The borrowed binding is retained:
//! consumers must validate it with `matches_binding` before using another plan.

use alloc::vec::Vec;
use core::{fmt, mem::size_of};
use doslet_executable::LeFixupSourceType;
use iced_x86::{
    ConstantOffsets, Decoder, DecoderError, DecoderOptions, FlowControl, Instruction, Mnemonic,
    OpKind,
};

use crate::{DefaultBitness, ObjectAddress, SymbolicImage, binding::DosFlatBindingPlan};

/// Independent bounds for offline traversal. Exhaustion returns an error, never
/// a successful graph that silently omits queued work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CfgLimits {
    /// Includes unsuccessful decoder attempts, so malformed entries cost work.
    pub max_instructions: usize,
    pub max_edges: usize,
    /// Verified intersecting instruction pairs, not the number of shared bytes.
    pub max_overlaps: usize,
    /// Maximum pending unique addresses, and maximum number of supplied seeds
    /// including the header entry, before deduplication.
    pub max_worklist: usize,
    /// Requested vector capacity, including temporary instruction-start maps and
    /// relocation/binding indexes. Allocator bookkeeping and iced's static tables are
    /// excluded. Reservations are charged before allocation and never refunded.
    pub max_owned_bytes: usize,
}

impl Default for CfgLimits {
    fn default() -> Self {
        Self {
            max_instructions: 1_000_000,
            max_edges: 3_000_000,
            max_overlaps: 1_000_000,
            max_worklist: 1_000_000,
            max_owned_bytes: 256 * 1024 * 1024,
        }
    }
}

/// The decoder is deliberately private to the offline analysis layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstructionFlow {
    Next,
    ConditionalBranch,
    DirectJump,
    DirectCall,
    IndirectJump,
    IndirectCall,
    FarJump,
    FarCall,
    Return,
    InterruptReturn,
    RuntimeService,
    Halt,
    Exception,
    SystemTransfer,
    TransactionalTransfer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstructionNode {
    /// Unique instruction identity within this graph. Other nodes may overlap
    /// its bytes; consumers must follow node addresses rather than byte owners.
    pub address: ObjectAddress,
    pub byte_len: u8,
    /// Default code mode, independent of operand/address overrides and byte ops.
    pub bitness: DefaultBitness,
    pub flow: InstructionFlow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Fallthrough,
    Branch,
    Call,
    /// Possible return to the instruction after a call; does not prove return.
    CallContinuation,
    ServiceContinuation,
    Resume,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StopReason {
    MissingObject,
    NotExecutable,
    OutsideObject,
    NotFileBacked,
    InvalidInstruction,
    TruncatedInstruction,
    /// An instruction's encoding can change when a fixup is applied, including
    /// fixups spanning instruction boundaries. No node is claimed for it.
    RelocatedEncoding,
    /// No instruction is decoded above or across the first code16 IP window.
    Code16Boundary,
    /// Bound traversal currently requires a full flat32 source CS and only
    /// reaches objects with a compatible base-zero default32 view. A target's
    /// normal selector is not loaded by a near transfer.
    UnsupportedCodeContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnresolvedReason {
    Unavailable {
        address: ObjectAddress,
        reason: StopReason,
    },
    /// An object-relative signed result, retained without guessing another
    /// object's mapping from preferred bases or wrapping it modulo 2^32.
    RelativeTargetOutsideObject {
        offset: i64,
    },
    BindingDependent16BitTarget,
    BindingDependent16BitFallthrough,
    RelocatedControlTarget,
    IndirectTarget,
    FarTarget,
    ReturnTarget,
    InterruptReturnTarget,
    RuntimeService,
    Halt,
    Exception,
    SystemTransfer,
    TransactionalTransfer,
    UnsupportedRelativeEncoding,
    OffsetOverflow,
    /// An actual bound near-transfer EIP has no declared loaded object.
    UnmappedLinearTarget {
        linear: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeTarget {
    /// A proven symbolic address, not necessarily a valid instruction. Consult
    /// nodes and diagnostics: decoding can stop at this address.
    Address(ObjectAddress),
    Unresolved(UnresolvedReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CfgEdge {
    pub source: ObjectAddress,
    pub kind: EdgeKind,
    pub target: EdgeTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CfgDiagnostic {
    pub address: ObjectAddress,
    pub reason: StopReason,
}

/// Two independently reachable, valid instructions sharing immutable file bytes.
/// This records a byte-layout fact, not proof of executable runtime semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstructionOverlap {
    /// Lower instruction start address.
    pub first: ObjectAddress,
    /// Higher instruction start address, in the same object as `first`.
    pub second: ObjectAddress,
    /// First shared byte, in the same object as both instructions.
    pub start: ObjectAddress,
    /// Length of the half-open intersection beginning at `start`.
    pub byte_len: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlapPolicy {
    /// Preserve both reachable instructions and record their intersection.
    #[default]
    Record,
    /// Diagnostic gate for consumers that cannot yet model overlapping streams.
    Reject,
}

/// Deterministically ordered instruction graph tied to the exact immutable image
/// it analyzes. Dynamic exits and stopped paths are explicit; success does not
/// imply complete program coverage, CPU support, or game compatibility.
#[derive(Debug)]
pub struct StaticCfg<'image> {
    image: &'image SymbolicImage,
    binding: Option<&'image DosFlatBindingPlan<'image>>,
    entries: Vec<ObjectAddress>,
    nodes: Vec<InstructionNode>,
    edges: Vec<CfgEdge>,
    diagnostics: Vec<CfgDiagnostic>,
    overlaps: Vec<InstructionOverlap>,
}

impl<'image> StaticCfg<'image> {
    /// Traverse the real LE entry plus explicitly trusted additional entries.
    /// No profiles, relocation targets, byte sweeping, or runtime addresses are
    /// accepted as implicit roots. Calls explore both callee and continuation.
    pub fn analyze(
        image: &'image SymbolicImage,
        extra_entries: &[ObjectAddress],
        limits: CfgLimits,
    ) -> Result<Self, CfgError> {
        Self::analyze_with_overlap_policy(image, extra_entries, limits, OverlapPolicy::Record)
    }

    /// Apply an explicit overlap policy. Rejection concerns pairs of validated
    /// nodes only: an invalid or truncated interior entry remains a stopped path.
    pub fn analyze_with_overlap_policy(
        image: &'image SymbolicImage,
        extra_entries: &[ObjectAddress],
        limits: CfgLimits,
        policy: OverlapPolicy,
    ) -> Result<Self, CfgError> {
        Builder::new(image, extra_entries, limits, policy)?.build()
    }

    /// Traverse relocated instructions under an explicitly declared full flat32
    /// source CS (base zero, default32, limit `u32::MAX`). Each legal root uses
    /// its executable object's normal view; near paths retain that context and
    /// may reach file-backed data32 objects without loading their selectors.
    /// Other source contexts and incompatible target objects stop explicitly.
    /// Relocation records and their pointer values never establish roots.
    pub fn analyze_bound(
        binding: &'image DosFlatBindingPlan<'image>,
        extra_entries: &[ObjectAddress],
        limits: CfgLimits,
    ) -> Result<Self, CfgError> {
        Self::analyze_bound_with_overlap_policy(
            binding,
            extra_entries,
            limits,
            OverlapPolicy::Record,
        )
    }

    pub fn analyze_bound_with_overlap_policy(
        binding: &'image DosFlatBindingPlan<'image>,
        extra_entries: &[ObjectAddress],
        limits: CfgLimits,
        policy: OverlapPolicy,
    ) -> Result<Self, CfgError> {
        Builder::new_with_binding(
            binding.image(),
            Some(binding),
            extra_entries,
            limits,
            policy,
        )?
        .build()
    }

    pub fn image(&self) -> &'image SymbolicImage {
        self.image
    }
    pub fn binding(&self) -> Option<&'image DosFlatBindingPlan<'image>> {
        self.binding
    }
    /// Unbound graphs accept layouts of the exact same immutable image. Bound
    /// graphs require the exact immutably borrowed plan used for discovery;
    /// equal source images alone do not prove equal relocated control flow.
    pub fn matches_binding(&self, binding: &DosFlatBindingPlan<'_>) -> bool {
        core::ptr::eq(self.image, binding.image())
            && self
                .binding
                .is_none_or(|expected| core::ptr::eq(expected, binding))
    }
    pub fn entries(&self) -> &[ObjectAddress] {
        &self.entries
    }
    pub fn nodes(&self) -> &[InstructionNode] {
        &self.nodes
    }
    pub fn edges(&self) -> &[CfgEdge] {
        &self.edges
    }
    pub fn diagnostics(&self) -> &[CfgDiagnostic] {
        &self.diagnostics
    }
    pub fn overlaps(&self) -> &[InstructionOverlap] {
        &self.overlaps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfgStorage {
    Entries,
    Objects,
    ByteStates,
    RelocationIndex,
    BindingIndex,
    Worklist,
    Nodes,
    Edges,
    Diagnostics,
    Overlaps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfgResource {
    Instructions,
    Edges,
    Worklist,
    Overlaps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfgError {
    MissingEntry,
    Limit(CfgResource),
    OwnedByteLimit(CfgStorage),
    Allocation(CfgStorage),
    Arithmetic(&'static str),
    /// Two valid reachable instructions overlap under `OverlapPolicy::Reject`.
    OverlappingInstructions {
        instruction: ObjectAddress,
        other: ObjectAddress,
        byte: ObjectAddress,
    },
}

impl fmt::Display for CfgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEntry => f.write_str("static CFG has no LE entry or trusted extra entry"),
            Self::Limit(resource) => write!(f, "static CFG {resource:?} limit"),
            Self::OwnedByteLimit(storage) => write!(f, "static CFG owned-byte limit: {storage:?}"),
            Self::Allocation(storage) => write!(f, "cannot reserve static CFG {storage:?}"),
            Self::Arithmetic(field) => write!(f, "static CFG arithmetic overflow: {field}"),
            Self::OverlappingInstructions {
                instruction,
                other,
                byte,
            } => write!(
                f,
                "overlapping instructions {instruction:?} and {other:?} at {byte:?}"
            ),
        }
    }
}

impl core::error::Error for CfgError {}

const UNSEEN: u32 = 0;
const QUEUED: u32 = u32::MAX;
const STOPPED: u32 = u32::MAX - 1;
const UNBACKED: u32 = u32::MAX - 2;
const MAX_INSTRUCTION_BYTES: usize = 15;

struct ObjectState {
    // Per-byte backing and start state only. A node ID never claims the rest of
    // its instruction's bytes, which may contain other reachable start states.
    starts: Vec<u32>,
}

#[derive(Clone, Copy)]
struct FixupRange {
    object: u32,
    start: u32,
    end: u32,
}

struct BoundRange {
    object: u32,
    start: u32,
    end: u64,
}

struct Builder<'image> {
    graph: StaticCfg<'image>,
    limits: CfgLimits,
    budget: Budget,
    objects: Vec<ObjectState>,
    fixups: Vec<FixupRange>,
    bound_ranges: Vec<BoundRange>,
    worklist: Vec<ObjectAddress>,
    decode_attempts: usize,
    overlap_policy: OverlapPolicy,
}

impl<'image> Builder<'image> {
    fn new(
        image: &'image SymbolicImage,
        extra: &[ObjectAddress],
        limits: CfgLimits,
        overlap_policy: OverlapPolicy,
    ) -> Result<Self, CfgError> {
        Self::new_with_binding(image, None, extra, limits, overlap_policy)
    }

    fn new_with_binding(
        image: &'image SymbolicImage,
        binding: Option<&'image DosFlatBindingPlan<'image>>,
        extra: &[ObjectAddress],
        limits: CfgLimits,
        overlap_policy: OverlapPolicy,
    ) -> Result<Self, CfgError> {
        let seed_count = extra
            .len()
            .checked_add(usize::from(image.header().entry.is_some()))
            .ok_or(CfgError::Arithmetic("entry count"))?;
        if seed_count == 0 {
            return Err(CfgError::MissingEntry);
        }
        if seed_count > limits.max_worklist {
            return Err(CfgError::Limit(CfgResource::Worklist));
        }
        let mut budget = Budget(limits.max_owned_bytes);
        let mut entries = Vec::new();
        budget.reserve(&mut entries, seed_count, CfgStorage::Entries)?;
        if let Some(entry) = image.header().entry {
            entries.push(ObjectAddress {
                object_number: entry.object_number,
                offset: entry.offset,
            });
        }
        entries.extend_from_slice(extra);
        entries.sort_unstable();
        entries.dedup();
        let mut objects = Vec::new();
        budget.reserve(&mut objects, image.objects().len(), CfgStorage::Objects)?;
        for object in image.objects() {
            let mut starts = Vec::new();
            if object.metadata().is_executable()
                || (binding.is_some() && object.default_bitness() == DefaultBitness::Bits32)
            {
                budget.reserve(&mut starts, object.bytes().len(), CfgStorage::ByteStates)?;
                starts.resize(object.bytes().len(), UNBACKED);
                for span in object.file_backed_spans() {
                    let start = host(span.object_offset)?;
                    let end = start
                        .checked_add(host(span.byte_len)?)
                        .ok_or(CfgError::Arithmetic("backed span"))?;
                    starts
                        .get_mut(start..end)
                        .ok_or(CfgError::Arithmetic("backed span"))?
                        .fill(UNSEEN);
                }
            }
            objects.push(ObjectState { starts });
        }
        let mut fixups = Vec::new();
        budget.reserve(
            &mut fixups,
            image.relocations().len(),
            CfgStorage::RelocationIndex,
        )?;
        for relocation in image.relocations() {
            fixups.push(FixupRange {
                object: relocation.source.object_number,
                start: relocation.source.offset,
                end: relocation
                    .source
                    .offset
                    .checked_add(u32::from(relocation.kind.width()))
                    .ok_or(CfgError::Arithmetic("relocation end"))?,
            });
        }
        let mut bound_ranges = Vec::new();
        if let Some(binding) = binding {
            budget.reserve(
                &mut bound_ranges,
                image.objects().len(),
                CfgStorage::BindingIndex,
            )?;
            for object in image.objects() {
                if object.metadata().virtual_size == 0 {
                    continue;
                }
                let start = binding
                    .object_binding(object.object_number())
                    .ok_or(CfgError::Arithmetic("bound object"))?
                    .linear_base;
                bound_ranges.push(BoundRange {
                    object: object.object_number(),
                    start,
                    end: u64::from(start) + u64::from(object.metadata().virtual_size),
                });
            }
            bound_ranges.sort_unstable_by_key(|range| range.start);
        }
        Ok(Self {
            graph: StaticCfg {
                image,
                binding,
                entries,
                nodes: Vec::new(),
                edges: Vec::new(),
                diagnostics: Vec::new(),
                overlaps: Vec::new(),
            },
            limits,
            budget,
            objects,
            fixups,
            bound_ranges,
            worklist: Vec::new(),
            decode_attempts: 0,
            overlap_policy,
        })
    }

    fn build(mut self) -> Result<StaticCfg<'image>, CfgError> {
        // Reverse insertion visits the lowest root first on the DFS stack.
        for index in (0..self.graph.entries.len()).rev() {
            let address = self.graph.entries[index];
            if let Some(reason) = self.root_unavailable(address) {
                self.diagnostic(address, reason)?;
            } else {
                self.schedule(address)?;
            }
        }
        while let Some(address) = self.worklist.pop() {
            self.decode(address)?;
        }
        self.graph.nodes.sort_unstable_by_key(|node| node.address);
        self.graph.edges.sort_unstable();
        self.graph.diagnostics.sort_unstable();
        self.graph.overlaps.sort_unstable();
        Ok(self.graph)
    }

    fn unavailable(&self, address: ObjectAddress) -> Option<StopReason> {
        let Some(object) = self.graph.image.object(address.object_number) else {
            return Some(StopReason::MissingObject);
        };
        if self.graph.binding.is_some() {
            if object.default_bitness() != DefaultBitness::Bits32 {
                return Some(StopReason::UnsupportedCodeContext);
            }
        } else if !object.metadata().is_executable() {
            return Some(StopReason::NotExecutable);
        }
        if address.offset >= object.metadata().virtual_size {
            return Some(StopReason::OutsideObject);
        }
        let index = usize::try_from(address.object_number - 1).ok()?;
        let offset = usize::try_from(address.offset).ok()?;
        if self.objects[index].starts[offset] == UNBACKED {
            return Some(StopReason::NotFileBacked);
        }
        None
    }

    fn root_unavailable(&self, address: ObjectAddress) -> Option<StopReason> {
        if let Some(binding) = self.graph.binding {
            let Some(object) = self.graph.image.object(address.object_number) else {
                return Some(StopReason::MissingObject);
            };
            if !object.metadata().is_executable() {
                return Some(StopReason::NotExecutable);
            }
            let Some(normal) = binding.object_binding(address.object_number)?.normal else {
                return Some(StopReason::UnsupportedCodeContext);
            };
            if normal.base != 0
                || normal.default_bitness != DefaultBitness::Bits32
                || normal.limit != u32::MAX
            {
                return Some(StopReason::UnsupportedCodeContext);
            }
        }
        self.unavailable(address)
    }

    fn schedule(&mut self, address: ObjectAddress) -> Result<(), CfgError> {
        let index = host(
            address
                .object_number
                .checked_sub(1)
                .ok_or(CfgError::Arithmetic("object number"))?,
        )?;
        let offset = host(address.offset)?;
        match self.objects[index].starts[offset] {
            UNSEEN => {
                if self.worklist.len() >= self.limits.max_worklist {
                    return Err(CfgError::Limit(CfgResource::Worklist));
                }
                self.budget.push(
                    &mut self.worklist,
                    address,
                    self.limits.max_worklist,
                    CfgStorage::Worklist,
                )?;
                self.objects[index].starts[offset] = QUEUED;
            }
            QUEUED | STOPPED => {}
            UNBACKED => return Err(CfgError::Arithmetic("scheduled unbacked byte")),
            // This exact start is already a node, even when its bytes overlap
            // other nodes. Queued, stopped and decoded starts are never retried.
            _ => {}
        }
        Ok(())
    }

    fn diagnostic(&mut self, address: ObjectAddress, reason: StopReason) -> Result<(), CfgError> {
        self.budget.push(
            &mut self.graph.diagnostics,
            CfgDiagnostic { address, reason },
            usize::MAX,
            CfgStorage::Diagnostics,
        )
    }

    fn stop(&mut self, address: ObjectAddress, reason: StopReason) -> Result<(), CfgError> {
        self.objects[host(address.object_number - 1)?].starts[host(address.offset)?] = STOPPED;
        self.diagnostic(address, reason)
    }

    fn decode(&mut self, address: ObjectAddress) -> Result<(), CfgError> {
        if self.decode_attempts >= self.limits.max_instructions {
            return Err(CfgError::Limit(CfgResource::Instructions));
        }
        self.decode_attempts += 1;
        let index = host(address.object_number - 1)?;
        let start = host(address.offset)?;
        let object = &self.graph.image.objects()[index];
        let bitness = object.default_bitness();
        if bitness == DefaultBitness::Bits16 && address.offset >= 0x1_0000 {
            return self.stop(address, StopReason::Code16Boundary);
        }
        // Per-byte backing makes adjacent object pages contiguous even when
        // their physical file pages are reordered. At most 15 bytes are read.
        let available = self.objects[index].starts[start..]
            .iter()
            .take(MAX_INSTRUCTION_BYTES)
            .take_while(|&&owner| owner != UNBACKED)
            .count();
        let bytes = &object.bytes()[start..start + available];
        let ip = self.graph.binding.map_or(address.offset, |binding| {
            binding
                .object_binding(address.object_number)
                .expect("validated binding contains every object")
                .linear_base
                .wrapping_add(address.offset)
        });
        let mut decoder = Decoder::with_ip(
            match bitness {
                DefaultBitness::Bits16 => 16,
                DefaultBitness::Bits32 => 32,
            },
            bytes,
            u64::from(ip),
            DecoderOptions::NONE,
        );
        let mut instruction = decoder.decode();
        if instruction.is_invalid() {
            return self.stop(
                address,
                if decoder.last_error() == DecoderError::NoMoreBytes {
                    StopReason::TruncatedInstruction
                } else {
                    StopReason::InvalidInstruction
                },
            );
        }
        let len = instruction.len();
        let end = start
            .checked_add(len)
            .ok_or(CfgError::Arithmetic("instruction end"))?;
        let end_offset = address
            .offset
            .checked_add(
                u32::try_from(len).map_err(|_| CfgError::Arithmetic("instruction length"))?,
            )
            .ok_or(CfgError::Arithmetic("instruction end"))?;
        if bitness == DefaultBitness::Bits16 && end_offset > 0x1_0000 {
            return self.stop(address, StopReason::Code16Boundary);
        }
        let constants = decoder.get_constant_offsets(&instruction);
        let (encoding_relocated, control_relocated) =
            self.relocation_effect(address, end_offset, constants);
        if encoding_relocated {
            return self.stop(address, StopReason::RelocatedEncoding);
        }
        let mut window = [0; MAX_INSTRUCTION_BYTES];
        let bytes = if let Some(binding) = self.graph.binding {
            if !bound_relocations_valid(binding, address, end_offset, &instruction, constants) {
                return self.stop(address, StopReason::RelocatedEncoding);
            }
            window[..available].copy_from_slice(bytes);
            patch_window(binding, address, &mut window[..available]);
            let mut relocated = Decoder::with_ip(
                32,
                &window[..available],
                u64::from(ip),
                DecoderOptions::NONE,
            );
            let patched = relocated.decode();
            if patched.is_invalid()
                || patched.code() != instruction.code()
                || patched.len() != instruction.len()
            {
                return self.stop(address, StopReason::RelocatedEncoding);
            }
            instruction = patched;
            &window[..available]
        } else {
            bytes
        };
        self.record_overlaps(address, end)?;
        let flow = classify(&instruction);
        let owner = u32::try_from(self.graph.nodes.len())
            .ok()
            .and_then(|value| value.checked_add(1))
            .filter(|&value| value < UNBACKED)
            .ok_or(CfgError::Arithmetic("instruction start ID"))?;
        self.budget.push(
            &mut self.graph.nodes,
            InstructionNode {
                address,
                byte_len: len as u8,
                bitness,
                flow,
            },
            self.limits.max_instructions,
            CfgStorage::Nodes,
        )?;
        self.objects[index].starts[start] = owner;
        self.successors(
            address,
            &instruction,
            constants,
            bytes,
            flow,
            control_relocated,
        )
    }

    fn record_overlaps(&mut self, address: ObjectAddress, end: usize) -> Result<(), CfgError> {
        let index = host(address.object_number - 1)?;
        let start = host(address.offset)?;
        // An x86 instruction is at most 15 bytes. Only starts in this bounded
        // window can intersect the new instruction, regardless of decode order.
        // Every pair is encountered once, when its second node is decoded.
        for offset in start.saturating_sub(MAX_INSTRUCTION_BYTES - 1)..end {
            let owner = self.objects[index].starts[offset];
            if owner == UNSEEN || owner >= UNBACKED {
                continue;
            }
            let other = self.graph.nodes[host(owner - 1)?];
            let other_end = offset + usize::from(other.byte_len);
            let shared_start = start.max(offset);
            let shared_end = end.min(other_end);
            if shared_start >= shared_end {
                continue;
            }
            let intersection = ObjectAddress {
                offset: shared_start as u32,
                ..address
            };
            if self.overlap_policy == OverlapPolicy::Reject {
                return Err(CfgError::OverlappingInstructions {
                    instruction: address,
                    other: other.address,
                    byte: intersection,
                });
            }
            if self.graph.overlaps.len() >= self.limits.max_overlaps {
                return Err(CfgError::Limit(CfgResource::Overlaps));
            }
            self.budget.push(
                &mut self.graph.overlaps,
                InstructionOverlap {
                    first: address.min(other.address),
                    second: address.max(other.address),
                    start: intersection,
                    byte_len: (shared_end - shared_start) as u8,
                },
                self.limits.max_overlaps,
                CfgStorage::Overlaps,
            )?;
        }
        Ok(())
    }

    fn relocation_effect(
        &self,
        address: ObjectAddress,
        end: u32,
        constants: ConstantOffsets,
    ) -> (bool, bool) {
        let first = self.fixups.partition_point(|fixup| {
            fixup.object < address.object_number
                || (fixup.object == address.object_number && fixup.end <= address.offset)
        });
        let mut constant_bytes = [false; 15];
        for (offset, size) in [
            (
                constants.displacement_offset(),
                constants.displacement_size(),
            ),
            (constants.immediate_offset(), constants.immediate_size()),
            (constants.immediate_offset2(), constants.immediate_size2()),
        ] {
            for byte in constant_bytes.iter_mut().skip(offset).take(size) {
                *byte = true;
            }
        }
        let mut relocated = false;
        for fixup in self.fixups[first..]
            .iter()
            .take_while(|fixup| fixup.object == address.object_number && fixup.start < end)
        {
            relocated = true;
            if fixup.start < address.offset || fixup.end > end {
                return (true, true);
            }
            for byte in fixup.start..fixup.end {
                if !constant_bytes[(byte - address.offset) as usize] {
                    return (true, true);
                }
            }
        }
        (false, relocated)
    }

    fn successors(
        &mut self,
        address: ObjectAddress,
        instruction: &Instruction,
        constants: ConstantOffsets,
        bytes: &[u8],
        flow: InstructionFlow,
        control_relocated: bool,
    ) -> Result<(), CfgError> {
        use InstructionFlow as F;
        use UnresolvedReason as U;
        let target = match flow {
            F::ConditionalBranch | F::DirectJump | F::DirectCall => Some(self.relative_target(
                address,
                instruction,
                constants,
                bytes,
                control_relocated,
            )),
            F::IndirectJump | F::IndirectCall => Some(EdgeTarget::Unresolved(U::IndirectTarget)),
            F::FarJump | F::FarCall => Some(EdgeTarget::Unresolved(U::FarTarget)),
            _ => None,
        };
        if let Some(target) = target {
            let kind = if matches!(flow, F::DirectCall | F::IndirectCall | F::FarCall) {
                EdgeKind::Call
            } else {
                EdgeKind::Branch
            };
            self.edge(address, kind, target)?;
        }
        let exit = match flow {
            F::Return => Some(U::ReturnTarget),
            F::InterruptReturn => Some(U::InterruptReturnTarget),
            F::RuntimeService => Some(U::RuntimeService),
            F::Halt => Some(U::Halt),
            F::Exception => Some(U::Exception),
            F::SystemTransfer => Some(U::SystemTransfer),
            F::TransactionalTransfer => Some(U::TransactionalTransfer),
            _ => None,
        };
        if let Some(reason) = exit {
            self.edge(address, EdgeKind::Exit, EdgeTarget::Unresolved(reason))?;
        }
        let continuation = match flow {
            F::Next | F::ConditionalBranch => Some(EdgeKind::Fallthrough),
            F::DirectCall | F::IndirectCall | F::FarCall => Some(EdgeKind::CallContinuation),
            F::RuntimeService => Some(EdgeKind::ServiceContinuation),
            F::Halt => Some(EdgeKind::Resume),
            _ => None,
        };
        if let Some(kind) = continuation {
            let target = if self.graph.binding.is_some() {
                self.linear_target(instruction.next_ip32())
            } else {
                match address.offset.checked_add(instruction.len() as u32) {
                    Some(offset)
                        if instruction.code_size() == iced_x86::CodeSize::Code16
                            && offset >= 0x1_0000 =>
                    {
                        EdgeTarget::Unresolved(U::BindingDependent16BitFallthrough)
                    }
                    Some(offset) => self.address_target(ObjectAddress { offset, ..address }),
                    None => EdgeTarget::Unresolved(U::OffsetOverflow),
                }
            };
            self.edge(address, kind, target)?;
        }
        Ok(())
    }

    fn relative_target(
        &self,
        address: ObjectAddress,
        instruction: &Instruction,
        constants: ConstantOffsets,
        bytes: &[u8],
        relocated: bool,
    ) -> EdgeTarget {
        use UnresolvedReason as U;
        if self.graph.binding.is_some() {
            return self.bound_relative_target(instruction);
        }
        if relocated {
            return EdgeTarget::Unresolved(U::RelocatedControlTarget);
        }
        match instruction.op0_kind() {
            OpKind::NearBranch16 => return EdgeTarget::Unresolved(U::BindingDependent16BitTarget),
            OpKind::NearBranch32 => {}
            _ => return EdgeTarget::Unresolved(U::UnsupportedRelativeEncoding),
        }
        let start = constants.immediate_offset();
        let displacement = match bytes.get(start..start + constants.immediate_size()) {
            Some(&[value]) => i64::from(value as i8),
            Some(&[a, b]) => i64::from(i16::from_le_bytes([a, b])),
            Some(&[a, b, c, d]) => i64::from(i32::from_le_bytes([a, b, c, d])),
            _ => return EdgeTarget::Unresolved(U::UnsupportedRelativeEncoding),
        };
        let offset = i64::from(address.offset) + instruction.len() as i64 + displacement;
        let size = self.graph.image.objects()[address.object_number as usize - 1]
            .metadata()
            .virtual_size;
        if offset < 0 || offset >= i64::from(size) {
            return EdgeTarget::Unresolved(U::RelativeTargetOutsideObject { offset });
        }
        self.address_target(ObjectAddress {
            offset: offset as u32,
            ..address
        })
    }

    fn bound_relative_target(&self, instruction: &Instruction) -> EdgeTarget {
        match instruction.op0_kind() {
            OpKind::NearBranch32 => self.linear_target(instruction.near_branch32()),
            OpKind::NearBranch16 => {
                EdgeTarget::Unresolved(UnresolvedReason::BindingDependent16BitTarget)
            }
            _ => EdgeTarget::Unresolved(UnresolvedReason::UnsupportedRelativeEncoding),
        }
    }

    fn linear_target(&self, linear: u32) -> EdgeTarget {
        let range = self
            .bound_ranges
            .partition_point(|range| range.start <= linear)
            .checked_sub(1)
            .and_then(|index| self.bound_ranges.get(index))
            .filter(|range| u64::from(linear) < range.end);
        match range {
            Some(range) => self.address_target(ObjectAddress {
                object_number: range.object,
                offset: linear - range.start,
            }),
            None => EdgeTarget::Unresolved(UnresolvedReason::UnmappedLinearTarget { linear }),
        }
    }

    fn address_target(&self, address: ObjectAddress) -> EdgeTarget {
        match self.unavailable(address) {
            Some(reason) => {
                EdgeTarget::Unresolved(UnresolvedReason::Unavailable { address, reason })
            }
            None => EdgeTarget::Address(address),
        }
    }

    fn edge(
        &mut self,
        source: ObjectAddress,
        kind: EdgeKind,
        target: EdgeTarget,
    ) -> Result<(), CfgError> {
        if self.graph.edges.len() >= self.limits.max_edges {
            return Err(CfgError::Limit(CfgResource::Edges));
        }
        self.budget.push(
            &mut self.graph.edges,
            CfgEdge {
                source,
                kind,
                target,
            },
            self.limits.max_edges,
            CfgStorage::Edges,
        )?;
        if let EdgeTarget::Address(address) = target {
            self.schedule(address)?;
        }
        Ok(())
    }
}

/// Bound decoding accepts complete scalar fields only. A loader may patch any
/// bytes, but it does not establish trustworthy instruction boundaries. In
/// particular, a partial immediate, opcode mutation, or fixup crossing an
/// independently reached instruction start cannot manufacture a trusted edge.
fn bound_relocations_valid(
    binding: &DosFlatBindingPlan<'_>,
    address: ObjectAddress,
    end: u32,
    instruction: &Instruction,
    constants: ConstantOffsets,
) -> bool {
    let patches = binding
        .object_patches(address.object_number)
        .expect("validated binding contains every object");
    let first = patches.partition_point(|patch| {
        u64::from(patch.source.offset) + patch.bytes().len() as u64 <= u64::from(address.offset)
    });
    for patch in patches[first..]
        .iter()
        .take_while(|patch| patch.source.offset < end)
    {
        if patch.source.offset < address.offset
            || u64::from(patch.source.offset) + patch.bytes().len() as u64 > u64::from(end)
        {
            return false;
        }
        let offset = (patch.source.offset - address.offset) as usize;
        let fields = [
            (
                constants.displacement_offset(),
                constants.displacement_size(),
            ),
            (constants.immediate_offset(), constants.immediate_size()),
            (constants.immediate_offset2(), constants.immediate_size2()),
        ];
        if !fields.contains(&(offset, patch.bytes().len())) {
            return false;
        }
        if matches!(
            classify(instruction),
            InstructionFlow::DirectCall
                | InstructionFlow::DirectJump
                | InstructionFlow::ConditionalBranch
        ) && (instruction.op0_kind() != OpKind::NearBranch32
            || patch.kind != LeFixupSourceType::SelfRelative32
            || offset != constants.immediate_offset()
            || constants.immediate_size() != 4)
        {
            return false;
        }
    }
    true
}

fn patch_window(binding: &DosFlatBindingPlan<'_>, address: ObjectAddress, window: &mut [u8]) {
    let start = u64::from(address.offset);
    let end = start + window.len() as u64;
    let patches = binding
        .object_patches(address.object_number)
        .expect("validated binding contains every object");
    let first = patches.partition_point(|patch| {
        u64::from(patch.source.offset) + patch.bytes().len() as u64 <= start
    });
    for patch in patches[first..]
        .iter()
        .take_while(|patch| u64::from(patch.source.offset) < end)
    {
        for (index, &byte) in patch.bytes().iter().enumerate() {
            let at = u64::from(patch.source.offset) + index as u64;
            if at >= start && at < end {
                window[(at - start) as usize] = byte;
            }
        }
    }
}

fn classify(instruction: &Instruction) -> InstructionFlow {
    use InstructionFlow as F;
    // iced groups XBEGIN separately, but XABORT can transfer to the runtime
    // transaction's saved destination and XEND also needs transaction state.
    if matches!(
        instruction.mnemonic(),
        Mnemonic::Xbegin | Mnemonic::Xabort | Mnemonic::Xend
    ) {
        return F::TransactionalTransfer;
    }
    match instruction.flow_control() {
        FlowControl::Next if instruction.mnemonic() == Mnemonic::Hlt => F::Halt,
        FlowControl::Next => F::Next,
        FlowControl::ConditionalBranch => F::ConditionalBranch,
        FlowControl::UnconditionalBranch if instruction.is_jmp_short_or_near() => F::DirectJump,
        FlowControl::UnconditionalBranch if instruction.is_jmp_far() => F::FarJump,
        FlowControl::IndirectBranch if instruction.is_jmp_far_indirect() => F::FarJump,
        FlowControl::IndirectBranch => F::IndirectJump,
        FlowControl::Call if instruction.is_call_near() => F::DirectCall,
        FlowControl::Call if instruction.is_call_far() => F::FarCall,
        FlowControl::IndirectCall if instruction.is_call_far_indirect() => F::FarCall,
        FlowControl::IndirectCall => F::IndirectCall,
        FlowControl::Return
            if matches!(
                instruction.mnemonic(),
                Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq
            ) =>
        {
            F::InterruptReturn
        }
        FlowControl::Return if matches!(instruction.mnemonic(), Mnemonic::Ret | Mnemonic::Retf) => {
            F::Return
        }
        FlowControl::Interrupt => F::RuntimeService,
        FlowControl::Exception => F::Exception,
        FlowControl::XbeginXabortXend => F::TransactionalTransfer,
        _ => F::SystemTransfer,
    }
}

struct Budget(usize);

impl Budget {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        capacity: usize,
        storage: CfgStorage,
    ) -> Result<(), CfgError> {
        let additional = capacity.saturating_sub(values.capacity());
        let bytes = additional
            .checked_mul(size_of::<T>())
            .ok_or(CfgError::OwnedByteLimit(storage))?;
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(CfgError::OwnedByteLimit(storage))?;
        if additional != 0 {
            values
                .try_reserve_exact(capacity - values.len())
                .map_err(|_| CfgError::Allocation(storage))?;
        }
        Ok(())
    }

    fn push<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
        limit: usize,
        storage: CfgStorage,
    ) -> Result<(), CfgError> {
        if values.len() == values.capacity() {
            let needed = values
                .len()
                .checked_add(1)
                .ok_or(CfgError::Arithmetic("vector length"))?;
            let desired = values.capacity().saturating_mul(2).max(16).min(limit);
            let affordable = self.0 / size_of::<T>();
            let capacity = desired.min(values.capacity().saturating_add(affordable));
            if capacity < needed {
                return Err(CfgError::OwnedByteLimit(storage));
            }
            self.reserve(values, capacity, storage)?;
        }
        values.push(value);
        Ok(())
    }
}

fn host(value: u32) -> Result<usize, CfgError> {
    usize::try_from(value).map_err(|_| CfgError::Arithmetic("host size"))
}

#[cfg(test)]
#[path = "cfg/bound_tests.rs"]
mod bound_tests;
#[cfg(test)]
mod tests;
