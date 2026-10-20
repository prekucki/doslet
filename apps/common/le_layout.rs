//! Where a DOS extender placed each object of a 32-bit LE program. The
//! loader chooses bases at run time, so they differ with and without a DPMI
//! host or with another memory configuration. Learned speed tiers, patches
//! and procedure modules are kept relative to these objects and bound to the
//! bases found in each run.
use doslet_aot::{
    DefaultBitness, ImageLimits, SymbolicImage, SymbolicObject,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
};
use doslet_executable::{LeFixupSourceType, LeImage, LeLimits};
use doslet_runtime::Session;
use doslet_x86::SegmentRegister;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Loaders place objects on page boundaries; the page-aligned search reads
/// one window per page, the exhaustive one every offset in one pass.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Search {
    PageAligned,
    Exhaustive,
}

/// Candidate bases per anchor, at most 65 each.
fn scan(ram: &[u8], anchors: &[Anchor], search: Search) -> Vec<Vec<u32>> {
    let mut found = vec![Vec::new(); anchors.len()];
    let mut record = |index: usize, offset: usize| {
        let matches: &mut Vec<u32> = &mut found[index];
        if matches.len() <= 64
            && ram[offset..offset + 32] == anchors[index].bytes
            && let Some(base) = (offset as u32 + 0x100000).checked_sub(anchors[index].offset)
        {
            matches.push(base);
        }
    };
    match search {
        Search::PageAligned => {
            for (index, anchor) in anchors.iter().enumerate() {
                let mut offset = anchor.offset as usize % 4096;
                while offset + 32 <= ram.len() {
                    record(index, offset);
                    offset += 4096;
                }
            }
        }
        Search::Exhaustive => {
            let mut prefixes = HashMap::<[u8; 4], Vec<usize>>::new();
            for (index, anchor) in anchors.iter().enumerate() {
                let prefix = anchor.bytes[..4].try_into().expect("anchor prefix");
                prefixes.entry(prefix).or_default().push(index);
            }
            for offset in 0..ram.len().saturating_sub(31) {
                let prefix: [u8; 4] = ram[offset..offset + 4].try_into().expect("window");
                if let Some(indices) = prefixes.get(&prefix) {
                    for &index in indices {
                        record(index, offset);
                    }
                }
            }
        }
    }
    found
}

pub struct Anchor {
    object: u32,
    offset: u32,
    bytes: [u8; 32],
}

/// Distinct, relocation-free 32-byte runs spread over the file-backed code.
pub fn anchors(image: &SymbolicImage) -> Vec<Anchor> {
    let mut result = Vec::new();
    for object in image
        .objects()
        .iter()
        .filter(|o| o.metadata().is_executable())
    {
        let mut fixed = vec![false; object.bytes().len()];
        for span in object.file_backed_spans() {
            let start = span.object_offset as usize;
            fixed[start..start + span.byte_len as usize].fill(true);
        }
        for r in image
            .relocations()
            .filter(|r| r.source.object_number == object.object_number())
        {
            let start = r.source.offset as usize;
            fixed[start..start + r.kind.width() as usize].fill(false);
        }
        let stride = (object.bytes().len() / 8).max(32);
        for start in (0..object.bytes().len()).step_by(stride).take(8) {
            let end = (start + stride).min(object.bytes().len());
            for offset in start..end.saturating_sub(31) {
                if !fixed[offset..offset + 32].iter().all(|&b| b) {
                    continue;
                }
                let bytes = &object.bytes()[offset..offset + 32];
                let mut seen = [false; 256];
                for &byte in bytes {
                    seen[byte as usize] = true;
                }
                if seen.iter().filter(|&&v| v).count() < 10 {
                    continue;
                }
                result.push(Anchor {
                    object: object.object_number(),
                    offset: offset as u32,
                    bytes: bytes.try_into().unwrap(),
                });
                break;
            }
        }
        if result.len() >= 128 {
            break;
        }
    }
    result
}

fn winner<T: Copy + Ord>(votes: &BTreeMap<T, usize>) -> Option<T> {
    let mut ranked: Vec<_> = votes
        .iter()
        .map(|(&value, &count)| (count, value))
        .collect();
    ranked.sort_unstable_by(|a, b| b.cmp(a));
    let &(count, value) = ranked.first()?;
    (count >= 2 && ranked.get(1).is_none_or(|&(next, _)| count > next * 2)).then_some(value)
}

fn descriptor(session: &Session, selector: u16) -> Option<DosFlatSelector> {
    let control = session.control_registers();
    let table = if selector & 4 != 0 {
        control.ldtr?
    } else {
        control.gdtr
    };
    let offset = u32::from(selector & !7);
    if offset + 7 > u32::from(table.limit) {
        return None;
    }
    let mut raw = [0; 8];
    if !session.debug_read_memory(table.base.checked_add(offset)?, &mut raw)
        || raw[5] & 0x90 != 0x90
    {
        return None;
    }
    let base = u32::from(raw[2])
        | (u32::from(raw[3]) << 8)
        | (u32::from(raw[4]) << 16)
        | (u32::from(raw[7]) << 24);
    let mut limit = u32::from(raw[0]) | (u32::from(raw[1]) << 8) | (u32::from(raw[6] & 15) << 16);
    if raw[6] & 0x80 != 0 {
        limit = (limit << 12) | 0xfff;
    }
    Some(DosFlatSelector {
        value: selector,
        base,
        limit,
        default_bitness: if raw[6] & 0x40 != 0 {
            DefaultBitness::Bits32
        } else {
            DefaultBitness::Bits16
        },
    })
}

/// Bindings of every LE object in the running session. Preferred image
/// addresses never select a base; anchored live code and its relocated
/// operands do, and every executable fixup must match the loaded bytes.
#[allow(dead_code, reason = "the headless capture example probes directly")]
pub fn discover(
    session: &Session,
    image: &SymbolicImage,
    anchors: &[Anchor],
) -> Result<Vec<DosFlatObjectBinding>> {
    discover_with(session, image, anchors, Search::PageAligned)
        .or_else(|_| discover_with(session, image, anchors, Search::Exhaustive))
}

fn discover_with(
    session: &Session,
    image: &SymbolicImage,
    anchors: &[Anchor],
    search: Search,
) -> Result<Vec<DosFlatObjectBinding>> {
    let ram_end = usize::try_from(session.memory_usage().installed_bytes)?;
    let ram = session
        .ram_view(0x100000..ram_end)
        .ok_or("extended RAM unavailable")?;
    let mut votes = BTreeMap::<u32, BTreeMap<u32, usize>>::new();
    for (anchor, matches) in anchors.iter().zip(scan(ram, anchors, search)) {
        // Repeated fill patterns are poor anchors and must not create an
        // unbounded candidate table from guest-controlled bytes.
        if matches.len() <= 64 {
            for base in matches {
                *votes
                    .entry(anchor.object)
                    .or_default()
                    .entry(base)
                    .or_default() += 1;
            }
        }
    }
    let mut bases = BTreeMap::new();
    let mut active_code = false;
    for (&number, votes) in &votes {
        let size = image.object(number).ok_or("missing object")?.bytes().len() as u64;
        let pc = u64::from(session.debug_pc());
        // A loader may retain an unrelocated file copy elsewhere in RAM.
        // Only the candidate containing the active PC can anchor live code.
        let active: BTreeMap<_, _> = votes
            .iter()
            .filter(|&(&base, _)| pc >= u64::from(base) && pc < u64::from(base) + size)
            .map(|(&base, &count)| (base, count))
            .collect();
        if let Some(base) = winner(&active) {
            bases.insert(number, base);
            active_code = true;
        } else if let Some(base) = winner(votes) {
            bases.insert(number, base);
        }
    }
    if !active_code {
        return Err("not executing uniquely anchored LE code yet".into());
    }
    // Infer other objects from the relocated addresses in that anchored code.
    for _ in 0..image.objects().len() {
        let mut target_votes = BTreeMap::<u32, BTreeMap<u32, usize>>::new();
        for r in image.relocations() {
            let (Some(&base), Some(addend)) = (bases.get(&r.source.object_number), r.target.offset)
            else {
                continue;
            };
            if !matches!(
                r.kind,
                LeFixupSourceType::Offset32 | LeFixupSourceType::SelfRelative32
            ) {
                continue;
            }
            let mut raw = [0; 4];
            if !session.debug_read_memory(base.wrapping_add(r.source.offset), &mut raw) {
                continue;
            }
            let value = u32::from_le_bytes(raw);
            let target = match r.kind {
                LeFixupSourceType::Offset32 => value.wrapping_sub(addend),
                _ => value
                    .wrapping_add(base)
                    .wrapping_add(r.source.offset)
                    .wrapping_add(4)
                    .wrapping_sub(addend),
            };
            *target_votes
                .entry(r.target.object_number)
                .or_default()
                .entry(target)
                .or_default() += 1;
        }
        for (&number, votes) in &target_votes {
            if !bases.contains_key(&number)
                && let Some(base) = winner(votes)
            {
                bases.insert(number, base);
            }
        }
    }
    let mut result = Vec::new();
    for object in image.objects() {
        let number = object.object_number();
        let base = *bases
            .get(&number)
            .ok_or("object lacks relocated base evidence")?;
        let select = |alias: bool| -> Result<Option<DosFlatSelector>> {
            let mut values = BTreeMap::<u16, usize>::new();
            for r in image.relocations().filter(|r| {
                r.target.object_number == number
                    && r.kind == LeFixupSourceType::Selector16
                    && r.is_alias == alias
            }) {
                let source = bases
                    .get(&r.source.object_number)
                    .ok_or("selector source missing")?;
                let mut raw = [0; 2];
                if session.debug_read_memory(source.wrapping_add(r.source.offset), &mut raw) {
                    *values.entry(u16::from_le_bytes(raw)).or_default() += 1;
                }
            }
            let observed = match values.len() {
                0 => None,
                1 => values.keys().next().copied(),
                _ => return Err("conflicting live selector fixups".into()),
            };
            let bits = if alias {
                DefaultBitness::Bits16
            } else {
                object.default_bitness()
            };
            if let Some(value) = observed {
                return descriptor(session, value)
                    .map(Some)
                    .ok_or_else(|| "live selector descriptor absent".into());
            }
            if bits == DefaultBitness::Bits32 {
                let segment = if object.metadata().is_executable() {
                    SegmentRegister::Cs
                } else {
                    SegmentRegister::Ds
                };
                return descriptor(session, session.registers().segment_cache(segment).selector)
                    .map(Some)
                    .ok_or_else(|| "flat descriptor absent".into());
            }
            let control = session.control_registers();
            let mut candidates = Vec::new();
            let mut matching_base = Vec::new();
            for (table, ti) in [(Some(control.gdtr), 0), (control.ldtr, 4)] {
                let Some(table) = table else {
                    continue;
                };
                for index in 1..(u32::from(table.limit) + 1) / 8 {
                    let value = (index as u16 * 8) | ti;
                    if let Some(d) = descriptor(session, value)
                        && d.base == base
                    {
                        matching_base.push(d);
                        if d.limit == 0xffff && d.default_bitness == bits {
                            candidates.push(d);
                        }
                    }
                }
            }
            if candidates.is_empty() && !alias {
                // Some loaded objects have addresses but no live selector.
                // The binder rejects this if a Selector16 fixup requires one;
                // the bound CFG never admits an address-only execution view.
                return Ok(None);
            }
            if candidates.len() != 1 {
                return Err(format!(
                    "object{number} base{base:#x} alias={alias}: narrow descriptor is absent or ambiguous; descriptors={matching_base:?}; bases={bases:?}; anchors={votes:?}"
                ).into());
            }
            Ok(Some(candidates[0]))
        };
        let normal = select(false)?;
        let needs_alias = image
            .relocations()
            .any(|r| r.target.object_number == number && r.is_alias);
        result.push(DosFlatObjectBinding {
            object_number: number,
            linear_base: base,
            normal,
            alias: if needs_alias {
                Some(select(true)?.ok_or("alias descriptor missing")?)
            } else {
                None
            },
        });
    }
    let plan = DosFlatBindingPlan::new(image, &result, BindingLimits::default())?;
    // Validate every relocation field in executable objects. Mutable data fields
    // may already have been updated by startup code and are not an exact oracle.
    for patch in plan.patches() {
        if !image
            .object(patch.source.object_number)
            .ok_or("patch object")?
            .metadata()
            .is_executable()
        {
            continue;
        }
        let base = bases[&patch.source.object_number];
        let mut raw = [0; 6];
        let bytes = &mut raw[..patch.bytes().len()];
        if !session.debug_read_memory(
            base.checked_add(patch.source.offset)
                .ok_or("patch address overflow")?,
            bytes,
        ) || bytes != patch.bytes()
        {
            return Err(format!(
                "loaded code fixup differs at object{}+{:#x}",
                patch.source.object_number, patch.source.offset
            )
            .into());
        }
    }
    Ok(result)
}

/// One loaded object: its number, live base and extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectSpan {
    pub number: u32,
    pub base: u32,
    pub size: u32,
}

/// The loaded extent: the virtual size, or the file data when it is larger.
fn object_size(object: &SymbolicObject) -> u32 {
    object
        .metadata()
        .virtual_size
        .max(object.bytes().len() as u32)
}

/// The object containing `address`, and the offset of `address` in it.
pub fn locate(spans: &[ObjectSpan], address: u32) -> Option<(u32, u32)> {
    spans.iter().find_map(|span| {
        let offset = address.checked_sub(span.base)?;
        (offset < span.size).then_some((span.number, offset))
    })
}

/// Spans of `image` at the given `(object, base)` pairs; unknown objects are
/// an error.
pub fn spans(image: &SymbolicImage, bases: &[(u32, u32)]) -> Result<Vec<ObjectSpan>> {
    bases
        .iter()
        .map(|&(number, base)| {
            let object = image
                .object(number)
                .ok_or_else(|| format!("the program has no LE object {number}"))?;
            Ok(ObjectSpan {
                number,
                base,
                size: object_size(object),
            })
        })
        .collect()
}

/// Where the loader placed each LE object in one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub bindings: Vec<DosFlatObjectBinding>,
    pub objects: Vec<ObjectSpan>,
}
impl Layout {
    pub fn new(image: &SymbolicImage, bindings: Vec<DosFlatObjectBinding>) -> Result<Self> {
        let bases: Vec<_> = bindings
            .iter()
            .map(|b| (b.object_number, b.linear_base))
            .collect();
        Ok(Self {
            objects: spans(image, &bases)?,
            bindings,
        })
    }
    pub fn bases(&self) -> Vec<(u32, u32)> {
        self.objects.iter().map(|o| (o.number, o.base)).collect()
    }
    /// A cache directory name: the bases, then a digest of every binding
    /// field, since selector values are relocated into code as well.
    pub fn key(&self) -> String {
        let mut digest = Sha256::new();
        for binding in &self.bindings {
            digest.update(binding.object_number.to_le_bytes());
            digest.update(binding.linear_base.to_le_bytes());
            for selector in [binding.normal, binding.alias] {
                match selector {
                    Some(s) => {
                        digest.update([1]);
                        digest.update(s.value.to_le_bytes());
                        digest.update(s.base.to_le_bytes());
                        digest.update(s.limit.to_le_bytes());
                        digest.update([u8::from(s.default_bitness == DefaultBitness::Bits32)]);
                    }
                    None => digest.update([0]),
                }
            }
        }
        let bases: Vec<_> = self
            .objects
            .iter()
            .map(|o| format!("{:x}", o.base))
            .collect();
        format!("le-{}-{:.8x}", bases.join("-"), digest.finalize())
    }
}

/// Probes the running session for its live layout, once per session epoch.
pub struct Watcher {
    exe: Arc<[u8]>,
    anchors: Vec<Anchor>,
    sizes: BTreeMap<u32, u32>,
    epoch: Option<u64>,
    next_probe: u64,
    probes: u32,
    found: bool,
}
/// Steps between probes while flat 32-bit code runs; about 10 ms at 100 MHz.
const PROBE_STEPS: u64 = 1_000_000;
/// One probe in this many also searches unaligned bases, in about 50 ms.
const EXHAUSTIVE_EVERY: u32 = 32;

impl Watcher {
    /// `None` unless `exe` is a 32-bit LE program with code to anchor.
    pub fn new(exe: Arc<[u8]>) -> Option<Self> {
        let le = LeImage::discover(&exe, LeLimits::default()).ok()?;
        let image = SymbolicImage::from_le(&le, ImageLimits::default()).ok()?;
        let anchors = anchors(&image);
        let sizes = image
            .objects()
            .iter()
            .map(|o| (o.object_number(), object_size(o)))
            .collect();
        drop(image);
        (!anchors.is_empty()).then(|| Self {
            exe: exe.clone(),
            anchors,
            sizes,
            epoch: None,
            next_probe: 0,
            probes: 0,
            found: false,
        })
    }
    pub fn exe(&self) -> &Arc<[u8]> {
        &self.exe
    }
    /// The layout, as soon as anchored LE code runs in this session.
    pub fn poll(&mut self, session: &Session) -> Option<Layout> {
        if self.epoch != Some(session.epoch()) {
            self.epoch = Some(session.epoch());
            self.next_probe = 0;
            self.probes = 0;
            self.found = false;
        }
        if self.found || session.steps() < self.next_probe || !flat_code(session) {
            return None;
        }
        self.next_probe = session.steps() + PROBE_STEPS;
        self.probes += 1;
        let search = if self.probes.is_multiple_of(EXHAUSTIVE_EVERY) {
            Search::Exhaustive
        } else {
            Search::PageAligned
        };
        // Only anchored code at the active PC justifies building the image.
        if !self.anchored_pc(session, search) {
            return None;
        }
        let le = LeImage::discover(&self.exe, LeLimits::default()).ok()?;
        let image = SymbolicImage::from_le(&le, ImageLimits::default()).ok()?;
        let bindings = discover_with(session, &image, &self.anchors, search).ok()?;
        let layout = Layout::new(&image, bindings).ok()?;
        self.found = true;
        Some(layout)
    }
    fn anchored_pc(&self, session: &Session, search: Search) -> bool {
        let Ok(ram_end) = usize::try_from(session.memory_usage().installed_bytes) else {
            return false;
        };
        let Some(ram) = session.ram_view(0x100000..ram_end) else {
            return false;
        };
        let pc = u64::from(session.debug_pc());
        self.anchors
            .iter()
            .zip(scan(ram, &self.anchors, search))
            .any(|(anchor, bases)| {
                let size = u64::from(self.sizes.get(&anchor.object).copied().unwrap_or(0));
                bases
                    .iter()
                    .any(|&base| pc >= u64::from(base) && pc < u64::from(base) + size)
            })
    }
}

/// A DOS extender's flat 32-bit code segment, without paging.
fn flat_code(session: &Session) -> bool {
    let cs = session.registers().segment_cache(SegmentRegister::Cs);
    cs.default32
        && cs.base == 0
        && cs.limit == u32::MAX
        && session.control_registers().cr0 & 0x8000_0001 == 1
}
