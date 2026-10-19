//! Runtime side of ABI v2. No WASM engine or frontend dependency.
use crate::{
    ANY_TIER, BINDINGS, DISPATCH_PROBES, DISPATCH_SLOTS, FLAGS, INTERPRETED, IP, MAX_QUANTUM,
    QUOTA, Region, STATE_BYTES, TIER, Translation, WINDOW, WINDOW_LEN,
};
use alloc::vec::Vec;
use core::ops::Range;
pub use doslet_bus::CodeCheck;
use doslet_bus::CpuBus;
use doslet_x86::{Cpu, CpuError, Gpr32, SegmentRegister, StepOutcome, flags};

const REGISTERS: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];
/// Dispatch validity is tracked per linear 4 KiB code page.
const PAGE_SHIFT: u32 = 12;
/// A grant chains only into entries on its own page and the adjacent pages.
const WINDOW_PAGES: u32 = 1;
/// Longest blocks of a region checked before accepting a live delta.
const SAMPLE: usize = 64;
const MAX_REBASE_WAIT: u32 = 4096;
/// An entry stops ordinary batches only while its grants pay for the stop:
/// after this many attempts, fewer translated steps per attempt make it cold.
const PRUNE_ATTEMPTS: u32 = 32;
const MIN_YIELD: u32 = 32;
const REVIEW_ATTEMPTS: u32 = 8192;
/// Cold entries are retried after this much guest work, as code phases change.
const REVIVE_STEPS: u64 = 1 << 28;
/// Keeps a slot's probe chain intact; its tier never matches a grant.
const TOMBSTONE: u32 = (0xfe << 24) | 1;

/// The runtime owns CPU, memory, interrupts, service interception and clocks.
/// A grant promises no external code/mapping changes, IRQ, debugger stop,
/// service interception or device deadline before its end. Return zero if unsure.
pub trait Runtime {
    type Error;
    fn cpu(&self) -> &Cpu;
    fn cpu_mut(&mut self) -> &mut Cpu;
    fn running(&self) -> bool {
        !self.cpu().is_halted()
    }
    /// Apply pending host input before deciding whether a translated grant is safe.
    fn prepare(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]>;
    /// Write-generation identity of a code span. Without it the host compares
    /// guarded bytes again before every grant.
    fn code_check(&self, _address: u32, _length: u8) -> Option<CodeCheck> {
        None
    }
    fn code_valid(&self, _check: &CodeCheck) -> bool {
        false
    }
    fn translated_budget(&self, remaining: u32) -> u32;
    /// CPU state has been committed before this callback. Advance guest clocks
    /// and counters by exactly `steps`, without executing CPU instructions.
    fn account_translated(&mut self, steps: u32) -> Result<(), Self::Error>;
    /// Opt into tiered execution. The runtime must classify live guarded bytes
    /// and account the matching tier in `account_translated_tier`. A grant only
    /// binds blocks whose every instruction has this same live tier.
    fn translated_tier(&self, _address: u32, _length: u8) -> Option<u8> {
        None
    }
    fn account_translated_tier(&mut self, steps: u32, _tier: u8) -> Result<(), Self::Error> {
        self.account_translated(steps)
    }
    /// Sorted, unique flat-32 EIPs of live translated blocks. Ordinary batches
    /// should yield on reaching one. Setup-time: called after each rebase.
    fn set_entries(&mut self, _sorted: Vec<u32>) -> Result<(), Self::Error> {
        Ok(())
    }
    /// Execute at most `budget` complete ordinary runtime steps (including IRQs
    /// and services), or report an error. It may stop after any step and
    /// should stop on arriving at a registered entry. Zero means no progress.
    /// It is never recursively called by WASM.
    fn interpret(&mut self, budget: u32) -> Result<u32, Self::Error>;
}

#[derive(Debug)]
pub enum Error<E> {
    Runtime(E),
    Protocol(&'static str),
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub polls: u64,
    pub translated_steps: u64,
    pub translated_tiers: [u64; 4],
    pub interpreted_steps: u64,
    pub interpreter_batches: u64,
    pub grants: u64,
    pub zero_budget: u64,
    pub rejected_context: u64,
    pub missing_code: u64,
    pub page_validations: u64,
    pub rebase_attempts: u64,
    pub cold_entries: u32,
}

struct Page {
    number: u32,
    blocks: Range<u32>,
    /// Generations of this page and, when a block crosses it, the next page.
    guards: [Option<CodeCheck>; 2],
    valid: bool,
}
/// Live block placement and the scratch dispatch table built from it.
struct Dispatch {
    /// Declared regions, or one implicit region spanning every hint.
    regions: Vec<Region>,
    deltas: Vec<Option<u32>>,
    /// Region per block; `u8::MAX` binds at the hint.
    region_of: Vec<u8>,
    /// Longest blocks first, per region.
    samples: Vec<Vec<u16>>,
    unresolved: usize,
    pages: Vec<Page>,
    /// Live address and block index, grouped by `Page::blocks`.
    placed: Vec<(u32, u16)>,
    entries_pending: bool,
    /// Per block: grant attempts at its entry and the translated steps they
    /// produced; cold entries no longer stop ordinary batches.
    attempts: Vec<u32>,
    yields: Vec<u32>,
    cold: Vec<bool>,
    review: u32,
    since_revive: u64,
    cleared: bool,
    labels: bool,
    rebase_wait: u32,
    rebase_backoff: u32,
}
impl Dispatch {
    fn new(translation: &Translation) -> Self {
        let blocks = translation.blocks();
        let mut regions = translation.regions().to_vec();
        if regions.is_empty() && !blocks.is_empty() {
            // Observed hints come from one recorded layout, which may differ
            // from this run's loader, DPMI host or memory configuration.
            let start = blocks.iter().map(|b| b.address_hint).min().unwrap_or(0);
            let end = blocks
                .iter()
                .map(|b| u64::from(b.address_hint) + b.bytes.len() as u64)
                .max()
                .unwrap_or(0);
            regions.push(Region {
                base: start,
                size: u32::try_from(end - u64::from(start)).unwrap_or(u32::MAX),
            });
        }
        let region_of: Vec<u8> = blocks
            .iter()
            .map(|b| {
                regions
                    .iter()
                    .position(|r| r.contains(b.address_hint))
                    .map_or(u8::MAX, |r| r as u8)
            })
            .collect();
        let samples = (0..regions.len())
            .map(|r| {
                let mut sample: Vec<u16> = (0..blocks.len() as u16)
                    .filter(|&i| usize::from(region_of[usize::from(i)]) == r)
                    .collect();
                sample.sort_by_key(|&i| core::cmp::Reverse(blocks[usize::from(i)].bytes.len()));
                sample.truncate(SAMPLE);
                sample
            })
            .collect();
        let mut dispatch = Self {
            deltas: alloc::vec![None; regions.len()],
            unresolved: regions.len(),
            regions,
            region_of,
            samples,
            pages: Vec::new(),
            placed: Vec::new(),
            entries_pending: false,
            attempts: alloc::vec![0; blocks.len()],
            yields: alloc::vec![0; blocks.len()],
            cold: alloc::vec![false; blocks.len()],
            review: 0,
            since_revive: 0,
            cleared: false,
            labels: false,
            rebase_wait: 0,
            rebase_backoff: 1,
        };
        dispatch.place(translation);
        dispatch
    }
    /// Recompute pages after a region delta is resolved. Bindings of already
    /// placed blocks are rewritten when their page is next validated.
    fn place(&mut self, translation: &Translation) {
        self.placed.clear();
        for (index, block) in translation.blocks().iter().enumerate() {
            let delta = match self.region_of[index] {
                u8::MAX => Some(0),
                r => self.deltas[usize::from(r)],
            };
            let Some(address) = delta.map(|d| block.address_hint.wrapping_add(d)) else {
                continue;
            };
            if address.checked_add(block.bytes.len() as u32 - 1).is_some() {
                self.placed.push((address, index as u16));
            }
        }
        self.placed.sort_unstable();
        self.placed.dedup_by_key(|(address, _)| *address);
        self.pages.clear();
        let mut start = 0;
        while start < self.placed.len() {
            let number = self.placed[start].0 >> PAGE_SHIFT;
            let end = start
                + self.placed[start..]
                    .iter()
                    .take_while(|(address, _)| address >> PAGE_SHIFT == number)
                    .count();
            self.pages.push(Page {
                number,
                blocks: start as u32..end as u32,
                guards: [None; 2],
                valid: false,
            });
            start = end;
        }
        self.entries_pending = true;
    }
    fn entry_at(&self, ip: u32) -> Option<u16> {
        self.placed
            .binary_search_by_key(&ip, |(address, _)| *address)
            .ok()
            .map(|index| self.placed[index].1)
    }
    fn entries(&self) -> Vec<u32> {
        self.placed
            .iter()
            .filter(|(_, block)| !self.cold[usize::from(*block)])
            .map(|(address, _)| *address)
            .collect()
    }
    /// Make entries cold whose grants do not repay the batch stop and poll.
    fn review(&mut self) -> u32 {
        self.review = 0;
        let mut changed = false;
        for (block, cold) in self.cold.iter_mut().enumerate() {
            let attempts = self.attempts[block];
            if !*cold && attempts >= PRUNE_ATTEMPTS && self.yields[block] < attempts * MIN_YIELD {
                *cold = true;
                changed = true;
            }
            self.attempts[block] /= 2;
            self.yields[block] /= 2;
        }
        self.entries_pending |= changed;
        self.cold.iter().filter(|&&cold| cold).count() as u32
    }
    fn revive(&mut self, steps: u32) {
        self.since_revive += u64::from(steps);
        if self.since_revive >= REVIVE_STEPS {
            self.since_revive = 0;
            if self.cold.contains(&true) {
                self.cold.fill(false);
                self.entries_pending = true;
            }
        }
    }
}

pub struct Host<R> {
    translation: Translation,
    runtime: R,
    outstanding: u32,
    outstanding_tier: Option<u8>,
    outstanding_entry: Option<u16>,
    stats: Stats,
    dispatch: Dispatch,
}
impl<R: Runtime> Host<R> {
    pub fn new(translation: Translation, runtime: R) -> Self {
        Self {
            dispatch: Dispatch::new(&translation),
            translation,
            runtime,
            outstanding: 0,
            outstanding_tier: None,
            outstanding_entry: None,
            stats: Stats::default(),
        }
    }
    pub fn translation(&self) -> &Translation {
        &self.translation
    }
    /// Setup-only replacement between completed generated runs.
    pub fn replace_translation(&mut self, translation: Translation) -> Result<(), &'static str> {
        if self.outstanding != 0 {
            return Err("cannot replace an active translated module");
        }
        self.dispatch = Dispatch::new(&translation);
        self.translation = translation;
        self.stats = Stats::default();
        Ok(())
    }
    pub fn runtime(&self) -> &R {
        &self.runtime
    }
    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }
    /// Restore a benchmark owner between completed runs, keeping prepared code.
    /// A run must flush its final grant before replacing runtime state.
    pub fn replace_runtime(&mut self, runtime: R) -> Result<(), &'static str> {
        if self.outstanding != 0 {
            return Err("cannot replace an active translated runtime");
        }
        self.runtime = runtime;
        self.dispatch = Dispatch::new(&self.translation);
        self.stats = Stats::default();
        Ok(())
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// Regions with their verified live delta, if any.
    pub fn regions(&self) -> impl Iterator<Item = (Region, Option<u32>)> + '_ {
        self.dispatch
            .regions
            .iter()
            .copied()
            .zip(self.dispatch.deltas.iter().copied())
    }
    /// ABI import `doslet.poll(completed, remaining) -> block index`.
    /// -1 asks for another poll, -2 stops this run. Every result first stores
    /// the interpreted steps at `INTERPRETED`. Call only from the matching
    /// compiler-produced module, synchronously and exclusively. `memory` starts
    /// at that module's imported scratch base and persists between polls.
    pub fn poll(
        &mut self,
        completed: u32,
        remaining: u32,
        memory: &mut [u8],
    ) -> Result<i32, Error<R::Error>> {
        if memory.len() < STATE_BYTES {
            return Err(Error::Protocol("short ABI memory"));
        }
        if completed > self.outstanding {
            return Err(Error::Protocol("translated grant exceeded"));
        }
        write(memory, INTERPRETED, 0);
        if completed != 0 {
            let cpu = self.runtime.cpu_mut();
            for (i, reg) in REGISTERS.iter().enumerate() {
                cpu.registers.set32(*reg, read(memory, i as u32 * 4));
            }
            cpu.flags = read(memory, FLAGS);
            cpu.registers.ip = read(memory, IP);
            if let Some(tier) = self.outstanding_tier {
                self.runtime.account_translated_tier(completed, tier)
            } else {
                self.runtime.account_translated(completed)
            }
            .map_err(Error::Runtime)?;
            self.stats.translated_steps += u64::from(completed);
            if let Some(tier) = self.outstanding_tier {
                self.stats.translated_tiers[usize::from(tier)] += u64::from(completed);
            }
            if let Some(block) = self.outstanding_entry {
                let yields = &mut self.dispatch.yields[usize::from(block)];
                *yields = yields.saturating_add(completed);
            }
            self.dispatch.revive(completed);
        }
        self.outstanding = 0;
        self.outstanding_tier = None;
        self.outstanding_entry = None;
        self.stats.polls += 1;
        if remaining == 0 || !self.runtime.running() {
            return Ok(-2);
        }
        if self.dispatch.entries_pending {
            self.dispatch.entries_pending = false;
            let entries = self.dispatch.entries();
            self.runtime.set_entries(entries).map_err(Error::Runtime)?;
        }
        self.runtime.prepare().map_err(Error::Runtime)?;
        if let Some(index) = self.grant(remaining, memory) {
            return Ok(index);
        }
        // Ordinary batches run up to the next registered entry, not one step.
        let steps = self.runtime.interpret(remaining).map_err(Error::Runtime)?;
        self.stats.interpreted_steps += u64::from(steps);
        self.stats.interpreter_batches += 1;
        self.dispatch.revive(steps);
        let steps = steps.min(remaining);
        write(memory, INTERPRETED, steps);
        if steps == 0 {
            return Ok(-2);
        }
        if steps < remaining
            && self.runtime.running()
            && let Some(index) = self.grant(remaining - steps, memory)
        {
            return Ok(index);
        }
        Ok(-1)
    }
    fn grant(&mut self, remaining: u32, memory: &mut [u8]) -> Option<i32> {
        let entry = self.dispatch.entry_at(self.runtime.cpu().registers.ip);
        if let Some(block) = entry {
            let attempts = &mut self.dispatch.attempts[usize::from(block)];
            *attempts = attempts.saturating_add(1);
            self.dispatch.review += 1;
            if self.dispatch.review >= REVIEW_ATTEMPTS {
                self.stats.cold_entries = self.dispatch.review();
            }
        }
        let quota = self
            .runtime
            .translated_budget(remaining)
            .min(remaining)
            .min(MAX_QUANTUM);
        let cpu = self.runtime.cpu();
        let cs = cpu.registers.segment_cache(SegmentRegister::Cs);
        let labels = cpu.execution_labels_enabled();
        let ip = cpu.registers.ip;
        let tier = labels
            .then(|| {
                let entry_length = self
                    .runtime
                    .code_bytes(ip, 15)
                    .and_then(doslet_x86::translation::decode)
                    .map_or(15, |i| i.length);
                self.runtime.translated_tier(ip, entry_length)
            })
            .flatten();
        let eligible = !cpu.is_halted()
            && cpu.control.cr0 & 0x8000_0001 == 1
            && cpu.registers.code32()
            && cs.base == 0
            && cs.limit == u32::MAX
            && !cpu.has_interrupt_shadow()
            && cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) == 0
            && (!labels || tier.is_some_and(|t| t < 4));
        if quota == 0 {
            self.stats.zero_budget += 1;
            return None;
        }
        if !eligible {
            self.stats.rejected_context += 1;
            return None;
        }
        if !self.dispatch.cleared {
            memory[BINDINGS as usize..STATE_BYTES].fill(0);
            self.dispatch.cleared = true;
        }
        if labels != self.dispatch.labels {
            self.dispatch.labels = labels;
            self.dispatch.pages.iter_mut().for_each(|p| p.valid = false);
        }
        if self.dispatch.unresolved != 0 {
            if self.dispatch.rebase_wait == 0 {
                self.rebase(ip);
            } else {
                self.dispatch.rebase_wait -= 1;
            }
        }
        let tier = tier.unwrap_or(ANY_TIER);
        let (window, window_len) = self.validate_window(ip, memory);
        let Some(index) = resolve(memory, ip, quota, tier) else {
            self.stats.missing_code += 1;
            return None;
        };
        let cpu = self.runtime.cpu();
        for (i, r) in REGISTERS.iter().enumerate() {
            write(memory, i as u32 * 4, cpu.registers.get32(*r));
        }
        write(memory, FLAGS, cpu.flags);
        write(memory, IP, ip);
        write(memory, QUOTA, quota);
        write(memory, TIER, u32::from(tier));
        write(memory, WINDOW, window);
        write(memory, WINDOW_LEN, window_len);
        self.outstanding = quota;
        self.outstanding_tier = (tier != ANY_TIER).then_some(tier);
        self.outstanding_entry = entry;
        self.stats.grants += 1;
        Some(index)
    }
    /// Revalidate pages around `ip` whose write generations changed, then
    /// return the linear window in which the module may chain entries.
    fn validate_window(&mut self, ip: u32, memory: &mut [u8]) -> (u32, u32) {
        let page = ip >> PAGE_SHIFT;
        let first = page.saturating_sub(WINDOW_PAGES);
        let last = page
            .saturating_add(WINDOW_PAGES)
            .min(u32::MAX >> PAGE_SHIFT);
        let start = self.dispatch.pages.partition_point(|p| p.number < first);
        let end = self.dispatch.pages.partition_point(|p| p.number <= last);
        for index in start..end {
            self.validate_page(index, memory);
        }
        (first << PAGE_SHIFT, (last - first + 1) << PAGE_SHIFT)
    }
    fn validate_page(&mut self, index: usize, memory: &mut [u8]) {
        let page = &self.dispatch.pages[index];
        if page.valid
            && page
                .guards
                .iter()
                .flatten()
                .all(|guard| self.runtime.code_valid(guard))
        {
            return;
        }
        self.stats.page_validations += 1;
        let labels = self.dispatch.labels;
        let mut crosses = false;
        for &(address, block) in
            &self.dispatch.placed[page.blocks.start as usize..page.blocks.end as usize]
        {
            let b = &self.translation.blocks()[usize::from(block)];
            let length = b.bytes.len() as u8;
            crosses |= (address + u32::from(length) - 1) >> PAGE_SHIFT != page.number;
            let tier = if !labels {
                Some(ANY_TIER)
            } else {
                let mut at = address;
                let mut uniform = None;
                b.instructions
                    .iter()
                    .try_for_each(|instruction| {
                        let tier = self.runtime.translated_tier(at, instruction.length)?;
                        at = at.wrapping_add(u32::from(instruction.length));
                        (*uniform.get_or_insert(tier) == tier && tier < 4).then_some(())
                    })
                    .and(uniform)
            };
            let packed = tier
                .filter(|_| self.runtime.code_bytes(address, length) == Some(b.bytes.as_slice()))
                .map(|tier| {
                    (u32::from(tier) << 24)
                        | ((b.instructions.len() as u32) << 16)
                        | (u32::from(block) + 1)
                });
            bind(memory, address, packed);
        }
        let base = page.number << PAGE_SHIFT;
        let guards = [
            self.runtime.code_check(base, 1),
            crosses
                .then(|| {
                    self.runtime
                        .code_check(base.wrapping_add(1 << PAGE_SHIFT), 1)
                })
                .flatten(),
        ];
        let page = &mut self.dispatch.pages[index];
        page.valid = guards[0].is_some() && (!crosses || guards[1].is_some());
        page.guards = guards;
    }
    /// Resolve unknown region deltas from code executing at `ip`. Bytes still
    /// guard every binding; a delta only selects where blocks are looked for.
    fn rebase(&mut self, ip: u32) {
        self.stats.rebase_attempts += 1;
        let mut resolved = false;
        for r in 0..self.dispatch.regions.len() {
            if self.dispatch.deltas[r].is_some() {
                continue;
            }
            if let Some(delta) = self.find_delta(r, ip) {
                self.dispatch.deltas[r] = Some(delta);
                self.dispatch.unresolved -= 1;
                resolved = true;
            }
        }
        if resolved {
            self.dispatch.place(&self.translation);
            self.dispatch.rebase_backoff = 1;
            self.dispatch.rebase_wait = 0;
        } else {
            self.dispatch.rebase_wait = self.dispatch.rebase_backoff;
            self.dispatch.rebase_backoff = (self.dispatch.rebase_backoff * 2).min(MAX_REBASE_WAIT);
        }
    }
    /// The live region must contain `ip`. Search its anchor (longest sampled
    /// block) at every placement doing so, and accept a delta when most samples
    /// match. Loaders allocate pages, so page-aligned deltas win over unaligned
    /// copies of the same image (for example a file buffer).
    fn find_delta(&self, r: usize, ip: u32) -> Option<u32> {
        let region = self.dispatch.regions[r];
        let sample = &self.dispatch.samples[r];
        let anchor = &self.translation.blocks()[usize::from(*sample.first()?)];
        let offset = anchor.address_hint.wrapping_sub(region.base);
        // Live base in (ip - size, ip], so the anchor lies in this span.
        let first =
            u64::from(ip) + u64::from(offset) + 1 - u64::from(region.size).min(u64::from(ip) + 1);
        let last = u64::from(ip) + u64::from(offset);
        let mut best: Option<u32> = None;
        let mut at = first;
        let needle = anchor.bytes.as_slice();
        while at <= last && at <= u64::from(u32::MAX) {
            let Some(chunk) = self.runtime.code_bytes(at as u32, u8::MAX) else {
                at = (at | ((1 << PAGE_SHIFT) - 1)) + 1;
                continue;
            };
            let positions = chunk.len().saturating_sub(needle.len()) + 1;
            for (i, window) in chunk.windows(needle.len()).enumerate() {
                let found = at + i as u64;
                if found > last {
                    break;
                }
                if window != needle {
                    continue;
                }
                let delta = (found as u32).wrapping_sub(anchor.address_hint);
                if best.is_some_and(|d| d & 0xfff == 0 || delta & 0xfff != 0) {
                    continue;
                }
                if self.verify(r, delta) {
                    best = Some(delta);
                    if delta & 0xfff == 0 {
                        return best;
                    }
                }
            }
            at += positions as u64;
        }
        best
    }
    fn verify(&self, r: usize, delta: u32) -> bool {
        let sample = &self.dispatch.samples[r];
        let hits = sample
            .iter()
            .filter(|&&index| {
                let block = &self.translation.blocks()[usize::from(index)];
                let address = block.address_hint.wrapping_add(delta);
                address.checked_add(block.bytes.len() as u32 - 1).is_some()
                    && self.runtime.code_bytes(address, block.bytes.len() as u8)
                        == Some(block.bytes.as_slice())
            })
            .count();
        hits * 2 >= sample.len()
    }
}
fn hash(ip: u32) -> u32 {
    ip.wrapping_mul(0x9e37_79b1) >> 19
}
/// Insert or replace one entry. `None` tombstones an existing entry only;
/// saturated probe chains leave the address on the interpreter.
fn bind(memory: &mut [u8], ip: u32, packed: Option<u32>) {
    for probe in 0..DISPATCH_PROBES {
        let slot = (hash(ip) + probe) & (DISPATCH_SLOTS as u32 - 1);
        let offset = BINDINGS + slot * 8;
        if read(memory, offset + 4) == 0 {
            if let Some(packed) = packed {
                write(memory, offset, ip);
                write(memory, offset + 4, packed);
            }
            return;
        }
        if read(memory, offset) == ip {
            write(memory, offset + 4, packed.unwrap_or(TOMBSTONE));
            return;
        }
    }
}
fn resolve(memory: &[u8], ip: u32, quota: u32, tier: u8) -> Option<i32> {
    for probe in 0..DISPATCH_PROBES {
        let slot = (hash(ip) + probe) & (DISPATCH_SLOTS as u32 - 1);
        let offset = BINDINGS + slot * 8;
        let packed = read(memory, offset + 4);
        if packed == 0 {
            return None;
        }
        if read(memory, offset) == ip {
            return ((packed >> 16) & 0xff <= quota && (packed >> 24) as u8 == tier)
                .then_some(((packed & 0xffff) - 1) as i32);
        }
    }
    None
}
fn read(memory: &[u8], offset: u32) -> u32 {
    u32::from_le_bytes(
        memory[offset as usize..offset as usize + 4]
            .try_into()
            .unwrap(),
    )
}
fn write(memory: &mut [u8], offset: u32, value: u32) {
    memory[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
}

/// CPU/bus test adapter. No PC timers or runtime service interception; use a
/// machine/session Runtime implementation before running an actual game.
pub struct CpuRuntime<B> {
    pub cpu: Cpu,
    pub bus: B,
    pub translated_limit: u32,
}
impl<B: CpuBus> Runtime for CpuRuntime<B> {
    type Error = CpuError;
    fn cpu(&self) -> &Cpu {
        &self.cpu
    }
    fn cpu_mut(&mut self) -> &mut Cpu {
        &mut self.cpu
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        self.bus.code_bytes(address, length)
    }
    fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
        self.bus.code_check(address, length)
    }
    fn code_valid(&self, check: &CodeCheck) -> bool {
        self.bus.code_valid(check)
    }
    fn translated_budget(&self, remaining: u32) -> u32 {
        remaining.min(self.translated_limit)
    }
    fn account_translated(&mut self, _steps: u32) -> Result<(), CpuError> {
        Ok(())
    }
    fn interpret(&mut self, _budget: u32) -> Result<u32, CpuError> {
        // HLT itself is one step; the next poll stops on the halted CPU.
        self.cpu.step(&mut self.bus).map(|_: StepOutcome| 1)
    }
}
