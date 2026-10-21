//! Single-instruction cache. Unsupported forms use the original interpreter.
pub mod analysis;
pub mod translation;
use super::{Cpu, CpuError, StepOutcome};
use crate::state::SegmentWindow;
use crate::{
    Access, Gpr16, Gpr32, Registers, SegmentCache, SegmentRegister, decode::Operand, flags,
};
use alloc::boxed::Box;
use doslet_bus::{CodeCheck, CpuBus};

#[path = "prepared.rs"]
mod prepared;
use prepared::Prepared;
#[path = "x87_batch.rs"]
mod x87_batch;
use x87_batch::X87;

const CAPACITY: usize = 4096;
const NO_REGISTER: u8 = 8;

#[cfg(feature = "profiling")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeCacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Resident entries rejected after a write, mapping or CS-context change.
    pub invalidations: u64,
    pub fallbacks: u64,
    /// Transitions between observed CS contexts (not entries).
    pub context_switches: u64,
    /// Resident entries discarded by a context transition.
    pub context_invalidations: u64,
    /// Mutually exclusive reasons for a matching entry's stale code check.
    pub write_invalidations: u64,
    pub mapping_invalidations: u64,
    pub owner_invalidations: u64,
    pub unavailable_invalidations: u64,
    pub segment_invalidations: u64,
    /// Lookups finding an occupied slot with a different instruction key.
    pub collisions: u64,
    /// Occupied slots replaced by another key after successful decoding.
    pub evictions: u64,
    pub decode_attempts: u64,
    pub failed_decodes: u64,
    pub fallback_hits: u64,
    /// Interpreter batches without the native core, and the steps they ran.
    pub batches: u64,
    pub batched_steps: u64,
    /// Validated basic-block lookups, and blocks decoded on a miss.
    pub block_hits: u64,
    pub block_misses: u64,
    /// Block misses whose slot held another address, or the same address with
    /// changed code generations.
    pub block_conflicts: u64,
    pub block_stale: u64,
    /// Single-instruction lookups in code whose CS is not default-32-bit.
    pub code16_lookups: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Context {
    cs: SegmentCache,
    selector: u16,
    protected: bool,
}
#[derive(Clone, Copy)]
struct Entry {
    address: u32,
    code32: bool,
    check: CodeCheck,
    instruction: Instruction,
}

const BLOCK_CAPACITY: usize = 4096;
pub(super) const BLOCK_INSTRUCTIONS: usize = 8;

/// Consecutive decoded instructions up to the first control transfer, validated
/// once per lookup. A block starting with an instruction that cannot be batched
/// has `count == 0` and remembers that rejection against the same code check.
#[derive(Clone, Copy)]
struct Block {
    /// Linear start address; `u32::MAX`, which is never RAM, marks a free slot.
    address: u32,
    /// Bytes covered by `check` and by the CS window test of a hit.
    length: u8,
    code32: bool,
    count: u8,
    /// Membership of the linear start in the setup-time flat-32 entry set.
    /// This occupies existing header padding, keeping hot blocks compact.
    prepared_entry: bool,
    check: CodeCheck,
    instructions: [Prepared; BLOCK_INSTRUCTIONS],
}
impl Block {
    const EMPTY: Self = Self {
        address: u32::MAX,
        length: 0,
        count: 0,
        code32: false,
        prepared_entry: false,
        check: CodeCheck {
            space: 0,
            pages: [0; 2],
            generations: [0; 2],
        },
        instructions: [Prepared::EMPTY; BLOCK_INSTRUCTIONS],
    };
}

const ENTRY_HINT_CAPACITY: usize = 512;
const MAX_BLOCK_BYTES: usize = 128;
const VREG_CAPACITY: usize = 128;
const VREG_PROBE: usize = 8;

/// A self-modified 32-bit immediate or displacement slot in RAM mapped as a
/// virtual register bound to a cached basic-block instruction operand.
#[derive(Clone, Copy)]
struct VirtualRegister {
    address: u32,
    value: u32,
    block_index: u16,
    insn_pos: u8,
    is_displacement: bool,
    template_offset: u8,
}
impl VirtualRegister {
    const EMPTY: Self = Self {
        address: u32::MAX,
        value: 0,
        block_index: 0,
        insn_pos: 0,
        is_displacement: false,
        template_offset: 0,
    };
}

/// Cold byte template and 32-bit operand-slot offsets for a cached basic block.
/// Kept off the hot `Block` table so `size_of::<Block>() == 168` stays intact.
#[derive(Clone, Copy)]
struct BlockTemplate {
    bytes: [u8; MAX_BLOCK_BYTES],
    insn_offset: [u8; BLOCK_INSTRUCTIONS],
    disp32_offset: [u8; BLOCK_INSTRUCTIONS],
    imm32_offset: [u8; BLOCK_INSTRUCTIONS],
    disp_vreg: [u8; BLOCK_INSTRUCTIONS],
    imm_vreg: [u8; BLOCK_INSTRUCTIONS],
}
impl BlockTemplate {
    const EMPTY: Self = Self {
        bytes: [0; MAX_BLOCK_BYTES],
        insn_offset: [0; BLOCK_INSTRUCTIONS],
        disp32_offset: [u8::MAX; BLOCK_INSTRUCTIONS],
        imm32_offset: [u8::MAX; BLOCK_INSTRUCTIONS],
        disp_vreg: [u8::MAX; BLOCK_INSTRUCTIONS],
        imm_vreg: [u8::MAX; BLOCK_INSTRUCTIONS],
    };
}

#[derive(Clone, Copy)]
struct EntryHint {
    ip: u32,
    /// The two largest values mark an unused slot and cached non-membership.
    index: u32,
}

/// Immutable registry and replaceable membership hints, allocated only when a
/// machine installs entries. Hints depend on EIP membership, never code bytes.
struct PreparedEntries {
    ips: Box<[u32]>,
    hints: Box<[EntryHint; ENTRY_HINT_CAPACITY]>,
}
impl PreparedEntries {
    #[inline]
    fn index(&mut self, ip: u32) -> Option<usize> {
        let slot = ((ip ^ (ip >> 9)) as usize) & (ENTRY_HINT_CAPACITY - 1);
        let hint = &mut self.hints[slot];
        if hint.index == u32::MAX || hint.ip != ip {
            *hint = EntryHint {
                ip,
                index: self
                    .ips
                    .binary_search(&ip)
                    .map_or(u32::MAX - 1, |index| index as u32),
            };
        }
        (hint.index != u32::MAX - 1).then_some(hint.index as usize)
    }
}

pub(super) struct Cache {
    /// Reserved once at setup; the fixed length lets masked indexing skip
    /// bounds checks without placing the table on the stack.
    entries: Box<[Option<Entry>; CAPACITY]>,
    /// Basic blocks for batched execution, reserved once at setup.
    blocks: Box<[Block; BLOCK_CAPACITY]>,
    /// Cold templates and 32-bit operand slot metadata per block.
    templates: Box<[BlockTemplate; BLOCK_CAPACITY]>,
    /// Mapped virtual registers for self-modified 32-bit operand slots.
    vregs: Box<[VirtualRegister; VREG_CAPACITY]>,
    /// Bitmask of `(address >> 12) & 63` pages holding bound virtual registers.
    vreg_page_mask: u64,
    /// Cold decoded forms, indexed by block slot and instruction position.
    fallback: Box<[Instruction]>,
    prepared_entries: Option<PreparedEntries>,
    context: Option<Context>,
    /// Registers lineage, CS version, selector and PE mode under which
    /// `code32` and `context` were last resolved.
    key: Option<(u32, u64, u16, bool)>,
    code32: bool,
    /// Execute window of `context`; None leaves its faults to the interpreter.
    window: Option<SegmentWindow>,
    pub enabled: bool,
    /// Decode common 387 forms into batch blocks; off keeps the reference.
    pub(super) x87: bool,
    /// Batched x87 forms use the exact binary64 fast paths of `doslet-x87`.
    pub(super) x87_fast: bool,
    pub(super) profile: Option<crate::execution_tiers::ExecutionProfile>,
    #[cfg(feature = "profiling")]
    pub stats: DecodeCacheStats,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: alloc::vec![None; CAPACITY]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("the vector has CAPACITY entries")),
            blocks: alloc::vec![Block::EMPTY; BLOCK_CAPACITY]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("the vector has BLOCK_CAPACITY blocks")),
            templates: alloc::vec![BlockTemplate::EMPTY; BLOCK_CAPACITY]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("the vector has BLOCK_CAPACITY templates")),
            vregs: alloc::vec![VirtualRegister::EMPTY; VREG_CAPACITY]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("the vector has VREG_CAPACITY vregs")),
            vreg_page_mask: 0,
            fallback: alloc::vec![Instruction::UNSUPPORTED; BLOCK_CAPACITY * BLOCK_INSTRUCTIONS]
                .into_boxed_slice(),
            prepared_entries: None,
            context: None,
            key: None,
            code32: false,
            window: None,
            enabled: true,
            x87: true,
            x87_fast: true,
            profile: None,
            #[cfg(feature = "profiling")]
            stats: DecodeCacheStats::default(),
        }
    }
}
impl core::fmt::Debug for Cache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecodeCache")
            .field("enabled", &self.enabled)
            .field("capacity", &CAPACITY)
            .finish_non_exhaustive()
    }
}
impl Cache {
    pub fn clear(&mut self) {
        self.entries.fill(None);
        self.blocks.fill(Block::EMPTY);
        self.templates.fill(BlockTemplate::EMPTY);
        self.vregs.fill(VirtualRegister::EMPTY);
        self.vreg_page_mask = 0;
        self.context = None;
        self.key = None;
        self.code32 = false;
        self.window = None;
    }
    /// Full context check. Entries are keyed by linear address and default
    /// operand/address mode, so they remain valid across compatible contexts;
    /// each hit still checks its offset range against the current window.
    #[inline(always)]
    fn resolve_context(&mut self, r: &Registers) {
        let context = Context {
            cs: r.segment_cache(SegmentRegister::Cs),
            selector: r.cs,
            protected: r.protected,
        };
        self.code32 = context.cs.default32;
        if self.context != Some(context) {
            #[cfg(feature = "profiling")]
            {
                self.stats.context_switches += u64::from(self.context.is_some());
            }
            self.context = Some(context);
            // The context fixes every CS check except the instruction's offset range.
            self.window = r.window(SegmentRegister::Cs, Access::Execute);
        }
    }
    #[inline(always)]
    pub(super) fn code_context(&mut self, r: &Registers) -> Option<(SegmentWindow, SegmentCache)> {
        // The hidden CS cache changes only with a new version in the same
        // lineage or with PE mode; the selector is public, so it is keyed too.
        // Keep the tuple: a packed [u64; 2] key measured ~5 ns/step slower in
        // Chrome WASM, although native code was unaffected.
        let key = (r.cs_lineage, r.cs_version, r.cs, r.protected);
        let known = r.cs_lineage != 0 && r.cs_version != u64::MAX && self.key == Some(key);
        #[cfg(debug_assertions)]
        if known {
            let cs = r.segment_cache(SegmentRegister::Cs);
            debug_assert_eq!(self.code32, cs.default32);
            debug_assert!(
                !self.code32
                    || self.context
                        == Some(Context {
                            cs,
                            selector: r.cs,
                            protected: r.protected,
                        })
            );
        }
        if !known {
            self.resolve_context(r);
            self.key = Some(key);
        }
        Some((self.window?, self.context?.cs))
    }
    fn instruction<B: CpuBus>(&mut self, r: &Registers, bus: &B) -> Option<Instruction> {
        if !self.enabled {
            return None;
        }
        let Some((window, _)) = self.code_context(r) else {
            #[cfg(feature = "profiling")]
            {}
            return None;
        };
        #[cfg(feature = "profiling")]
        {
            self.stats.code16_lookups += u64::from(!self.code32);
        }
        self.instruction_in(window, r.ip, bus)
    }
    /// Look up `ip` in an already resolved code context.
    #[inline]
    fn instruction_in<B: CpuBus>(
        &mut self,
        window: SegmentWindow,
        ip: u32,
        bus: &B,
    ) -> Option<Instruction> {
        if !self.code32 && !self.context?.protected && ip >= 0xFFF1 {
            return None;
        }
        let address = window.address(ip, 1)?;
        let index = ((address ^ (address >> 9)) as usize) & (CAPACITY - 1);
        if let Some(entry) = self.entries[index]
            && entry.address == address
            && entry.code32 == self.code32
        {
            let segment_valid = window.address(ip, entry.instruction.length).is_some();
            if segment_valid && bus.code_valid(&entry.check) {
                if matches!(entry.instruction.operation, Operation::Unsupported) {
                    #[cfg(feature = "profiling")]
                    {
                        self.stats.fallback_hits += 1;
                    }
                    return None;
                }
                #[cfg(feature = "profiling")]
                {
                    self.stats.hits += 1;
                }
                return Some(entry.instruction);
            }
            self.entries[index] = None;
            #[cfg(feature = "profiling")]
            {
                self.stats.invalidations += 1;
                // Classify with a fresh check; only profiling builds pay for it.
                if !segment_valid {
                    self.stats.segment_invalidations += 1;
                } else if let Some(check) = bus.code_check(address, entry.instruction.length) {
                    if check.space >> 32 != entry.check.space >> 32 {
                        self.stats.owner_invalidations += 1;
                    } else if check.space != entry.check.space {
                        self.stats.mapping_invalidations += 1;
                    } else {
                        self.stats.write_invalidations += 1;
                    }
                } else {
                    self.stats.unavailable_invalidations += 1;
                }
            }
        }
        #[cfg(feature = "profiling")]
        {
            self.stats.misses += 1;
            self.stats.collisions += u64::from(self.entries[index].is_some());
        }
        let bytes = bus.code_bytes(address, 15)?;
        #[cfg(feature = "profiling")]
        {
            self.stats.decode_attempts += 1;
        }
        let Some(instruction) = Instruction::decode_mode(bytes, self.code32) else {
            #[cfg(feature = "profiling")]
            {
                self.stats.failed_decodes += 1;
            }
            // Remember the rejection while the bytes the decoder saw stay
            // unchanged, so the reference path runs without another attempt.
            let length = bytes.len() as u8;
            if let Some(check) = bus.code_check(address, length) {
                self.entries[index] = Some(Entry {
                    address,
                    code32: self.code32,
                    check,
                    instruction: Instruction::UNSUPPORTED,
                });
            }
            return None;
        };
        // Failed probes leave registers/bus untouched. Let byte fetching report
        // the original fault and partial IP progress at a code-segment boundary.
        window.address(ip, instruction.length)?;
        let check = bus.code_check(address, instruction.length)?;
        #[cfg(feature = "profiling")]
        {
            self.stats.evictions += u64::from(self.entries[index].is_some());
        }
        self.entries[index] = Some(Entry {
            address,
            code32: self.code32,
            check,
            instruction,
        });
        Some(instruction)
    }
}

impl Cache {
    #[inline]
    fn vreg_hash(address: u32) -> usize {
        (((address >> 2) ^ (address >> 5) ^ (address >> 11)) as usize) & (VREG_CAPACITY - 1)
    }

    #[inline(always)]
    fn write_vreg(&mut self, address: u32, value: u32) {
        if self.vreg_page_mask & (1u64 << ((address >> 12) & 63)) != 0 {
            self.write_vreg_slow(address, value);
        }
    }

    #[cold]
    fn write_vreg_slow(&mut self, address: u32, value: u32) {
        let base = Self::vreg_hash(address);
        for step in 0..VREG_PROBE {
            let slot = (base + step) & (VREG_CAPACITY - 1);
            let vreg = self.vregs[slot];
            if vreg.address != address {
                continue;
            }
            self.vregs[slot].value = value;
            let b_idx = usize::from(vreg.block_index);
            let pos = usize::from(vreg.insn_pos);
            let off = usize::from(vreg.template_offset);
            let block = &mut self.blocks[b_idx];
            let template = &mut self.templates[b_idx];
            let expected_off = if vreg.is_displacement {
                template.disp32_offset[pos]
            } else {
                template.imm32_offset[pos]
            };
            if usize::from(block.count) <= pos
                || expected_off != vreg.template_offset
                || block.address.wrapping_add(u32::from(vreg.template_offset)) != address
                || off + 4 > usize::from(block.length)
            {
                continue;
            }
            if vreg.is_displacement {
                block.instructions[pos].set_displacement(value);
            } else {
                block.instructions[pos].set_immediate(value);
            }
            template.bytes[off..off + 4].copy_from_slice(&value.to_le_bytes());
            if let Some(profile) = &self.profile {
                let insn_start = usize::from(template.insn_offset[pos]);
                let insn_len = usize::from(block.instructions[pos].length());
                if insn_start + insn_len <= usize::from(block.length) {
                    let tier = profile.classify(
                        block.address.wrapping_add(insn_start as u32),
                        block.code32,
                        &template.bytes[insn_start..insn_start + insn_len],
                    );
                    block.instructions[pos].set_tier(tier);
                }
            }
        }
    }

    fn bind_vreg(
        &mut self,
        address: u32,
        value: u32,
        block_index: usize,
        insn_pos: u8,
        is_displacement: bool,
        template_offset: u8,
    ) -> u8 {
        self.vreg_page_mask |= 1u64 << ((address >> 12) & 63);
        let existing = if is_displacement {
            self.templates[block_index].disp_vreg[usize::from(insn_pos)]
        } else {
            self.templates[block_index].imm_vreg[usize::from(insn_pos)]
        };
        if existing != u8::MAX {
            let slot = usize::from(existing);
            let vreg = &mut self.vregs[slot];
            if vreg.address == address
                && usize::from(vreg.block_index) == block_index
                && vreg.insn_pos == insn_pos
                && vreg.is_displacement == is_displacement
            {
                vreg.value = value;
                vreg.template_offset = template_offset;
                return existing;
            }
        }
        let base = Self::vreg_hash(address);
        let mut free_slot = None;
        for step in 0..VREG_PROBE {
            let slot = (base + step) & (VREG_CAPACITY - 1);
            let vreg = &mut self.vregs[slot];
            if vreg.address == address
                && usize::from(vreg.block_index) == block_index
                && vreg.insn_pos == insn_pos
                && vreg.is_displacement == is_displacement
            {
                vreg.value = value;
                vreg.template_offset = template_offset;
                return slot as u8;
            }
            if vreg.address == u32::MAX && free_slot.is_none() {
                free_slot = Some(slot);
            }
        }
        let target = free_slot
            .unwrap_or((base + (address as usize & (VREG_PROBE - 1))) & (VREG_CAPACITY - 1));
        self.vregs[target] = VirtualRegister {
            address,
            value,
            block_index: block_index as u16,
            insn_pos,
            is_displacement,
            template_offset,
        };
        target as u8
    }

    /// Revalidate a block whose code-page generation advanced. If either no
    /// bytes in this block changed or only 32-bit displacement/immediate slots
    /// changed, bind/update their virtual registers and refresh `check` without
    /// flushing the block or re-decoding instructions.
    #[cold]
    fn revalidate_block<B: CpuBus>(&mut self, index: usize, bus: &B) -> bool {
        let block = self.blocks[index];
        let len = usize::from(block.length);
        if len == 0 || len > MAX_BLOCK_BYTES {
            return false;
        }
        let Some(live) = bus.code_bytes(block.address, block.length) else {
            return false;
        };
        if live.len() < len {
            return false;
        }
        let live = &live[..len];
        if live == &self.templates[index].bytes[..len] {
            let Some(new_check) = bus.code_check(block.address, block.length) else {
                return false;
            };
            self.blocks[index].check = new_check;
            return true;
        }
        if block.count == 0 {
            return false;
        }
        let count = usize::from(block.count);
        for pos in 0..count {
            let disp_off = self.templates[index].disp32_offset[pos];
            if disp_off != u8::MAX {
                let off = usize::from(disp_off);
                if off + 4 <= len && live[off..off + 4] != self.templates[index].bytes[off..off + 4]
                {
                    let val = u32::from_le_bytes([
                        live[off],
                        live[off + 1],
                        live[off + 2],
                        live[off + 3],
                    ]);
                    let vreg_idx = self.bind_vreg(
                        block.address.wrapping_add(u32::from(disp_off)),
                        val,
                        index,
                        pos as u8,
                        true,
                        disp_off,
                    );
                    self.templates[index].disp_vreg[pos] = vreg_idx;
                    self.blocks[index].instructions[pos].set_displacement(val);
                    self.templates[index].bytes[off..off + 4].copy_from_slice(&live[off..off + 4]);
                }
            }
            let imm_off = self.templates[index].imm32_offset[pos];
            if imm_off != u8::MAX {
                let off = usize::from(imm_off);
                if off + 4 <= len && live[off..off + 4] != self.templates[index].bytes[off..off + 4]
                {
                    let val = u32::from_le_bytes([
                        live[off],
                        live[off + 1],
                        live[off + 2],
                        live[off + 3],
                    ]);
                    let vreg_idx = self.bind_vreg(
                        block.address.wrapping_add(u32::from(imm_off)),
                        val,
                        index,
                        pos as u8,
                        false,
                        imm_off,
                    );
                    self.templates[index].imm_vreg[pos] = vreg_idx;
                    self.blocks[index].instructions[pos].set_immediate(val);
                    self.templates[index].bytes[off..off + 4].copy_from_slice(&live[off..off + 4]);
                }
            }
            if let Some(profile) = &self.profile {
                let insn_start = usize::from(self.templates[index].insn_offset[pos]);
                let insn_len = usize::from(self.blocks[index].instructions[pos].length());
                if insn_start + insn_len <= len {
                    let tier = profile.classify(
                        block.address.wrapping_add(insn_start as u32),
                        block.code32,
                        &self.templates[index].bytes[insn_start..insn_start + insn_len],
                    );
                    self.blocks[index].instructions[pos].set_tier(tier);
                }
            }
        }
        if live != &self.templates[index].bytes[..len] {
            return false;
        }
        let Some(new_check) = bus.code_check(block.address, block.length) else {
            return false;
        };
        self.blocks[index].check = new_check;
        true
    }

    #[cold]
    fn revalidate_in_batch<B: CpuBus>(
        &mut self,
        index: usize,
        bus: &B,
        check: &mut CodeCheck,
    ) -> bool {
        if self.revalidate_block(index, bus) {
            *check = self.blocks[index].check;
            true
        } else {
            false
        }
    }

    /// Find or decode the block starting at `ip`, validated against the current
    /// CS window and code generations. None when no batchable block starts there.
    #[inline]
    fn block_in<B: CpuBus>(&mut self, window: SegmentWindow, ip: u32, bus: &B) -> Option<usize> {
        if !self.code32 && !self.context?.protected && ip >= 0xFFF1 {
            return None;
        }
        let address = window.address(ip, 1)?;
        let index = ((address ^ (address >> 10)) as usize) & (BLOCK_CAPACITY - 1);
        let block = &self.blocks[index];
        if block.address == address && block.code32 == self.code32 {
            let (count, length) = (block.count, block.length);
            let valid = if bus.code_valid(&block.check) {
                true
            } else {
                (count == 0 || window.address(ip, length).is_some())
                    && self.revalidate_block(index, bus)
            };
            if valid {
                if count == 0 {
                    return None;
                }
                if window.address(ip, length).is_some() {
                    #[cfg(feature = "profiling")]
                    {
                        self.stats.block_hits += 1;
                    }
                    return Some(index);
                }
            }
        }
        self.build_block(window, ip, address, index, bus)
    }

    #[cold]
    fn build_block<B: CpuBus>(
        &mut self,
        window: SegmentWindow,
        ip: u32,
        address: u32,
        index: usize,
        bus: &B,
    ) -> Option<usize> {
        #[cfg(feature = "profiling")]
        {
            let slot = &self.blocks[index];
            self.stats.block_misses += 1;
            self.stats.block_conflicts +=
                u64::from(slot.address != address && slot.address != u32::MAX);
            self.stats.block_stale +=
                u64::from(slot.address == address && !bus.code_valid(&slot.check));
        }
        let bytes = bus.code_bytes(address, u8::MAX)?;
        let old_template = self.templates[index];
        for v in old_template
            .disp_vreg
            .into_iter()
            .chain(old_template.imm_vreg)
        {
            if v != u8::MAX {
                let v_idx = usize::from(v);
                if usize::from(self.vregs[v_idx].block_index) == index {
                    self.vregs[v_idx] = VirtualRegister::EMPTY;
                }
            }
        }
        let mut template = BlockTemplate::EMPTY;
        let mut block = Block {
            address,
            code32: self.code32,
            ..Block::EMPTY
        };
        // Block keys are linear, so annotate and split by linear addresses even
        // in a non-flat CS. Only a flat-32 caller may act on these annotations;
        // compatible CS contexts can therefore continue sharing the same block.
        let entries = self
            .prepared_entries
            .as_ref()
            .filter(|_| self.code32)
            .map_or(&[][..], |entries| entries.ips.as_ref());
        let mut entry_position = entries.partition_point(|&entry| entry < address);
        block.prepared_entry = entries.get(entry_position) == Some(&address);
        entry_position += usize::from(block.prepared_entry);
        let mut offset = 0;
        while usize::from(block.count) < BLOCK_INSTRUCTIONS {
            let current = address.wrapping_add(offset as u32);
            // A registry address inside an instruction is not an instruction
            // boundary. Skip it here so it cannot hide a later valid entry.
            while entries
                .get(entry_position)
                .is_some_and(|&entry| entry < current)
            {
                entry_position += 1;
            }
            if offset != 0 && entries.get(entry_position) == Some(&current) {
                break;
            }
            let Some(instruction) =
                Instruction::decode_block(&bytes[offset..], self.code32, self.x87)
            else {
                break;
            };
            let end = offset + usize::from(instruction.length);
            if matches!(
                instruction.operation,
                Operation::Input | Operation::Output | Operation::Unsupported
            ) || end > usize::from(u8::MAX)
                || (!self.code32
                    && !self.context?.protected
                    && u64::from(ip) + end as u64 >= 0x10000)
                || window
                    .address(ip.wrapping_add(offset as u32), instruction.length)
                    .is_none()
            {
                break;
            }
            let position = usize::from(block.count);
            let mut prepared = Prepared::new(instruction);
            if let Some(profile) = &self.profile {
                prepared.set_tier(profile.classify(
                    address.wrapping_add(offset as u32),
                    self.code32,
                    &bytes[offset..end],
                ));
            }
            template.insn_offset[position] = offset as u8;
            if prepared.is_fallback() {
                self.fallback[index * BLOCK_INSTRUCTIONS + position] = instruction;
            } else {
                if instruction.disp32_offset != u8::MAX {
                    template.disp32_offset[position] = offset as u8 + instruction.disp32_offset;
                }
                if instruction.imm32_offset != u8::MAX {
                    template.imm32_offset[position] = offset as u8 + instruction.imm32_offset;
                }
            }
            block.instructions[position] = prepared;
            block.count += 1;
            offset = end;
            if matches!(
                instruction.operation,
                Operation::Branch { .. }
                    | Operation::Call(_)
                    | Operation::CallIndirect(_)
                    | Operation::JumpIndirect(_)
                    | Operation::Return(_)
                    | Operation::Loop { .. }
            ) {
                break;
            }
        }
        // A rejection covers the bytes the decoder could have inspected.
        block.length = if block.count == 0 {
            bytes.len().min(15)
        } else {
            offset
        } as u8;
        let len = usize::from(block.length);
        if len <= MAX_BLOCK_BYTES {
            template.bytes[..len].copy_from_slice(&bytes[..len]);
        }
        block.check = bus.code_check(address, block.length)?;
        self.templates[index] = template;
        self.blocks[index] = block;
        (block.count != 0).then_some(index)
    }
}

#[derive(Clone, Copy)]
enum Target {
    Register(u8),
    Memory {
        segment: SegmentRegister,
        base: u8,
        index: u8,
        scale: u8,
        displacement: u32,
        address32: bool,
    },
}
impl Target {
    fn resolve(self, r: &Registers) -> Operand {
        match self {
            Self::Register(code) => Operand::Register(code),
            Self::Memory {
                segment,
                base,
                index,
                scale,
                displacement,
                address32,
            } => {
                let value = |code| {
                    if code == NO_REGISTER {
                        0
                    } else {
                        let value = r.get32(Gpr32::decode(code));
                        if address32 {
                            value
                        } else {
                            u32::from(value as u16)
                        }
                    }
                };
                let offset = value(base)
                    .wrapping_add(value(index).wrapping_shl(u32::from(scale)))
                    .wrapping_add(displacement);
                Operand::Memory {
                    segment,
                    offset: if address32 {
                        offset
                    } else {
                        u32::from(offset as u16)
                    },
                }
            }
        }
    }
}
#[derive(Clone, Copy)]
enum Value {
    Operand(Target),
    Immediate(u32),
}
impl Value {
    fn read<B: CpuBus>(self, cpu: &Cpu, bus: &mut B, width: u8) -> Result<u32, CpuError> {
        match self {
            Self::Immediate(value) => Ok(value),
            Self::Operand(target) => read_width(target.resolve(&cpu.registers), cpu, bus, width),
        }
    }
}
#[inline(always)]
fn alu_width(flags: u32, left: u32, right: u32, operation: u8, width: u8) -> (u32, u32) {
    if width == 4 {
        flags::alu32(flags, left, right, operation)
    } else {
        let (value, flags) = flags::alu(flags, left as u16, right as u16, operation, width == 2);
        (u32::from(value), flags)
    }
}
#[inline(always)]
fn increment_result(previous: u32, value: u32, decrement: bool, width: u8) -> (u32, u32) {
    let (value, updated) = alu_width(previous, value, 1, if decrement { 5 } else { 0 }, width);
    (value, (updated & !flags::CARRY) | (previous & flags::CARRY))
}

#[inline(always)]
fn extend_result(value: u32, word: bool, signed: bool) -> u32 {
    match (signed, word) {
        (true, true) => value as i16 as i32 as u32,
        (true, false) => value as u8 as i8 as i32 as u32,
        _ => value,
    }
}

/// Shared by cached reference execution and prepared shift handlers.
#[inline(always)]
fn shift_result(
    mut updated_flags: u32,
    original: u32,
    operation: u8,
    count: u8,
    width: u8,
) -> (u32, u32) {
    let bits = width * 8;
    let sign = 1 << (bits - 1);
    let mask = u32::MAX >> (32 - bits);
    let mut value = original;
    let mut carry = updated_flags & flags::CARRY != 0;
    for _ in 0..count {
        let old_carry = carry;
        if matches!(operation, 0 | 2 | 4 | 6) {
            carry = value & sign != 0;
            value = (value << 1) & mask;
            if operation == 0 {
                value |= u32::from(carry);
            }
            if operation == 2 {
                value |= u32::from(old_carry);
            }
        } else {
            carry = value & 1 != 0;
            let high = value & sign;
            value >>= 1;
            if operation == 1 {
                value |= u32::from(carry) << (bits - 1);
            }
            if operation == 3 {
                value |= u32::from(old_carry) << (bits - 1);
            }
            if operation == 7 {
                value |= high;
            }
        }
    }
    if operation >= 4 {
        updated_flags =
            alu_width(updated_flags, value, 0, 1, width).1 | (updated_flags & flags::OVERFLOW);
    }
    updated_flags = (updated_flags & !flags::CARRY) | u32::from(carry);
    if count == 1 {
        let overflow = match operation {
            0 | 2 | 4 | 6 => (value & sign != 0) != carry,
            1 | 3 => ((value >> (bits - 1)) ^ (value >> (bits - 2))) & 1 != 0,
            5 => original & sign != 0,
            _ => false,
        };
        updated_flags =
            (updated_flags & !flags::OVERFLOW) | if overflow { flags::OVERFLOW } else { 0 };
    }
    (value, updated_flags)
}

fn read_width<B: CpuBus>(
    target: Operand,
    cpu: &Cpu,
    bus: &mut B,
    width: u8,
) -> Result<u32, CpuError> {
    match width {
        4 => target.read32(&cpu.registers, bus),
        _ => target.read(&cpu.registers, bus, width == 2).map(u32::from),
    }
}
fn write_width<B: CpuBus>(
    target: Operand,
    cpu: &mut Cpu,
    bus: &mut B,
    width: u8,
    value: u32,
) -> Result<(), CpuError> {
    match width {
        4 => target.write32(&mut cpu.registers, bus, value),
        _ => target.write(&mut cpu.registers, bus, width == 2, value as u16),
    }
}
#[derive(Clone, Copy)]
enum Operation {
    Move {
        target: Target,
        source: Value,
    },
    Lea {
        register: u8,
        target: Target,
    },
    Alu {
        target: Target,
        source: Value,
        operation: u8,
        store: bool,
    },
    Exchange {
        target: Target,
        register: u8,
    },
    Inc {
        register: u8,
        decrement: bool,
    },
    Unary {
        target: Target,
        negate: bool,
    },
    Push(Value),
    Pop(u8),
    Branch {
        condition: Option<u8>,
        displacement: i32,
    },
    Call(i32),
    CallIndirect(Value),
    JumpIndirect(Value),
    IncTarget {
        target: Target,
        decrement: bool,
    },
    Shift {
        target: Target,
        operation: u8,
        count: Option<u8>,
    },
    DoubleShift {
        target: Target,
        register: u8,
        right: bool,
        count: Option<u8>,
    },
    Return(u16),
    Input,
    Output,
    /// LOOPNE/LOOPE/LOOP (E0-E2) and JECXZ (E3), selected by `kind`.
    Loop {
        kind: u8,
        displacement: i32,
        address32: bool,
    },
    /// MOVS (A4), STOS (AA) or LODS (AC); one iteration per step under REP.
    String {
        kind: u8,
        width: u8,
        segment: SegmentRegister,
        address32: bool,
        repeat: bool,
    },
    /// MUL, IMUL, DIV or IDIV of the accumulator (F6/F7 /4-/7).
    MulDiv {
        target: Target,
        operation: u8,
    },
    /// CWDE (98) or CDQ (99).
    Extend {
        cdq: bool,
    },
    /// MOVZX/MOVSX (0F B6/B7/BE/BF) into a dword register.
    MoveExtend {
        register: u8,
        source: Target,
        word: bool,
        signed: bool,
    },
    /// MOV r/m, Sreg (8C): a full-width register or a word in memory.
    StoreSegment {
        target: Target,
        segment: SegmentRegister,
    },
    /// PUSH ES/CS/SS/DS/FS/GS, zero-extended to the operand width.
    PushSegment(SegmentRegister),
    /// POP r/m (8F /0); the address uses the registers before the pop.
    PopTarget(Target),
    /// LEAVE (C9).
    Leave,
    /// ENTER imm16, 0 (C8); other nesting levels use the reference path.
    Enter(u16),
    /// BT/BTS/BTR/BTC r/m, imm8 (0F BA /4-/7).
    BitTest {
        target: Target,
        operation: u8,
        bit: u8,
    },
    /// A batched 387 form (block decoding only); `target` is its memory
    /// operand, or an unused register for register forms.
    X87 {
        x87: X87,
        target: Target,
    },
    /// A remembered decoder rejection; never executed from the cache.
    Unsupported,
}
#[derive(Clone, Copy)]
pub(super) struct Instruction {
    length: u8,
    /// Position before a faultable data read that precedes immediate fetching.
    read_length: u8,
    dword: bool,
    width: u8,
    branch32: bool,
    disp32_offset: u8,
    imm32_offset: u8,
    operation: Operation,
}

#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
impl Instruction {
    pub(super) fn native_candidate(self) -> bool {
        if self.width == 2 || !self.branch32 {
            return false;
        }
        match self.operation {
            Operation::Move {
                target: Target::Register(_),
                source,
            } => {
                matches!(
                    source,
                    Value::Immediate(_)
                        | Value::Operand(Target::Register(_))
                        | Value::Operand(Target::Memory {
                            address32: true,
                            ..
                        })
                )
            }
            Operation::Move {
                target: Target::Memory {
                    address32: true, ..
                },
                source: Value::Immediate(_) | Value::Operand(Target::Register(_)),
            } => true,
            Operation::Alu {
                target,
                source,
                operation,
                store,
            } => {
                let readable = |target| {
                    matches!(
                        target,
                        Target::Register(_)
                            | Target::Memory {
                                address32: true,
                                ..
                            }
                    )
                };
                readable(target)
                    && match source {
                        Value::Immediate(_) => true,
                        Value::Operand(source) => readable(source),
                    }
                    && if store {
                        self.dword && matches!(target, Target::Register(_))
                    } else {
                        self.dword || matches!(operation, 4 | 7)
                    }
            }
            Operation::Lea {
                target: Target::Memory {
                    address32: true, ..
                },
                ..
            }
            | Operation::Push(_)
            | Operation::Pop(_)
            | Operation::Call(_)
            | Operation::CallIndirect(_)
            | Operation::Return(_) => true,
            Operation::Exchange {
                target: Target::Memory {
                    address32: true, ..
                },
                ..
            } => self.dword,
            Operation::Inc { .. }
            | Operation::Exchange {
                target: Target::Register(_),
                ..
            }
            | Operation::Unary {
                target: Target::Register(_),
                ..
            } => self.dword,
            _ => false,
        }
    }
}

/// Decode default-32-bit trace operations without resolving data addresses.
/// Used segment assumptions are validated and cached at native entry.
#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) fn native_trace_instruction(
    bytes: &[u8],
    offset: u8,
) -> Option<(doslet_x86_jit::TraceInstruction, u8, u8)> {
    use doslet_x86_jit::{DwordMemoryAddress, TraceInstruction, TraceOperation as Op, TraceSource};
    let i = Instruction::decode(bytes)?;
    if i.width == 2 || !i.branch32 {
        return None;
    }
    let mut segments = 0;
    let mut write_segments = 0;
    let memory = |target| match target {
        Target::Memory {
            segment,
            base,
            index,
            scale,
            displacement,
            address32: true,
        } => Some((
            segment,
            DwordMemoryAddress {
                base: (base != NO_REGISTER).then_some(base),
                index: (index != NO_REGISTER).then_some(index),
                scale,
                displacement,
            },
        )),
        _ => None,
    };
    let mut source = |v, byte| match v {
        Value::Immediate(v) => Some(TraceSource::Immediate(v)),
        Value::Operand(Target::Register(r)) => Some(if byte {
            TraceSource::ByteRegister(r)
        } else {
            TraceSource::Register(r)
        }),
        Value::Operand(target) => {
            let (segment, address) = memory(target)?;
            segments |= 1 << segment as u8;
            Some(if byte {
                TraceSource::ByteMemory(address)
            } else {
                TraceSource::Memory(address)
            })
        }
    };
    let operation = match i.operation {
        Operation::Move {
            target: Target::Register(target),
            source: value,
        } => {
            let source = source(value, !i.dword)?;
            if i.dword {
                Op::Move { target, source }
            } else {
                Op::MoveByte { target, source }
            }
        }
        Operation::Move {
            target,
            source: value @ (Value::Immediate(_) | Value::Operand(Target::Register(_))),
        } => {
            let source = source(value, !i.dword)?;
            let (segment, address) = memory(target)?;
            write_segments |= 1 << segment as u8;
            if i.dword {
                Op::Store { address, source }
            } else {
                Op::StoreByte { address, source }
            }
        }
        Operation::Alu {
            target,
            source: value,
            operation,
            store: false,
        } if matches!(operation, 4 | 7) => Op::Compare {
            left: source(Value::Operand(target), !i.dword)?,
            right: source(value, !i.dword)?,
            operation,
            byte: !i.dword,
        },
        Operation::Alu {
            target: Target::Register(target),
            source: value,
            operation,
            store,
        } if i.dword => Op::Alu {
            target,
            source: source(value, false)?,
            operation,
            store,
        },
        Operation::Lea {
            register: target,
            target: address,
        } => Op::Lea {
            target,
            address: memory(address)?.1,
        },
        Operation::Inc {
            register,
            decrement,
        } if i.dword => Op::Inc {
            register,
            decrement,
        },
        Operation::Exchange {
            target: Target::Register(left),
            register: right,
        } if i.dword => Op::Exchange { left, right },
        Operation::Exchange { target, register } if i.dword => {
            let (segment, address) = memory(target)?;
            segments |= 1 << segment as u8;
            write_segments |= 1 << segment as u8;
            Op::ExchangeMemory { address, register }
        }
        Operation::Unary {
            target: Target::Register(register),
            negate,
        } if i.dword => Op::Unary { register, negate },
        Operation::Push(value) => {
            let source = source(value, false)?;
            write_segments |= 1 << SegmentRegister::Ss as u8;
            Op::Push { source }
        }
        Operation::Pop(register) => {
            segments |= 1 << SegmentRegister::Ss as u8;
            Op::Pop { register }
        }
        Operation::Call(displacement) => {
            write_segments |= 1 << SegmentRegister::Ss as u8;
            Op::Call {
                target: u32::from(offset)
                    .wrapping_add(u32::from(i.length))
                    .wrapping_add_signed(displacement),
            }
        }
        Operation::CallIndirect(value) => {
            let target = source(value, false)?;
            write_segments |= 1 << SegmentRegister::Ss as u8;
            Op::CallIndirect { target }
        }
        Operation::Return(release) => {
            segments |= 1 << SegmentRegister::Ss as u8;
            Op::Return { release }
        }
        Operation::Branch {
            condition,
            displacement,
        } => Op::Branch {
            condition,
            target: u32::from(offset)
                .wrapping_add(u32::from(i.length))
                .wrapping_add_signed(displacement),
        },
        _ => return None,
    };
    Some((
        TraceInstruction {
            offset,
            length: i.length,
            operation,
        },
        segments,
        write_segments,
    ))
}

/// Upper bound of one batch; the machine's event deadline usually ends it first.
pub(super) const BATCH_STEPS: u32 = 256;
const PLAIN_RAM: &str = "validated batch access and plain-RAM bus contract";

/// A validated operand: a register or a plain-RAM linear address.
#[derive(Clone, Copy)]
enum Place {
    Register(u8),
    Ram(u32),
}
#[derive(Clone, Copy)]
enum Source {
    Immediate(u32),
    Place(Place),
}

/// Data-segment windows resolved once per batch. Batched instructions cannot
/// load segment registers, so the hidden caches stay unchanged throughout.
#[derive(Default)]
pub(super) struct Windows([[Option<Option<SegmentWindow>>; 2]; 6]);
impl Windows {
    #[inline]
    fn linear<B: CpuBus>(
        &mut self,
        r: &Registers,
        bus: &B,
        segment: SegmentRegister,
        offset: u32,
        length: u8,
        access: Access,
    ) -> Option<u32> {
        let slot = &mut self.0[segment as usize][usize::from(access == Access::Write)];
        let window = *slot.get_or_insert_with(|| r.window(segment, access));
        let address = window?.address(offset, length)?;
        bus.is_batch_memory(address, length).then_some(address)
    }
    #[inline]
    fn place<B: CpuBus>(
        &mut self,
        r: &Registers,
        bus: &B,
        target: Target,
        length: u8,
        access: Access,
    ) -> Option<Place> {
        match target.resolve(r) {
            Operand::Register(code) => Some(Place::Register(code)),
            Operand::Memory { segment, offset } => self
                .linear(r, bus, segment, offset, length, access)
                .map(Place::Ram),
        }
    }
    #[inline]
    fn source<B: CpuBus>(
        &mut self,
        r: &Registers,
        bus: &B,
        value: Value,
        length: u8,
    ) -> Option<Source> {
        match value {
            Value::Immediate(value) => Some(Source::Immediate(value)),
            Value::Operand(target) => self
                .place(r, bus, target, length, Access::Read)
                .map(Source::Place),
        }
    }
}

impl Cpu {
    /// Replace immutable flat-32 procedure-entry EIPs at a setup/batch boundary.
    /// Entries must be strictly increasing; returned indices retain this order.
    /// Invalid input preserves the previous registry and caches. Installation
    /// allocates metadata and invalidates decoded blocks, with no guest changes.
    /// An empty list removes the registry. Ordinary cache/profile resets retain
    /// it; a newly created or restored CPU starts without registered entries.
    pub fn set_prepared_entries(
        &mut self,
        sorted_entries: alloc::vec::Vec<u32>,
    ) -> Result<(), &'static str> {
        if sorted_entries.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("prepared entries must be sorted and unique");
        }
        if sorted_entries.len() as u64 > u64::from(u32::MAX - 1) {
            return Err("too many prepared entries");
        }
        let entries = (!sorted_entries.is_empty()).then(|| PreparedEntries {
            ips: sorted_entries.into_boxed_slice(),
            hints: alloc::vec![EntryHint { ip: 0, index: u32::MAX }; ENTRY_HINT_CAPACITY]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("the vector has ENTRY_HINT_CAPACITY hints")),
        });
        self.decode_cache.blocks.fill(Block::EMPTY);
        self.decode_cache.templates.fill(BlockTemplate::EMPTY);
        self.decode_cache.vregs.fill(VirtualRegister::EMPTY);
        self.decode_cache.vreg_page_mask = 0;
        self.decode_cache.prepared_entries = entries;
        Ok(())
    }

    pub fn has_prepared_entries(&self) -> bool {
        self.decode_cache.prepared_entries.is_some()
    }

    /// Cached registry membership at the current EIP, including when decoding
    /// is disabled. A result is a scheduling hint: the caller must still check
    /// source bytes and its complete execution/context contract before dispatch.
    /// Only default-32-bit code with CS base zero uses registered flat EIPs.
    pub fn prepared_entry_index(&mut self) -> Option<usize> {
        self.decode_cache.prepared_entries.as_ref()?;
        if self.registers.protected != self.protected_mode() {
            return None;
        }
        let (window, cs) = self.decode_cache.code_context(&self.registers)?;
        if !self.decode_cache.code32 || cs.base != 0 {
            return None;
        }
        window.address(self.registers.ip, 1)?;
        self.decode_cache
            .prepared_entries
            .as_mut()?
            .index(self.registers.ip)
    }

    /// Execute a bounded run of decoded ordinary-RAM instructions without the
    /// native core. The machine checks pending IRQs first and supplies the next
    /// hardware deadline. I/O, MMIO, faultable operands and mode/IF changes stop
    /// before execution, so hardware and scheduling counters can advance once.
    pub fn interpret_batch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
    ) -> u32 {
        if self.execution_labels_enabled() {
            self.interpret_batch_mode::<B, false, false, false, true>(
                bus,
                budget,
                limit,
                [0; 2],
                &[],
            )
        } else {
            self.interpret_batch_mode::<B, false, false, false, false>(
                bus,
                budget,
                limit,
                [0; 2],
                &[],
            )
        }
    }

    /// Yield on arrival at a selected instruction after doing work, including
    /// inside a cached block. The ordinary specialization has no comparison.
    pub fn interpret_batch_until_block<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        head: u32,
    ) -> u32 {
        self.interpret_batch_until_blocks(bus, budget, limit, [head; 2])
    }

    /// Optional scheduler hints for two selected block heads.
    pub fn interpret_batch_until_blocks<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        heads: [u32; 2],
    ) -> u32 {
        if self.execution_labels_enabled() {
            self.interpret_batch_mode::<B, true, false, false, true>(bus, budget, limit, heads, &[])
        } else {
            self.interpret_batch_mode::<B, true, false, false, false>(
                bus,
                budget,
                limit,
                heads,
                &[],
            )
        }
    }

    /// Yield before any sorted entry EIP after completing at least one
    /// instruction, including an arrival inside a cached block. A declined
    /// current entry can therefore make ordinary progress without retry loops.
    pub fn interpret_batch_until_entries<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        sorted_entries: &[u32],
    ) -> u32 {
        if self.execution_labels_enabled() {
            self.interpret_batch_mode::<B, false, true, false, true>(
                bus,
                budget,
                limit,
                [0; 2],
                sorted_entries,
            )
        } else {
            self.interpret_batch_mode::<B, false, true, false, false>(
                bus,
                budget,
                limit,
                [0; 2],
                sorted_entries,
            )
        }
    }

    /// Yield before reaching another registered flat-32 entry, including by
    /// fallthrough, CALL, indirect transfer or RET. Cold decoding splits blocks
    /// at entries; the hot instruction loop performs no registry lookup or
    /// entry comparison. Starting at a declined entry makes ordinary bounded
    /// progress before yielding, including when a branch returns to that entry.
    pub fn interpret_batch_until_prepared_entry<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
    ) -> u32 {
        if self.execution_labels_enabled() {
            self.interpret_batch_mode::<B, false, false, true, true>(
                bus,
                budget,
                limit,
                [0; 2],
                &[],
            )
        } else {
            self.interpret_batch_mode::<B, false, false, true, false>(
                bus,
                budget,
                limit,
                [0; 2],
                &[],
            )
        }
    }

    fn interpret_batch_mode<
        B: CpuBus,
        const YIELD: bool,
        const ENTRIES: bool,
        const PREPARED: bool,
        const COUNT: bool,
    >(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        heads: [u32; 2],
        sorted_entries: &[u32],
    ) -> u32 {
        if budget < 2 || !self.batch_ready() || !self.decode_cache.enabled {
            return 0;
        }
        let Some((code, mut windows)) = self.batch_context() else {
            return 0;
        };
        let stop_at_prepared = PREPARED
            && self.decode_cache.code32
            && self
                .decode_cache
                .context
                .is_some_and(|context| context.cs.base == 0);
        let budget = budget.min(BATCH_STEPS).min(limit(bus));
        let mut completed = 0;
        'blocks: while completed < budget {
            // Cached blocks advance in instruction order; taken branches and
            // wrapping IPs return to this boundary. Resolve the next selected
            // EIP once per block, leaving only equality in its inner loop.
            let entry_head = if ENTRIES {
                let ip = self.registers.ip;
                let index = sorted_entries
                    .partition_point(|&entry| entry < ip || (completed == 0 && entry == ip));
                sorted_entries.get(index).copied()
            } else {
                None
            };
            if completed != 0
                && ((YIELD && heads.contains(&self.registers.ip))
                    || (ENTRIES && entry_head == Some(self.registers.ip)))
            {
                break;
            }
            let Some((index, count, mut check)) = self.batch_block(code, bus) else {
                break;
            };
            if stop_at_prepared && completed != 0 && self.decode_cache.blocks[index].prepared_entry
            {
                break;
            }
            for position in 0..count {
                if completed != 0
                    && ((YIELD && heads.contains(&self.registers.ip))
                        || (ENTRIES && entry_head == Some(self.registers.ip)))
                {
                    break 'blocks;
                }
                let instruction = self.block_instruction(index, position);
                let next = self
                    .registers
                    .ip
                    .wrapping_add(u32::from(instruction.length()));
                if !self.try_prepared(
                    bus,
                    &mut windows,
                    code,
                    instruction,
                    index * BLOCK_INSTRUCTIONS + position,
                ) {
                    break 'blocks;
                }
                if COUNT {
                    self.execution_counts[instruction.tier()] += 1;
                }
                completed += 1;
                // A taken branch, a repeated string or the budget ends the run;
                // a store to the same code page revalidates via virtual registers
                // before flushing the remaining instructions in this block.
                if completed == budget
                    || self.registers.ip != next
                    || (!bus.code_valid(&check)
                        && !self
                            .decode_cache
                            .revalidate_in_batch(index, bus, &mut check))
                {
                    continue 'blocks;
                }
            }
        }
        #[cfg(feature = "profiling")]
        {
            self.decode_cache.stats.batches += u64::from(completed != 0);
            self.decode_cache.stats.batched_steps += u64::from(completed);
        }
        completed
    }

    /// Validate every address an instruction uses, then perform it on those
    /// linear addresses. Returns false, with no effect, when any check fails:
    /// the reference step then reports faults and touches devices itself.
    /// Effects follow `execute_cached` in order and in flag computation.
    pub(super) fn try_batched<B: CpuBus>(
        &mut self,
        bus: &mut B,
        windows: &mut Windows,
        code: SegmentWindow,
        i: Instruction,
    ) -> bool {
        let r = &self.registers;
        let length = i.width;
        let next = r.ip.wrapping_add(u32::from(i.length));
        let jump = |displacement: i32| {
            let target = next.wrapping_add_signed(displacement);
            let target = if i.branch32 {
                target
            } else {
                u32::from(target as u16)
            };
            code.address(target, 1).map(|_| target)
        };
        let push_address = |windows: &mut Windows, bus: &B| {
            let sp = self.stack_pointer().wrapping_sub(u32::from(length));
            let sp = if r.stack32() {
                sp
            } else {
                u32::from(sp as u16)
            };
            windows
                .linear(r, bus, SegmentRegister::Ss, sp, length, Access::Write)
                .map(|address| (sp, address))
        };
        match i.operation {
            Operation::Move { target, source } => {
                let Some(source) = windows.source(r, bus, source, length) else {
                    return false;
                };
                let Some(target) = windows.place(r, bus, target, length, Access::Write) else {
                    return false;
                };
                let value = self.load_source(bus, source, length);
                self.store(bus, target, length, value);
            }
            Operation::Lea { register, target } => {
                let Operand::Memory { offset, .. } = target.resolve(r) else {
                    unreachable!()
                };
                self.store(bus, Place::Register(register), length, offset);
            }
            Operation::Alu {
                target,
                source,
                operation,
                store,
            } => {
                let Some(place) = windows.place(r, bus, target, length, Access::Read) else {
                    return false;
                };
                if store
                    && windows
                        .place(r, bus, target, length, Access::Write)
                        .is_none()
                {
                    return false;
                }
                let Some(source) = windows.source(r, bus, source, length) else {
                    return false;
                };
                let left = self.load(bus, place, length);
                let right = self.load_source(bus, source, length);
                let (value, new_flags) = alu_width(self.flags, left, right, operation, length);
                self.flags = new_flags;
                if store {
                    self.store(bus, place, length, value);
                }
            }
            Operation::Exchange { target, register } => {
                let Some(place) = windows.place(r, bus, target, length, Access::Read) else {
                    return false;
                };
                if windows
                    .place(r, bus, target, length, Access::Write)
                    .is_none()
                {
                    return false;
                }
                let other = Place::Register(register);
                let left = self.load(bus, place, length);
                let right = self.load(bus, other, length);
                self.store(bus, place, length, right);
                self.store(bus, other, length, left);
            }
            Operation::Inc {
                register,
                decrement,
            } => {
                let value = self.load(bus, Place::Register(register), length);
                let (value, updated) = increment_result(self.flags, value, decrement, length);
                self.flags = updated;
                self.store(bus, Place::Register(register), length, value);
            }
            Operation::IncTarget { target, .. }
            | Operation::Shift { target, .. }
            | Operation::DoubleShift { target, .. } => {
                let count = match i.operation {
                    Operation::Shift { count, .. } | Operation::DoubleShift { count, .. } => {
                        count.unwrap_or(r.get_byte(1)) & 31
                    }
                    _ => 1,
                };
                if matches!(i.operation, Operation::DoubleShift { .. }) && length == 2 && count > 16
                {
                    return false;
                }
                if count != 0
                    && (windows
                        .place(r, bus, target, length, Access::Read)
                        .is_none()
                        || windows
                            .place(r, bus, target, length, Access::Write)
                            .is_none())
                {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::Unary { target, negate } => {
                let Some(place) = windows.place(r, bus, target, length, Access::Read) else {
                    return false;
                };
                if windows
                    .place(r, bus, target, length, Access::Write)
                    .is_none()
                {
                    return false;
                }
                let value = self.load(bus, place, length);
                let value = if negate {
                    let (value, new_flags) = alu_width(self.flags, 0, value, 5, length);
                    self.flags = new_flags;
                    value
                } else {
                    !value
                };
                self.store(bus, place, length, value);
            }
            Operation::Push(value) => {
                let Some(source) = windows.source(r, bus, value, length) else {
                    return false;
                };
                let Some((sp, address)) = push_address(windows, bus) else {
                    return false;
                };
                let value = self.load_source(bus, source, length);
                self.store(bus, Place::Ram(address), length, value);
                self.set_stack_pointer(sp);
            }
            Operation::Pop(register) => {
                let sp = self.stack_pointer();
                let Some(address) =
                    windows.linear(r, bus, SegmentRegister::Ss, sp, length, Access::Read)
                else {
                    return false;
                };
                let value = self.load(bus, Place::Ram(address), length);
                self.adjust_stack(u32::from(length));
                self.store(bus, Place::Register(register), length, value);
            }
            Operation::Branch {
                condition,
                displacement,
            } => {
                if condition.is_none_or(|condition| flags::condition(self.flags, condition)) {
                    let Some(target) = jump(displacement) else {
                        return false;
                    };
                    self.registers.ip = target;
                    return true;
                }
            }
            Operation::Call(displacement) => {
                let Some(target) = jump(displacement) else {
                    return false;
                };
                let Some((sp, address)) = push_address(windows, bus) else {
                    return false;
                };
                self.store(bus, Place::Ram(address), length, next);
                self.set_stack_pointer(sp);
                self.registers.ip = target;
                return true;
            }
            Operation::JumpIndirect(value) => {
                let Some(source) = windows.source(r, bus, value, length) else {
                    return false;
                };
                let target = self.load_source(bus, source, length);
                if code.address(target, 1).is_none() {
                    return false;
                }
                self.registers.ip = target;
                return true;
            }
            Operation::CallIndirect(value) => {
                let Some(source) = windows.source(r, bus, value, length) else {
                    return false;
                };
                let Some((sp, address)) = push_address(windows, bus) else {
                    return false;
                };
                let target = self.load_source(bus, source, length);
                if code.address(target, 1).is_none() {
                    return false;
                }
                self.store(bus, Place::Ram(address), length, next);
                self.set_stack_pointer(sp);
                self.registers.ip = target;
                return true;
            }
            Operation::Return(release) => {
                let sp = self.stack_pointer();
                let Some(address) =
                    windows.linear(r, bus, SegmentRegister::Ss, sp, length, Access::Read)
                else {
                    return false;
                };
                let target = self.load(bus, Place::Ram(address), length);
                if code.address(target, 1).is_none() {
                    return false;
                }
                self.adjust_stack(u32::from(length));
                self.registers.ip = target;
                self.adjust_stack(u32::from(release));
                return true;
            }
            Operation::Loop {
                kind,
                displacement,
                address32,
            } => {
                let count = self.address_register(Gpr16::Cx, address32);
                let take = if kind == 3 {
                    count == 0
                } else {
                    let count = if address32 {
                        count.wrapping_sub(1)
                    } else {
                        u32::from((count as u16).wrapping_sub(1))
                    };
                    count != 0 && (kind == 2 || (self.flags & flags::ZERO != 0) == (kind == 1))
                };
                if take && jump(displacement).is_none() {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::String {
                kind,
                width,
                segment,
                address32,
                repeat,
            } => {
                if !repeat || self.address_register(Gpr16::Cx, address32) != 0 {
                    let si = self.address_register(Gpr16::Si, address32);
                    let di = self.address_register(Gpr16::Di, address32);
                    if kind != 0xAA
                        && windows
                            .linear(r, bus, segment, si, width, Access::Read)
                            .is_none()
                    {
                        return false;
                    }
                    if kind != 0xAC
                        && windows
                            .linear(r, bus, SegmentRegister::Es, di, width, Access::Write)
                            .is_none()
                    {
                        return false;
                    }
                }
                return self.delegate_batched(bus, i);
            }
            Operation::MulDiv { target, .. } => {
                if windows
                    .place(r, bus, target, length, Access::Read)
                    .is_none()
                {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::Extend { .. } => return self.delegate_batched(bus, i),
            Operation::MoveExtend { source, word, .. } => {
                let width = if word { 2 } else { 1 };
                if windows.place(r, bus, source, width, Access::Read).is_none() {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::StoreSegment { target, .. } => {
                if matches!(target, Target::Memory { .. })
                    && windows.place(r, bus, target, 2, Access::Write).is_none()
                {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::PushSegment(_) | Operation::Enter(_) => {
                if push_address(windows, bus).is_none() {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::PopTarget(target) => {
                let sp = self.stack_pointer();
                if windows
                    .linear(r, bus, SegmentRegister::Ss, sp, length, Access::Read)
                    .is_none()
                    || windows
                        .place(r, bus, target, length, Access::Write)
                        .is_none()
                {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::Leave => {
                let ebp = r.get32(Gpr32::Ebp);
                let sp = if r.stack32() {
                    ebp
                } else {
                    u32::from(ebp as u16)
                };
                if windows
                    .linear(r, bus, SegmentRegister::Ss, sp, length, Access::Read)
                    .is_none()
                {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::BitTest {
                target, operation, ..
            } => {
                if windows
                    .place(r, bus, target, length, Access::Read)
                    .is_none()
                    || (operation != 4
                        && windows
                            .place(r, bus, target, length, Access::Write)
                            .is_none())
                {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::X87 { x87, target } => {
                return self.x87_batched(bus, windows, x87, target, next);
            }
            Operation::Input | Operation::Output | Operation::Unsupported => return false,
        }
        self.registers.ip = next;
        true
    }

    /// Execute a validated instruction through the reference-equivalent cached
    /// path. Only a divide error remains possible; it has no effect except IP,
    /// which is restored so the reference step reports it.
    fn delegate_batched<B: CpuBus>(&mut self, bus: &mut B, i: Instruction) -> bool {
        let ip = self.registers.ip;
        if self.execute_cached(bus, i).is_err() {
            self.registers.ip = ip;
            return false;
        }
        true
    }

    #[inline(always)]
    fn load<B: CpuBus>(&self, bus: &mut B, place: Place, width: u8) -> u32 {
        match place {
            Place::Register(code) if width == 4 => self.registers.get32(Gpr32::decode(code)),
            Place::Register(code) if width == 2 => {
                u32::from(self.registers.get(Gpr16::decode(code)))
            }
            Place::Register(code) => u32::from(self.registers.get_byte(code)),
            Place::Ram(address) if width == 4 => bus.read_u32(address).expect(PLAIN_RAM),
            Place::Ram(address) if width == 2 => u32::from(bus.read_u16(address).expect(PLAIN_RAM)),
            Place::Ram(address) => u32::from(bus.read_u8(address).expect(PLAIN_RAM)),
        }
    }

    fn load_source<B: CpuBus>(&self, bus: &mut B, source: Source, width: u8) -> u32 {
        match source {
            Source::Immediate(value) => value,
            Source::Place(place) => self.load(bus, place, width),
        }
    }

    #[inline(always)]
    fn store<B: CpuBus>(&mut self, bus: &mut B, place: Place, width: u8, value: u32) {
        match place {
            Place::Register(code) if width == 4 => self.registers.set32(Gpr32::decode(code), value),
            Place::Register(code) if width == 2 => {
                self.registers.set(Gpr16::decode(code), value as u16)
            }
            Place::Register(code) => self.registers.set_byte(code, value as u8),
            Place::Ram(address) if width == 4 => {
                bus.write_u32(address, value).expect(PLAIN_RAM);
                self.decode_cache.write_vreg(address, value);
            }
            Place::Ram(address) if width == 2 => {
                bus.write_u16(address, value as u16).expect(PLAIN_RAM)
            }
            Place::Ram(address) => bus.write_u8(address, value as u8).expect(PLAIN_RAM),
        }
    }

    /// The validated block at the current IP: its slot, instruction count and
    /// code check. Batched instructions cannot change the CS context.
    #[inline]
    pub(super) fn batch_block<B: CpuBus>(
        &mut self,
        code: SegmentWindow,
        bus: &B,
    ) -> Option<(usize, usize, CodeCheck)> {
        let index = self.decode_cache.block_in(code, self.registers.ip, bus)?;
        let block = &self.decode_cache.blocks[index];
        Some((index, usize::from(block.count), block.check))
    }

    #[inline]
    pub(super) fn block_instruction(&self, index: usize, position: usize) -> Prepared {
        self.decode_cache.blocks[index].instructions[position]
    }

    /// The common interpreter/JIT batch dispatch. Only rare forms copy decoded IR.
    #[inline(always)]
    pub(super) fn try_prepared<B: CpuBus>(
        &mut self,
        bus: &mut B,
        windows: &mut Windows,
        code: SegmentWindow,
        instruction: Prepared,
        slot: usize,
    ) -> bool {
        if instruction.is_fallback() {
            self.try_batched(bus, windows, code, self.decode_cache.fallback[slot])
        } else {
            instruction.execute(self, bus, windows, code)
        }
    }

    /// The CS execute window and empty data windows for one slice.
    pub(super) fn batch_context(&mut self) -> Option<(SegmentWindow, Windows)> {
        let (code, _) = self.decode_cache.code_context(&self.registers)?;
        Some((code, Windows::default()))
    }

    /// The per-step checks of `step` are invariant across a batch: batched
    /// instructions cannot set TF, change CR0, load segments or open a shadow.
    fn batch_ready(&self) -> bool {
        !self.halted
            && self.interrupt_shadow == 0
            && self.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) == 0
            && self.control.cr0 & 0x8000_0000 == 0
            && self.registers.protected == self.protected_mode()
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    disp32_offset: u8,
    imm32_offset: u8,
}
impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.position)?;
        self.position += 1;
        Some(byte)
    }
    fn word(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes([self.byte()?, self.byte()?]))
    }
    fn dword(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes([
            self.byte()?,
            self.byte()?,
            self.byte()?,
            self.byte()?,
        ]))
    }
    fn disp32(&mut self) -> Option<u32> {
        self.disp32_offset = self.position as u8;
        self.dword()
    }
    fn immediate_width(&mut self, width: u8) -> Option<u32> {
        match width {
            4 => self.dword(),
            2 => self.word().map(u32::from),
            _ => self.byte().map(u32::from),
        }
    }
    fn immediate_data(&mut self, width: u8) -> Option<u32> {
        if width == 4 {
            self.imm32_offset = self.position as u8;
        }
        self.immediate_width(width)
    }
    fn target(
        &mut self,
        modrm: u8,
        address32: bool,
        override_segment: Option<SegmentRegister>,
    ) -> Option<Target> {
        let mode = modrm >> 6;
        let code = modrm & 7;
        if mode == 3 {
            return Some(Target::Register(code));
        }
        let (base, index, scale, mut displacement, stack) = if address32 {
            if code == 4 {
                let sib = self.byte()?;
                let base = sib & 7;
                let index = (sib >> 3) & 7;
                let (base, displacement, stack) = if base == 5 && mode == 0 {
                    (NO_REGISTER, self.disp32()?, false)
                } else {
                    (base, 0, matches!(base, 4 | 5))
                };
                (
                    base,
                    if index == 4 { NO_REGISTER } else { index },
                    sib >> 6,
                    displacement,
                    stack,
                )
            } else if code == 5 && mode == 0 {
                (NO_REGISTER, NO_REGISTER, 0, self.disp32()?, false)
            } else {
                (code, NO_REGISTER, 0, 0, code == 5)
            }
        } else {
            let (base, index, stack) = match code {
                0 => (3, 6, false),
                1 => (3, 7, false),
                2 => (5, 6, true),
                3 => (5, 7, true),
                4 => (6, NO_REGISTER, false),
                5 => (7, NO_REGISTER, false),
                6 if mode == 0 => (NO_REGISTER, NO_REGISTER, false),
                6 => (5, NO_REGISTER, true),
                _ => (3, NO_REGISTER, false),
            };
            (
                base,
                index,
                0,
                if mode == 0 && code == 6 {
                    u32::from(self.word()?)
                } else {
                    0
                },
                stack,
            )
        };
        displacement = displacement.wrapping_add(match mode {
            1 => self.byte()? as i8 as i32 as u32,
            2 if address32 => self.disp32()?,
            2 => u32::from(self.word()?),
            _ => 0,
        });
        Some(Target::Memory {
            base,
            index,
            scale,
            displacement,
            address32,
            segment: override_segment.unwrap_or(if stack {
                SegmentRegister::Ss
            } else {
                SegmentRegister::Ds
            }),
        })
    }
}
impl Instruction {
    /// One byte of offset range: the reference path reports its own faults.
    const UNSUPPORTED: Self = Self {
        length: 1,
        read_length: 1,
        dword: false,
        width: 1,
        branch32: true,
        disp32_offset: u8::MAX,
        imm32_offset: u8::MAX,
        operation: Operation::Unsupported,
    };

    #[cfg(all(
        feature = "dynamic-core",
        not(target_arch = "wasm32"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fn decode(bytes: &[u8]) -> Option<Self> {
        Self::decode_mode(bytes, true)
    }

    /// Block decoding additionally accepts the batched 387 subset. Other
    /// decoder users keep rejecting x87 bytes, as before that subset existed.
    fn decode_block(bytes: &[u8], code32: bool, x87: bool) -> Option<Self> {
        Self::decode_mode(bytes, code32).or_else(|| {
            if x87 {
                Self::decode_x87(bytes, code32)
            } else {
                None
            }
        })
    }

    /// Prefixes as in the reference step; operand size does not affect the
    /// batched forms. Repeat prefixes are left to the reference path.
    fn decode_x87(bytes: &[u8], code32: bool) -> Option<Self> {
        let mut r = Reader {
            bytes,
            position: 0,
            disp32_offset: u8::MAX,
            imm32_offset: u8::MAX,
        };
        let mut address32 = code32;
        let mut segment = None;
        let opcode = loop {
            match r.byte()? {
                0x66 => {}
                0x67 => address32 = !code32,
                byte @ (0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65) => {
                    segment = Some(match byte {
                        0x26 => SegmentRegister::Es,
                        0x2E => SegmentRegister::Cs,
                        0x36 => SegmentRegister::Ss,
                        0x64 => SegmentRegister::Fs,
                        0x65 => SegmentRegister::Gs,
                        _ => SegmentRegister::Ds,
                    })
                }
                byte => break byte,
            }
        };
        if !(0xD8..=0xDF).contains(&opcode) {
            return None;
        }
        let modrm = r.byte()?;
        let x87 = X87::decode(opcode, modrm)?;
        let target = r.target(modrm, address32, segment)?;
        if r.position > 15 {
            return None;
        }
        Some(Self {
            length: r.position as u8,
            read_length: r.position as u8,
            dword: false,
            width: 1,
            branch32: code32,
            // A patched disp32 updates the prepared displacement in place;
            // no 387 form has an immediate.
            disp32_offset: r.disp32_offset,
            imm32_offset: r.imm32_offset,
            operation: Operation::X87 { x87, target },
        })
    }

    fn decode_mode(bytes: &[u8], code32: bool) -> Option<Self> {
        let mut r = Reader {
            bytes,
            position: 0,
            disp32_offset: u8::MAX,
            imm32_offset: u8::MAX,
        };
        let mut operand32 = code32;
        let mut address32 = code32;
        let mut segment = None;
        let mut repeat = false;
        let opcode = loop {
            match r.byte()? {
                0x66 => operand32 = !code32,
                0x67 => address32 = !code32,
                0xF2 | 0xF3 => repeat = true,
                byte @ (0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65) => {
                    segment = Some(match byte {
                        0x26 => SegmentRegister::Es,
                        0x2E => SegmentRegister::Cs,
                        0x36 => SegmentRegister::Ss,
                        0x64 => SegmentRegister::Fs,
                        0x65 => SegmentRegister::Gs,
                        _ => SegmentRegister::Ds,
                    })
                }
                byte => break byte,
            }
        };
        if matches!(opcode, 0xA4 | 0xA5 | 0xAA | 0xAB | 0xAC | 0xAD) {
            // REPNE repeats MOVS/STOS/LODS exactly like REP.
            let width = if opcode & 1 == 0 {
                1
            } else if operand32 {
                4
            } else {
                2
            };
            if r.position > 15 {
                return None;
            }
            return Some(Self {
                length: r.position as u8,
                read_length: r.position as u8,
                dword: width == 4,
                width,
                branch32: code32,
                disp32_offset: u8::MAX,
                imm32_offset: u8::MAX,
                operation: Operation::String {
                    kind: opcode & 0xFE,
                    width,
                    segment: segment.unwrap_or(SegmentRegister::Ds),
                    address32,
                    repeat,
                },
            });
        }
        let mut dword = opcode & 1 != 0;
        let mut branch32 = operand32;
        let mut read_length = None;
        let operation = match opcode {
            0x88..=0x8B | 0x8D | 0xC6 | 0xC7 | 0x80 | 0x81 | 0x83 | 0x84..=0x87 => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                let register = (modrm >> 3) & 7;
                let source = Value::Operand(Target::Register(register));
                match opcode {
                    0x88 | 0x89 => Operation::Move { target, source },
                    0x8A | 0x8B => Operation::Move {
                        target: Target::Register(register),
                        source: Value::Operand(target),
                    },
                    0x8D if matches!(target, Target::Memory { .. }) => {
                        Operation::Lea { register, target }
                    }
                    0xC6 | 0xC7 if register == 0 => Operation::Move {
                        target,
                        source: Value::Immediate(r.immediate_data(if dword {
                            if operand32 { 4 } else { 2 }
                        } else {
                            1
                        })?),
                    },
                    0x86 | 0x87 => Operation::Exchange { target, register },
                    0x80 | 0x81 | 0x83 | 0x84 | 0x85 => {
                        if operand32 && matches!(opcode, 0x81 | 0x83) {
                            read_length = Some(r.position as u8);
                        }
                        let source = match opcode {
                            0x80 | 0x81 => Value::Immediate(r.immediate_data(if dword {
                                if operand32 { 4 } else { 2 }
                            } else {
                                1
                            })?),
                            0x83 => Value::Immediate(r.byte()? as i8 as i32 as u32),
                            _ => source,
                        };
                        let operation = if opcode >= 0x84 { 4 } else { register };
                        Operation::Alu {
                            target,
                            source,
                            operation,
                            store: opcode < 0x84 && operation != 7,
                        }
                    }
                    _ => return None,
                }
            }
            code if code <= 0x3D && code & 7 <= 5 => {
                let operation = code >> 3;
                let (target, source) = if code & 7 >= 4 {
                    (
                        Target::Register(0),
                        Value::Immediate(r.immediate_data(if dword {
                            if operand32 { 4 } else { 2 }
                        } else {
                            1
                        })?),
                    )
                } else {
                    let modrm = r.byte()?;
                    let rm = r.target(modrm, address32, segment)?;
                    let reg = Target::Register((modrm >> 3) & 7);
                    if code & 2 != 0 {
                        (reg, Value::Operand(rm))
                    } else {
                        (rm, Value::Operand(reg))
                    }
                };
                Operation::Alu {
                    target,
                    source,
                    operation,
                    store: operation != 7,
                }
            }
            0xC0 | 0xC1 | 0xD0..=0xD3 => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                let count = match opcode {
                    0xC0 | 0xC1 => Some(r.byte()?),
                    0xD2 | 0xD3 => None,
                    _ => Some(1),
                };
                Operation::Shift {
                    target,
                    operation: (modrm >> 3) & 7,
                    count,
                }
            }
            0x40..=0x4F => {
                dword = true;
                Operation::Inc {
                    register: opcode & 7,
                    decrement: opcode >= 0x48,
                }
            }
            0x50..=0x57 => Operation::Push(Value::Operand(Target::Register(opcode & 7))),
            0x58..=0x5F => Operation::Pop(opcode & 7),
            0x68 => Operation::Push(Value::Immediate(r.immediate_width(if operand32 {
                4
            } else {
                2
            })?)),
            0x6A => Operation::Push(Value::Immediate(r.byte()? as i8 as i32 as u32)),
            0x90..=0x97 => {
                dword = true;
                Operation::Exchange {
                    target: Target::Register(0),
                    register: opcode & 7,
                }
            }
            0xA0..=0xA3 => {
                let displacement = if address32 {
                    r.disp32()?
                } else {
                    u32::from(r.word()?)
                };
                let memory = Target::Memory {
                    segment: segment.unwrap_or(SegmentRegister::Ds),
                    base: NO_REGISTER,
                    index: NO_REGISTER,
                    scale: 0,
                    displacement,
                    address32,
                };
                let (target, source) = if opcode < 0xA2 {
                    (Target::Register(0), Value::Operand(memory))
                } else {
                    (memory, Value::Operand(Target::Register(0)))
                };
                Operation::Move { target, source }
            }
            0xA8 | 0xA9 => Operation::Alu {
                target: Target::Register(0),
                source: Value::Immediate(r.immediate_data(if dword {
                    if operand32 { 4 } else { 2 }
                } else {
                    1
                })?),
                operation: 4,
                store: false,
            },
            0xB0..=0xBF => {
                dword = opcode >= 0xB8;
                Operation::Move {
                    target: Target::Register(opcode & 7),
                    source: Value::Immediate(r.immediate_data(if dword {
                        if operand32 { 4 } else { 2 }
                    } else {
                        1
                    })?),
                }
            }
            0x70..=0x7F | 0xEB => {
                branch32 = code32;
                Operation::Branch {
                    condition: if opcode == 0xEB {
                        None
                    } else {
                        Some(opcode & 15)
                    },
                    displacement: i32::from(r.byte()? as i8),
                }
            }
            0x0F => {
                let second = r.byte()?;
                if matches!(second, 0xB6 | 0xB7 | 0xBE | 0xBF) {
                    let modrm = r.byte()?;
                    dword = true;
                    Operation::MoveExtend {
                        register: (modrm >> 3) & 7,
                        source: r.target(modrm, address32, segment)?,
                        word: second & 1 != 0,
                        signed: second & 8 != 0,
                    }
                } else if matches!(second, 0xA4 | 0xA5 | 0xAC | 0xAD) {
                    let modrm = r.byte()?;
                    let target = r.target(modrm, address32, segment)?;
                    let count = if second & 1 == 0 {
                        Some(r.byte()?)
                    } else {
                        None
                    };
                    // Undefined word counts retain the reference error and its exact IP.
                    if !operand32 && count.is_some_and(|c| c & 31 > 16) {
                        return None;
                    }
                    dword = true;
                    Operation::DoubleShift {
                        target,
                        register: (modrm >> 3) & 7,
                        right: second & 8 != 0,
                        count,
                    }
                } else if (0x80..=0x8F).contains(&second) {
                    Operation::Branch {
                        condition: Some(second & 15),
                        displacement: r.immediate_width(if operand32 { 4 } else { 2 })? as i32,
                    }
                } else if matches!(second, 0xA0 | 0xA8) {
                    dword = true;
                    Operation::PushSegment(if second == 0xA0 {
                        SegmentRegister::Fs
                    } else {
                        SegmentRegister::Gs
                    })
                } else if second == 0xBA {
                    let modrm = r.byte()?;
                    let target = r.target(modrm, address32, segment)?;
                    let operation = (modrm >> 3) & 7;
                    if operation < 4 {
                        return None;
                    }
                    dword = true;
                    Operation::BitTest {
                        target,
                        operation,
                        bit: r.byte()?,
                    }
                } else {
                    return None;
                }
            }
            0x8C | 0x8F => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                let register = (modrm >> 3) & 7;
                dword = true;
                if opcode == 0x8F {
                    if register != 0 {
                        return None;
                    }
                    Operation::PopTarget(target)
                } else {
                    Operation::StoreSegment {
                        target,
                        segment: match register {
                            0 => SegmentRegister::Es,
                            1 => SegmentRegister::Cs,
                            2 => SegmentRegister::Ss,
                            3 => SegmentRegister::Ds,
                            4 => SegmentRegister::Fs,
                            5 => SegmentRegister::Gs,
                            _ => return None,
                        },
                    }
                }
            }
            0x06 | 0x0E | 0x16 | 0x1E => {
                dword = true;
                Operation::PushSegment(match opcode {
                    0x06 => SegmentRegister::Es,
                    0x0E => SegmentRegister::Cs,
                    0x16 => SegmentRegister::Ss,
                    _ => SegmentRegister::Ds,
                })
            }
            0xC9 => {
                dword = true;
                Operation::Leave
            }
            0xC8 => {
                let allocation = r.word()?;
                if r.byte()? & 31 != 0 {
                    return None;
                }
                dword = true;
                Operation::Enter(allocation)
            }
            0xE0..=0xE3 => {
                branch32 = code32;
                Operation::Loop {
                    kind: opcode & 3,
                    displacement: i32::from(r.byte()? as i8),
                    address32,
                }
            }
            0x98 | 0x99 => {
                dword = true;
                Operation::Extend {
                    cdq: opcode == 0x99,
                }
            }
            0xE8 => Operation::Call(r.immediate_width(if operand32 { 4 } else { 2 })? as i32),
            0xE9 => Operation::Branch {
                condition: None,
                displacement: r.immediate_width(if operand32 { 4 } else { 2 })? as i32,
            },
            0xC2 => Operation::Return(r.word()?),
            0xC3 => Operation::Return(0),
            0xFE | 0xFF => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                match (modrm >> 3) & 7 {
                    op @ 0..=1 => Operation::IncTarget {
                        target,
                        decrement: op == 1,
                    },
                    4 if opcode == 0xFF => Operation::JumpIndirect(Value::Operand(target)),
                    2 if opcode == 0xFF => Operation::CallIndirect(Value::Operand(target)),
                    6 if opcode == 0xFF => Operation::Push(Value::Operand(target)),
                    _ => return None,
                }
            }
            0xF6 | 0xF7 => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                match (modrm >> 3) & 7 {
                    0 => {
                        read_length = Some(r.position as u8);
                        Operation::Alu {
                            target,
                            source: Value::Immediate(r.immediate_data(if dword {
                                if operand32 { 4 } else { 2 }
                            } else {
                                1
                            })?),
                            operation: 4,
                            store: false,
                        }
                    }
                    2 | 3 if dword => Operation::Unary {
                        target,
                        negate: modrm & 0x38 == 0x18,
                    },
                    operation @ 4..=7 => Operation::MulDiv { target, operation },
                    _ => return None,
                }
            }
            0xEC => Operation::Input,
            0xEE => Operation::Output,
            _ => return None,
        };
        if r.position > 15 {
            return None;
        }
        if matches!(
            operation,
            Operation::Push(_)
                | Operation::Pop(_)
                | Operation::Call(_)
                | Operation::CallIndirect(_)
                | Operation::JumpIndirect(_)
                | Operation::Return(_)
                | Operation::MoveExtend { .. }
                | Operation::Extend { .. }
        ) {
            dword = true;
        }
        let width = if dword {
            if operand32 { 4 } else { 2 }
        } else {
            1
        };
        Some(Self {
            length: r.position as u8,
            read_length: read_length.unwrap_or(r.position as u8),
            dword: width == 4,
            width,
            branch32,
            disp32_offset: r.disp32_offset,
            imm32_offset: r.imm32_offset,
            operation,
        })
    }
}

/// Share decoding with the native backend; unsupported or faultable operations
/// terminate a native block before any guest state changes.
#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) fn native_instruction(
    bytes: &[u8],
    block_offset: u8,
) -> Option<doslet_x86_jit::Instruction> {
    use doslet_x86_jit::{Instruction as Native, Operation as Op, Source};
    let i = Instruction::decode(bytes)?;
    if i.width == 2 || !i.branch32 {
        return None;
    }
    // Branches have no byte/dword data operand. For short Jcc the decoder's
    // generic width bit follows opcode bit 0, so both values must be accepted.
    // The shared decoder has already rejected operand-size override 66h.
    if !i.dword && !matches!(i.operation, Operation::Branch { .. }) {
        return None;
    }
    let source = |v| match v {
        Value::Immediate(v) => Some(Source::Immediate(v)),
        Value::Operand(Target::Register(r)) => Some(Source::Register(r)),
        _ => None,
    };
    let operation = match i.operation {
        Operation::Move {
            target: Target::Register(target),
            source: s,
        } => Op::Move {
            target,
            source: source(s)?,
        },
        Operation::Alu {
            target: Target::Register(target),
            source: s,
            operation,
            store,
        } => Op::Alu {
            target,
            source: source(s)?,
            operation,
            store,
        },
        Operation::Inc {
            register,
            decrement,
        } => Op::Inc {
            register,
            decrement,
        },
        Operation::Exchange {
            target: Target::Register(left),
            register: right,
        } => Op::Exchange { left, right },
        Operation::Unary {
            target: Target::Register(register),
            negate,
        } => Op::Unary { register, negate },
        Operation::Branch {
            condition,
            displacement,
        } if block_offset != 0
            && i64::from(block_offset) + i64::from(i.length) + i64::from(displacement) == 0 =>
        {
            Op::LoopBranch { condition }
        }
        _ => return None,
    };
    Some(Native {
        length: i.length,
        operation,
    })
}
impl Cpu {
    pub(super) fn cached_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
    ) -> Option<Result<StepOutcome, CpuError>> {
        let instruction = self.decode_cache.instruction(&self.registers, bus)?;
        if let Operation::DoubleShift { count, .. } = instruction.operation
            && instruction.width == 2
            && count.unwrap_or(self.registers.get_byte(1)) & 31 > 16
        {
            return None;
        }
        Some(self.execute_cached(bus, instruction))
    }
    fn execute_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        instruction: Instruction,
    ) -> Result<StepOutcome, CpuError> {
        let ip = self.registers.ip;
        let next = ip.wrapping_add(u32::from(instruction.length));
        self.registers.ip = ip.wrapping_add(u32::from(instruction.read_length));
        let dword = instruction.dword;
        let width = instruction.width;
        let jump = |displacement: i32| {
            let target = next.wrapping_add_signed(displacement);
            if instruction.branch32 {
                target
            } else {
                u32::from(target as u16)
            }
        };
        match instruction.operation {
            Operation::Move { target, source } => {
                let value = source.read(self, bus, width)?;
                write_width(target.resolve(&self.registers), self, bus, width, value)?;
            }
            Operation::Lea { register, target } => {
                let Operand::Memory { offset, .. } = target.resolve(&self.registers) else {
                    unreachable!()
                };
                write_width(Operand::Register(register), self, bus, width, offset)?;
            }
            Operation::Alu {
                target,
                source,
                operation,
                store,
            } => {
                let target = target.resolve(&self.registers);
                let left = read_width(target, self, bus, width)?;
                self.registers.ip = next;
                let right = source.read(self, bus, width)?;
                let (value, new_flags) = alu_width(self.flags, left, right, operation, width);
                self.flags = new_flags;
                if store {
                    write_width(target, self, bus, width, value)?;
                }
            }
            Operation::Exchange { target, register } => {
                let target = target.resolve(&self.registers);
                let source = Operand::Register(register);
                let left = read_width(target, self, bus, width)?;
                let right = read_width(source, self, bus, width)?;
                write_width(target, self, bus, width, right)?;
                write_width(source, self, bus, width, left)?;
            }
            Operation::Inc {
                register,
                decrement,
            } => {
                self.cached_inc(bus, Operand::Register(register), decrement, width)?;
            }
            Operation::IncTarget { target, decrement } => {
                self.cached_inc(bus, target.resolve(&self.registers), decrement, width)?;
            }
            Operation::Shift {
                target,
                operation,
                count,
            } => {
                self.cached_shift(
                    bus,
                    target.resolve(&self.registers),
                    operation,
                    count.unwrap_or(self.registers.get_byte(1)) & 31,
                    width,
                )?;
            }
            Operation::DoubleShift {
                target,
                register,
                right,
                count,
            } => {
                let count = count.unwrap_or(self.registers.get_byte(1)) & 31;
                if count != 0 {
                    let target = target.resolve(&self.registers);
                    let original = read_width(target, self, bus, width)?;
                    let source = read_width(Operand::Register(register), self, bus, width)?;
                    let bits = width * 8;
                    let mask = if width == 4 { u32::MAX } else { 0xFFFF };
                    let (value, carry) = if right {
                        (
                            (((u64::from(source) << bits) | u64::from(original)) >> count) as u32
                                & mask,
                            (original >> (count - 1)) & 1,
                        )
                    } else {
                        (
                            ((((u64::from(original) << bits) | u64::from(source)) << count) >> bits)
                                as u32
                                & mask,
                            (original >> (bits - count)) & 1,
                        )
                    };
                    let mut updated = alu_width(self.flags, value, 0, 1, width).1;
                    updated = (updated & !(flags::CARRY | flags::AUXILIARY | flags::OVERFLOW))
                        | carry
                        | (self.flags & (flags::AUXILIARY | flags::OVERFLOW));
                    if count == 1 {
                        updated = (updated & !flags::OVERFLOW)
                            | if (original ^ value) & (1 << (bits - 1)) != 0 {
                                flags::OVERFLOW
                            } else {
                                0
                            };
                    }
                    write_width(target, self, bus, width, value)?;
                    self.flags = updated;
                }
            }
            Operation::Unary { target, negate } => {
                let target = target.resolve(&self.registers);
                let value = read_width(target, self, bus, width)?;
                let value = if negate {
                    let (value, new_flags) = alu_width(self.flags, 0, value, 5, width);
                    self.flags = new_flags;
                    value
                } else {
                    !value
                };
                write_width(target, self, bus, width, value)?;
            }
            Operation::Push(value) => {
                let value = value.read(self, bus, width)?;
                if dword {
                    self.push32(bus, value)?;
                } else {
                    self.push(bus, value as u16)?;
                }
            }
            Operation::Pop(register) => {
                let value = if dword {
                    self.pop32(bus)?
                } else {
                    u32::from(self.pop(bus)?)
                };
                write_width(Operand::Register(register), self, bus, width, value)?;
            }
            Operation::Branch {
                condition,
                displacement,
            } => {
                if condition.is_none_or(|condition| flags::condition(self.flags, condition)) {
                    self.near_jump(jump(displacement))?;
                }
            }
            Operation::Call(displacement) => {
                if dword {
                    self.near_jump(jump(displacement))?;
                    self.push32(bus, next)?;
                } else {
                    self.push(bus, next as u16)?;
                    self.near_jump(jump(displacement))?;
                }
            }
            Operation::JumpIndirect(target) => {
                let target = target.read(self, bus, width)?;
                self.near_jump(target)?;
            }
            Operation::CallIndirect(target) => {
                // FF /2 reads the target before pushing, and validates the
                // destination after the push, matching the reference's faults.
                let target = target.read(self, bus, width)?;
                if dword {
                    self.push32(bus, next)?;
                } else {
                    self.push(bus, next as u16)?;
                }
                self.near_jump(target)?;
            }
            Operation::Return(release) => {
                let target = if dword {
                    self.pop32(bus)?
                } else {
                    u32::from(self.pop(bus)?)
                };
                self.near_jump(target)?;
                self.adjust_stack(u32::from(release));
            }
            Operation::Input => {
                let value = bus.read_port_u8(self.registers.get32(Gpr32::Edx) as u16)?;
                self.registers.set_byte(0, value);
            }
            Operation::Output => bus.write_port_u8(
                self.registers.get32(Gpr32::Edx) as u16,
                self.registers.get_byte(0),
            )?,
            Operation::Loop {
                kind,
                displacement,
                address32,
            } => {
                self.registers.ip = next;
                let take = if kind == 3 {
                    self.address_register(Gpr16::Cx, address32) == 0
                } else {
                    let count = self.address_register(Gpr16::Cx, address32).wrapping_sub(1);
                    self.set_address_register(Gpr16::Cx, count, address32);
                    let count = self.address_register(Gpr16::Cx, address32);
                    count != 0 && (kind == 2 || (self.flags & flags::ZERO != 0) == (kind == 1))
                };
                if take {
                    self.near_jump(jump(displacement))?;
                }
            }
            Operation::String {
                kind,
                width,
                segment,
                address32,
                repeat,
            } => {
                self.registers.ip = next;
                let count = self.address_register(Gpr16::Cx, address32);
                if repeat && count == 0 {
                    return Ok(StepOutcome::Continue);
                }
                let si = self.address_register(Gpr16::Si, address32);
                let di = self.address_register(Gpr16::Di, address32);
                let source = Operand::Memory {
                    segment,
                    offset: si,
                };
                let target = Operand::Memory {
                    segment: SegmentRegister::Es,
                    offset: di,
                };
                match kind {
                    0xA4 => {
                        let value = read_width(source, self, bus, width)?;
                        write_width(target, self, bus, width, value)?;
                    }
                    0xAA => {
                        let value = read_width(Operand::Register(0), self, bus, width)?;
                        write_width(target, self, bus, width, value)?;
                    }
                    _ => {
                        let value = read_width(source, self, bus, width)?;
                        write_width(Operand::Register(0), self, bus, width, value)?;
                    }
                }
                let amount = i32::from(width);
                let delta = if self.flags & flags::DIRECTION == 0 {
                    amount
                } else {
                    -amount
                };
                if kind != 0xAA {
                    self.set_address_register(Gpr16::Si, si.wrapping_add_signed(delta), address32);
                }
                if kind != 0xAC {
                    self.set_address_register(Gpr16::Di, di.wrapping_add_signed(delta), address32);
                }
                if repeat {
                    self.set_address_register(Gpr16::Cx, count - 1, address32);
                    if count > 1 {
                        self.registers.ip = ip;
                    }
                }
            }
            Operation::MulDiv { target, operation } => {
                self.registers.ip = next;
                let value = read_width(target.resolve(&self.registers), self, bus, width)?;
                self.multiply_divide(operation, value, width, ip)?;
            }
            Operation::Extend { cdq } => {
                self.registers.ip = next;
                if !dword {
                    if cdq {
                        self.registers.set(
                            Gpr16::Dx,
                            if self.registers.get(Gpr16::Ax) & 0x8000 != 0 {
                                u16::MAX
                            } else {
                                0
                            },
                        );
                    } else {
                        self.registers
                            .set(Gpr16::Ax, self.registers.get_byte(0) as i8 as i16 as u16);
                    }
                } else if cdq {
                    let sign = self.registers.get32(Gpr32::Eax) & 0x8000_0000 != 0;
                    self.registers
                        .set32(Gpr32::Edx, if sign { u32::MAX } else { 0 });
                } else {
                    let value = self.registers.get(Gpr16::Ax) as i16 as i32 as u32;
                    self.registers.set32(Gpr32::Eax, value);
                }
            }
            Operation::MoveExtend {
                register,
                source,
                word,
                signed,
            } => {
                self.registers.ip = next;
                let value = source
                    .resolve(&self.registers)
                    .read(&self.registers, bus, word)?;
                let value = extend_result(u32::from(value), word, signed);
                write_width(Operand::Register(register), self, bus, width, value)?;
            }
            Operation::StoreSegment { target, segment } => {
                self.registers.ip = next;
                let value = self.registers.selector(segment);
                let target = target.resolve(&self.registers);
                if matches!(target, Operand::Register(_)) {
                    write_width(target, self, bus, width, u32::from(value))?;
                } else {
                    target.write(&mut self.registers, bus, true, value)?;
                }
            }
            Operation::PushSegment(segment) => {
                self.registers.ip = next;
                let value = self.registers.selector(segment);
                if dword {
                    self.push32(bus, u32::from(value))?;
                } else {
                    self.push(bus, value)?;
                }
            }
            Operation::PopTarget(target) => {
                self.registers.ip = next;
                let target = target.resolve(&self.registers);
                let value = if dword {
                    self.pop32(bus)?
                } else {
                    u32::from(self.pop(bus)?)
                };
                write_width(target, self, bus, width, value)?;
            }
            Operation::Leave => {
                self.registers.ip = next;
                if dword {
                    self.set_stack_pointer(self.registers.get32(Gpr32::Ebp));
                    let value = self.pop32(bus)?;
                    self.registers.set32(Gpr32::Ebp, value);
                } else {
                    self.set_stack_pointer(if self.registers.stack32() {
                        self.registers.get32(Gpr32::Ebp)
                    } else {
                        u32::from(self.registers.get(Gpr16::Bp))
                    });
                    let value = self.pop(bus)?;
                    self.registers.set(Gpr16::Bp, value);
                }
            }
            Operation::Enter(allocation) => {
                self.registers.ip = next;
                if dword {
                    let original = self.registers.get32(Gpr32::Ebp);
                    self.push32(bus, original)?;
                    let frame = self.stack_pointer();
                    self.registers.set32(Gpr32::Ebp, frame);
                    self.adjust_stack(u32::from(allocation).wrapping_neg());
                } else {
                    let original = self.registers.get(Gpr16::Bp);
                    self.push(bus, original)?;
                    let frame = self.registers.get(Gpr16::Sp);
                    self.registers.set(Gpr16::Bp, frame);
                    self.registers
                        .set(Gpr16::Sp, frame.wrapping_sub(allocation));
                }
            }
            Operation::BitTest {
                target,
                operation,
                bit,
            } => {
                self.registers.ip = next;
                let target = target.resolve(&self.registers);
                let bit = bit & if dword { 31 } else { 15 };
                let value = read_width(target, self, bus, width)?;
                let mask = 1 << bit;
                self.flags = (self.flags & !flags::CARRY) | u32::from(value & mask != 0);
                let value = match operation {
                    5 => value | mask,
                    6 => value & !mask,
                    7 => value ^ mask,
                    _ => value,
                };
                if operation != 4 {
                    write_width(target, self, bus, width, value)?;
                }
            }
            Operation::Unsupported => unreachable!("lookups return None for rejections"),
            Operation::X87 { .. } => unreachable!("x87 forms are decoded only into blocks"),
        }
        Ok(StepOutcome::Continue)
    }

    fn cached_inc<B: CpuBus>(
        &mut self,
        bus: &mut B,
        target: Operand,
        decrement: bool,
        width: u8,
    ) -> Result<(), CpuError> {
        let value = read_width(target, self, bus, width)?;
        let (value, updated) = increment_result(self.flags, value, decrement, width);
        self.flags = updated;
        write_width(target, self, bus, width, value)
    }

    fn cached_shift<B: CpuBus>(
        &mut self,
        bus: &mut B,
        target: Operand,
        operation: u8,
        count: u8,
        width: u8,
    ) -> Result<(), CpuError> {
        if count == 0 {
            return Ok(());
        }
        let original = read_width(target, self, bus, width)?;
        let (value, updated) = shift_result(self.flags, original, operation, count, width);
        self.flags = updated;
        write_width(target, self, bus, width, value)
    }

    /// F6/F7 /4-/7 on AL/AX or EAX, as the byte and dword reference paths.
    fn multiply_divide(
        &mut self,
        operation: u8,
        value: u32,
        width: u8,
        ip: u32,
    ) -> Result<(), CpuError> {
        let divide_error = CpuError::DivideError {
            cs: self.registers.cs,
            ip,
        };
        if width == 2 {
            // The word reference path: DX:AX, as F7 without an operand override.
            let ax = self.registers.get(Gpr16::Ax);
            let value = value as u16;
            if operation < 6 {
                let product = if operation == 4 {
                    u32::from(ax) * u32::from(value)
                } else {
                    (i32::from(ax as i16) * i32::from(value as i16)) as u32
                };
                let overflow = if operation == 4 {
                    product > 0xFFFF
                } else {
                    product as i32 != i32::from(product as i16)
                };
                self.registers.set(Gpr16::Ax, product as u16);
                self.registers.set(Gpr16::Dx, (product >> 16) as u16);
                self.set_multiply_overflow(overflow);
                return Ok(());
            }
            let raw = (u32::from(self.registers.get(Gpr16::Dx)) << 16) | u32::from(ax);
            let (dividend, divisor) = if operation == 7 {
                (i64::from(raw as i32), i64::from(value as i16))
            } else {
                (i64::from(raw), i64::from(value))
            };
            if divisor == 0 {
                return Err(divide_error);
            }
            let quotient = dividend / divisor;
            let remainder = dividend % divisor;
            let range = if operation == 7 {
                -32768..=32767
            } else {
                0..=65535
            };
            if !range.contains(&quotient) {
                return Err(divide_error);
            }
            self.registers.set(Gpr16::Ax, quotient as u16);
            self.registers.set(Gpr16::Dx, remainder as u16);
            return Ok(());
        }
        if width == 4 {
            if operation < 6 {
                let accumulator = self.registers.get32(Gpr32::Eax);
                let (product, overflow) = if operation == 4 {
                    let product = u64::from(accumulator) * u64::from(value);
                    (product, product >> 32 != 0)
                } else {
                    let product = i64::from(accumulator as i32) * i64::from(value as i32);
                    (product as u64, product != i64::from(product as i32))
                };
                self.registers.set32(Gpr32::Eax, product as u32);
                self.registers.set32(Gpr32::Edx, (product >> 32) as u32);
                self.set_multiply_overflow(overflow);
                return Ok(());
            }
            let dividend = (u64::from(self.registers.get32(Gpr32::Edx)) << 32)
                | u64::from(self.registers.get32(Gpr32::Eax));
            let result = if operation == 6 {
                dividend
                    .checked_div(u64::from(value))
                    .filter(|quotient| *quotient <= u64::from(u32::MAX))
                    .map(|quotient| (quotient as u32, (dividend % u64::from(value)) as u32))
            } else {
                (dividend as i64)
                    .checked_div(i64::from(value as i32))
                    .filter(|quotient| i32::try_from(*quotient).is_ok())
                    .map(|quotient| {
                        (
                            quotient as u32,
                            ((dividend as i64) % i64::from(value as i32)) as u32,
                        )
                    })
            };
            let (quotient, remainder) = result.ok_or(divide_error)?;
            self.registers.set32(Gpr32::Eax, quotient);
            self.registers.set32(Gpr32::Edx, remainder);
            return Ok(());
        }
        let ax = self.registers.get(Gpr16::Ax);
        let value = value as u8;
        if operation < 6 {
            let product = if operation == 4 {
                u32::from(ax as u8) * u32::from(value)
            } else {
                (i32::from(ax as u8 as i8) * i32::from(value as i8)) as u32
            };
            let overflow = if operation == 4 {
                product > 0xFF
            } else {
                product as i32 != i32::from(product as i8)
            };
            self.registers.set(Gpr16::Ax, product as u16);
            self.set_multiply_overflow(overflow);
            return Ok(());
        }
        let (dividend, divisor) = if operation == 7 {
            (i64::from(ax as i16), i64::from(value as i8))
        } else {
            (i64::from(ax), i64::from(value))
        };
        if divisor == 0 {
            return Err(divide_error);
        }
        let quotient = dividend / divisor;
        let remainder = dividend % divisor;
        let range = if operation == 7 { -128..=127 } else { 0..=255 };
        if !range.contains(&quotient) {
            return Err(divide_error);
        }
        self.registers.set_byte(0, quotient as u8);
        self.registers.set_byte(4, remainder as u8);
        Ok(())
    }

    fn set_multiply_overflow(&mut self, overflow: bool) {
        self.flags = (self.flags & !(flags::CARRY | flags::OVERFLOW))
            | if overflow {
                flags::CARRY | flags::OVERFLOW
            } else {
                0
            };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gpr32;
    use doslet_bus::{BusError, MemoryBus, PortBus};

    #[test]
    fn compact_blocks_keep_decoded_ir_off_the_hot_table() {
        assert_eq!(core::mem::size_of::<Prepared>(), 16);
        // Include both the hot block and the explicitly reserved cold IR in
        // the architecture's storage accounting (64-bit hosts and WASM32).
        assert_eq!(core::mem::size_of::<Block>(), 168);
        assert_eq!(core::mem::size_of::<Instruction>(), 36);
    }

    #[test]
    #[cfg(all(
        feature = "dynamic-core",
        not(target_arch = "wasm32"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fn prepared_native_eligibility_keeps_word_and_branch_guards() {
        for bytes in [
            &[0x66, 0xB8, 1, 0][..],
            &[0x66, 0xE8, 0, 0],
            &[0x66, 0xE9, 0, 0],
            &[0x66, 0x0F, 0x84, 0, 0],
        ] {
            let i = Instruction::decode(bytes).unwrap();
            assert!(i.width == 2 || !i.branch32);
            assert!(!Prepared::new(i).native_candidate());
            assert!(native_instruction(bytes, 0).is_none());
            assert!(native_trace_instruction(bytes, 0).is_none());
        }
    }

    /// Flat RAM whose single write generation validates every code span.
    struct Flat<const SIZE: usize = 0x200> {
        ram: [u8; SIZE],
        generation: u64,
    }
    impl<const SIZE: usize> MemoryBus for Flat<SIZE> {
        fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
            let byte = self.ram.get(address as usize);
            byte.copied().ok_or(BusError::UnmappedMemory(address))
        }
        fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
            let byte = self.ram.get_mut(address as usize);
            *byte.ok_or(BusError::UnmappedMemory(address))? = value;
            self.generation += 1;
            Ok(())
        }
        fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
            (address as usize + usize::from(length) <= self.ram.len()).then_some(CodeCheck {
                space: 1 << 32,
                pages: [0; 2],
                generations: [self.generation; 2],
            })
        }
        fn code_valid(&self, check: &CodeCheck) -> bool {
            check.generations[0] == self.generation
        }
        fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
            let start = address as usize;
            self.ram
                .get(start..(start + usize::from(length)).min(self.ram.len()))
        }
        fn is_batch_memory(&self, address: u32, length: u8) -> bool {
            address as usize + usize::from(length) <= self.ram.len()
        }
    }
    impl<const SIZE: usize> PortBus for Flat<SIZE> {
        fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
            Err(BusError::UnmappedPort(port))
        }
        fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
            Err(BusError::UnmappedPort(port))
        }
    }

    fn prepared_cpu() -> Cpu {
        let mut cpu = Cpu::default();
        cpu.set_protected_mode(true);
        for segment in SegmentRegister::ALL {
            cpu.registers.set_segment(
                segment,
                SegmentCache {
                    selector: if segment == SegmentRegister::Cs {
                        8
                    } else {
                        16
                    },
                    base: 0,
                    limit: u32::MAX,
                    access: if segment == SegmentRegister::Cs {
                        0x9B
                    } else {
                        0x93
                    },
                    flags: 0xC0,
                    default32: true,
                    usable: true,
                },
            );
        }
        cpu.registers.set32(Gpr32::Esp, 0x1F0);
        cpu
    }

    #[test]
    fn prepared_entries_split_blocks_warmed_before_registration() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        let mut cpu = prepared_cpu();
        assert_eq!(cpu.interpret_batch(&mut bus, 8, |_| 8), 8);
        assert_eq!(cpu.decode_cache.blocks[0].count, 8);
        cpu.set_prepared_entries(alloc::vec![3, 6]).unwrap();
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            3
        );
        assert_eq!(cpu.registers.ip, 3);
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        assert_eq!(cpu.decode_cache.blocks[0].count, 3);
        assert!(cpu.decode_cache.blocks[3].prepared_entry);
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            3
        );
        assert_eq!(cpu.registers.ip, 6);
        assert_eq!(cpu.prepared_entry_index(), Some(1));
        assert_eq!(cpu.registers.get32(Gpr32::Eax), 14);
    }

    #[test]
    fn prepared_entry_replacement_validates_before_mutation_and_clears_hints() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![3, 6]).unwrap();
        cpu.registers.ip = 3;
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        for invalid in [alloc::vec![6, 3], alloc::vec![3, 3]] {
            assert!(cpu.set_prepared_entries(invalid).is_err());
            assert_eq!(cpu.prepared_entry_index(), Some(0));
        }
        cpu.set_prepared_entries(alloc::vec![6]).unwrap();
        assert_eq!(cpu.prepared_entry_index(), None);
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            6
        );
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        cpu.set_prepared_entries(alloc::vec![]).unwrap();
        assert!(cpu.decode_cache.prepared_entries.is_none());
        assert_eq!(cpu.prepared_entry_index(), None);
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            32
        );
    }

    #[test]
    fn declined_prepared_entry_repeatedly_makes_bounded_progress() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        bus.ram[..2].copy_from_slice(&[0xEB, 0xFE]);
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![0]).unwrap();
        for _ in 0..8 {
            assert_eq!(cpu.prepared_entry_index(), Some(0));
            assert_eq!(
                cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
                1
            );
            assert_eq!(cpu.registers.ip, 0);
        }
    }

    #[test]
    fn prepared_entries_stop_direct_and_indirect_calls_jumps_and_returns() {
        for bytes in [
            &[0xE8, 0x1B, 0, 0, 0][..], // CALL 20h
            &[0xFF, 0xD0],              // CALL EAX
            &[0xFF, 0xE0],              // JMP EAX
            &[0xC3],                    // RET
        ] {
            let mut bus = Flat {
                ram: [0x40; 0x200],
                generation: 0,
            };
            bus.ram[..bytes.len()].copy_from_slice(bytes);
            bus.ram[0x1F0..0x1F4].copy_from_slice(&0x20_u32.to_le_bytes());
            let mut cpu = prepared_cpu();
            cpu.registers.set32(Gpr32::Eax, 0x20);
            cpu.set_prepared_entries(alloc::vec![0x20]).unwrap();
            assert_eq!(
                cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
                1,
                "{bytes:02X?}"
            );
            assert_eq!(cpu.registers.ip, 0x20);
            assert_eq!(cpu.prepared_entry_index(), Some(0));
            assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x20);
        }
    }

    #[test]
    fn unaligned_registry_address_cannot_hide_later_fallthrough_entry() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        bus.ram[..5].copy_from_slice(&[0xB8, 0, 0, 0, 0]);
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![1, 6]).unwrap();
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            2
        );
        assert_eq!(cpu.registers.ip, 6);
        assert_eq!(cpu.prepared_entry_index(), Some(1));
    }

    #[test]
    fn prepared_hints_and_block_annotations_survive_slot_collisions() {
        let mut bus = Flat {
            ram: [0x40; 0x2000],
            generation: 0,
        };
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![0, 2, 0x1004]).unwrap();
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        cpu.registers.ip = 0x201; // Same membership-hint slot as EIP zero.
        assert_eq!(cpu.prepared_entry_index(), None);
        cpu.registers.ip = 0;
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 8, |_| 8),
            2
        );
        cpu.registers.ip = 0x1004; // Same decoded-block slot as EIP zero.
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 8, |_| 8),
            8
        );
        assert_eq!(cpu.decode_cache.blocks[0].address, 0x1004);
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 8, |_| 8),
            2
        );
        assert_eq!(cpu.registers.ip, 2);
        assert_eq!(cpu.prepared_entry_index(), Some(1));
    }

    #[test]
    fn code_rewrites_rebuild_entry_boundaries_but_keep_membership_hints() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![3]).unwrap();
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            3
        );
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        for (address, byte) in [0xB8, 0, 0, 0, 0].into_iter().enumerate() {
            bus.write_u8(address as u32, byte).unwrap();
        }
        // The old entry now lies in MOV's immediate, so no boundary exists.
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 2, |_| 2),
            2
        );
        assert_eq!(cpu.registers.ip, 6);
        cpu.registers.ip = 3;
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        for address in 0..5 {
            bus.write_u8(address, 0x40).unwrap();
        }
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            3
        );
        assert_eq!(cpu.prepared_entry_index(), Some(0));
    }

    #[test]
    fn prepared_entry_annotations_remain_safe_across_cs_aliases_and_modes() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![0x20]).unwrap();
        let flat = cpu.registers.segment_cache(SegmentRegister::Cs);
        cpu.registers.ip = 0x10;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 48, |_| 48),
            16
        );
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        cpu.registers
            .set_segment(SegmentRegister::Cs, SegmentCache { base: 0x10, ..flat });
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 48, |_| 48),
            48
        );
        cpu.registers.ip = 0x20;
        assert_eq!(cpu.prepared_entry_index(), None);
        cpu.registers.set_segment(
            SegmentRegister::Cs,
            SegmentCache {
                default32: false,
                flags: 0x80,
                ..flat
            },
        );
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 48, |_| 48),
            48
        );
        cpu.registers.ip = 0x20;
        assert_eq!(cpu.prepared_entry_index(), None);
        cpu.registers.set_segment(SegmentRegister::Cs, flat);
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 48, |_| 48),
            32
        );
        assert_eq!(cpu.prepared_entry_index(), Some(0));
    }

    #[test]
    fn prepared_registry_survives_disabled_cache_and_profile_replacement() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![0, 3]).unwrap();
        cpu.set_decode_cache(false);
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            0
        );
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.registers.ip, 1);
        cpu.set_decode_cache(true);
        cpu.set_execution_labels(Some(alloc::vec![])).unwrap();
        cpu.registers.ip = 0;
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
            3
        );
        assert_eq!(cpu.prepared_entry_index(), Some(1));
        assert_eq!(cpu.execution_counts(), [0, 0, 3, 0]);
        cpu.set_execution_labels(None).unwrap();
        assert_eq!(cpu.prepared_entry_index(), Some(1));
    }

    #[test]
    fn prepared_batches_keep_deadlines_and_do_not_allocate_during_execution() {
        let mut bus = Flat {
            ram: [0x40; 0x200],
            generation: 0,
        };
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![7]).unwrap();
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 2),
            2
        );
        assert_eq!(cpu.registers.ip, 2);
        assert_eq!(
            cpu.interpret_batch_until_prepared_entry(&mut bus, 1, |_| 32),
            0
        );
        let info = allocation_counter::measure(|| {
            for _ in 0..8 {
                cpu.registers.ip = 0;
                assert_eq!(cpu.prepared_entry_index(), None);
                assert_eq!(
                    cpu.interpret_batch_until_prepared_entry(&mut bus, 32, |_| 32),
                    7
                );
                assert_eq!(cpu.prepared_entry_index(), Some(0));
            }
        });
        assert_eq!(info.count_total, 0);
    }

    #[test]
    #[cfg(feature = "save-state")]
    fn restored_cpu_drops_prepared_registry() {
        use doslet_state::State;
        let mut cpu = prepared_cpu();
        cpu.set_prepared_entries(alloc::vec![0]).unwrap();
        assert_eq!(cpu.prepared_entry_index(), Some(0));
        let mut restored = <Cpu as State>::load(cpu.save()).unwrap();
        assert_eq!(restored.prepared_entry_index(), None);
        assert!(restored.decode_cache.prepared_entries.is_none());
    }

    #[test]
    fn exhausted_cs_lineage_or_version_resolves_the_context_every_step() {
        for exhausted in [
            |r: &mut Registers| r.cs_lineage = 0,
            |r: &mut Registers| r.cs_version = u64::MAX,
        ] {
            let mut bus = Flat {
                ram: [0; 0x200],
                generation: 0,
            };
            bus.ram[..5].copy_from_slice(&[0xB8, 1, 0, 0, 0]);
            bus.ram[0x100..0x105].copy_from_slice(&[0xB8, 2, 0, 0, 0]);
            let mut cpu = Cpu::default();
            cpu.control.cr0 |= 1;
            let r = &mut cpu.registers;
            r.sync_mode(true);
            r.cs = 8;
            r.caches[SegmentRegister::Cs as usize] = SegmentCache {
                selector: 8,
                base: 0,
                limit: 0x1FF,
                access: 0x9B,
                flags: 0x40,
                default32: true,
                usable: true,
            };
            exhausted(r);
            cpu.step(&mut bus).unwrap();
            assert_eq!(cpu.registers.get32(Gpr32::Eax), 1);
            // Rebase CS without a version change; only a full check notices.
            cpu.registers.caches[SegmentRegister::Cs as usize].base = 0x100;
            cpu.registers.caches[SegmentRegister::Cs as usize].limit = 0xFF;
            cpu.registers.ip = 0;
            cpu.step(&mut bus).unwrap();
            assert_eq!(cpu.registers.get32(Gpr32::Eax), 2);
        }
    }

    #[test]
    fn self_modifying_immediates_and_displacements_map_to_virtual_registers_without_flushing_blocks()
     {
        let mut bus = Flat {
            ram: [0x90; 0x200],
            generation: 0,
        };
        // Block at 0x00:
        //   0x00: 89 1D 0E 00 00 00   mov [0x0E], ebx  (patches imm32 of add ebp, imm32)
        //   0x06: 89 0D 14 00 00 00   mov [0x14], ecx  (patches disp32 of mov eax, [edx + disp32])
        //   0x0C: 81 C5 78 56 34 12   add ebp, 0x12345678  (imm32 at 0x0E)
        //   0x12: 8B 82 78 56 34 12   mov eax, [edx + 0x12345678]  (disp32 at 0x14)
        let code = [
            0x89, 0x1D, 0x0E, 0x00, 0x00, 0x00, // mov [0x0E], ebx
            0x89, 0x0D, 0x14, 0x00, 0x00, 0x00, // mov [0x14], ecx
            0x81, 0xC5, 0x78, 0x56, 0x34, 0x12, // add ebp, 0x12345678
            0x8B, 0x82, 0x78, 0x56, 0x34, 0x12, // mov eax, [edx + 0x12345678]
        ];
        bus.ram[..code.len()].copy_from_slice(&code);
        bus.ram[0x80..0x84].copy_from_slice(&0xCAFE_BABEu32.to_le_bytes());
        bus.ram[0x84..0x88].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        let mut cpu = prepared_cpu();
        cpu.registers.set32(Gpr32::Ebp, 10);
        cpu.registers.set32(Gpr32::Edx, 0);
        cpu.registers.set32(Gpr32::Ebx, 25);
        cpu.registers.set32(Gpr32::Ecx, 0x80);

        // First run: decodes the block once, binds 0x0E and 0x14 as virtual registers,
        // and executes all 4 instructions in a single basic-block pass without flushing.
        assert_eq!(cpu.interpret_batch(&mut bus, 4, |_| 4), 4);
        assert_eq!(cpu.registers.ip, 0x18);
        assert_eq!(cpu.registers.get32(Gpr32::Ebp), 35);
        assert_eq!(cpu.registers.get32(Gpr32::Eax), 0xCAFE_BABE);

        // Second run with new self-modified values: zero allocations, zero re-decodes.
        cpu.registers.ip = 0;
        cpu.registers.set32(Gpr32::Ebx, 100);
        cpu.registers.set32(Gpr32::Ecx, 0x84);
        let info = allocation_counter::measure(|| {
            assert_eq!(cpu.interpret_batch(&mut bus, 4, |_| 4), 4);
        });
        assert_eq!(info.count_total, 0);
        assert_eq!(cpu.registers.ip, 0x18);
        assert_eq!(cpu.registers.get32(Gpr32::Ebp), 135);
        assert_eq!(cpu.registers.get32(Gpr32::Eax), 0xDEAD_BEEF);
        #[cfg(feature = "profiling")]
        {
            assert_eq!(cpu.decode_cache.stats.block_misses, 1);
            assert_eq!(cpu.decode_cache.stats.block_stale, 0);
            assert_eq!(cpu.decode_cache.stats.block_hits, 1);
        }
    }

    #[test]
    fn self_modified_x87_displacements_map_to_virtual_registers() {
        let mut bus = Flat {
            ram: [0x90; 0x200],
            generation: 0,
        };
        let code = [
            0x89, 0x0D, 0x08, 0x00, 0x00, 0x00, // mov [0x08], ecx (patches the FLD disp32)
            0xD9, 0x05, 0x78, 0x56, 0x34, 0x12, // fld dword [0x12345678]
            0xD9, 0x1D, 0x90, 0x00, 0x00, 0x00, // fstp dword [0x90]
        ];
        bus.ram[..code.len()].copy_from_slice(&code);
        bus.ram[0x80..0x84].copy_from_slice(&1.5f32.to_le_bytes());
        bus.ram[0x84..0x88].copy_from_slice(&2.5f32.to_le_bytes());
        let mut cpu = prepared_cpu().with_x87();
        for (source, value) in [(0x80, 1.5f32), (0x84, 2.5)] {
            cpu.registers.ip = 0;
            cpu.registers.set32(Gpr32::Ecx, source);
            let info = allocation_counter::measure(|| {
                assert_eq!(cpu.interpret_batch(&mut bus, 3, |_| 3), 3);
            });
            if source == 0x84 {
                assert_eq!(info.count_total, 0);
            }
            assert_eq!(cpu.registers.ip, 0x12);
            assert_eq!(bus.ram[0x90..0x94], value.to_le_bytes());
        }
        #[cfg(feature = "profiling")]
        {
            assert_eq!(cpu.decode_cache.stats.block_misses, 1);
            assert_eq!(cpu.decode_cache.stats.block_stale, 0);
            assert_eq!(cpu.decode_cache.stats.block_hits, 1);
        }
    }
}
