//! Where a DOS extender placed each object of a 32-bit LE program, or where
//! a DJGPP program's COFF sections run. The loader chooses bases at run time,
//! so they differ with and without a DPMI host or with another memory
//! configuration. Learned speed tiers, patches and procedure modules are kept
//! relative to these objects and bound to the bases found in each run.
//!
//! DJGPP's stub loads every section at its virtual address in one DPMI block
//! and gives the code and data selectors that block's base, so a COFF layout
//! is that base: the live CS base while program text runs. DJGPP's `sbrk` can
//! move the block (DPMI 0503h), and programs such as Quake patch their own
//! code at startup, so the watcher keeps following a COFF program: a new base
//! or a changed, settled text is a new layout.
use doslet_aot::{
    DefaultBitness, ImageFormat, ImageLimits, SymbolicImage, SymbolicObject,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
    coff_offset_bindings,
};
use doslet_executable::{CoffImage, CoffLimits, LeFixupSourceType, LeImage, LeLimits};
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
                .ok_or_else(|| format!("the program has no object {number}"))?;
            Ok(ObjectSpan {
                number,
                base,
                size: object_size(object),
            })
        })
        .collect()
}

/// The symbolic image of a 32-bit LE or DJGPP COFF program. `text` replaces
/// a COFF program's text bytes with the code as loaded.
pub fn program_image(exe: &[u8], text: Option<&[u8]>) -> Result<SymbolicImage> {
    if let Ok(le) = LeImage::discover(exe, LeLimits::default()) {
        return Ok(SymbolicImage::from_le(&le, ImageLimits::default())?);
    }
    let coff = CoffImage::discover(exe, CoffLimits::default())?;
    Ok(SymbolicImage::from_coff_with_text(
        &coff,
        text,
        ImageLimits::default(),
    )?)
}

/// Where the program's code runs: the base of its code selector, which the
/// bound EIPs are relative to, and the CS limit the bound code needs. LE
/// objects are bound at linear addresses in flat code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeSpace {
    pub base: u32,
    pub limit: u32,
}

impl CodeSpace {
    pub const FLAT: Self = Self {
        base: 0,
        limit: u32::MAX,
    };
}

/// Where the loader placed each object in one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Analysis bindings: live linear bases of LE objects, or the offsets of
    /// COFF sections in their selectors.
    pub bindings: Vec<DosFlatObjectBinding>,
    /// Live linear extents of every object.
    pub objects: Vec<ObjectSpan>,
    pub code: CodeSpace,
    /// A COFF program's text as loaded when the layout was found.
    pub text: Option<Arc<[u8]>>,
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
            code: CodeSpace::FLAT,
            text: None,
        })
    }
    /// A DJGPP program whose selectors share `base`, with its text as loaded
    /// and `selector` its code selector. Its bound code needs a CS limit
    /// covering the text.
    pub fn coff(image: &SymbolicImage, base: u32, selector: u16, text: Arc<[u8]>) -> Result<Self> {
        if image.format() != ImageFormat::Coff {
            return Err("not a COFF image".into());
        }
        let bases = image
            .objects()
            .iter()
            .map(|o| {
                base.checked_add(o.metadata().preferred_base)
                    .map(|linear| (o.object_number(), linear))
                    .ok_or_else(|| "a section lies beyond 4 GiB".into())
            })
            .collect::<Result<Vec<_>>>()?;
        let code = image
            .objects()
            .iter()
            .find(|o| o.metadata().is_executable())
            .ok_or("no text section")?;
        let end = code.metadata().preferred_base + code.metadata().virtual_size;
        Ok(Self {
            objects: spans(image, &bases)?,
            bindings: coff_offset_bindings(image, selector),
            code: CodeSpace {
                base,
                limit: end.checked_sub(1).ok_or("empty text section")?,
            },
            text: Some(text),
        })
    }
    pub fn bases(&self) -> Vec<(u32, u32)> {
        self.objects.iter().map(|o| (o.number, o.base)).collect()
    }
    /// A cache directory name: the bases, then a digest of every binding
    /// field, since selector values are relocated into code as well. A COFF
    /// key names the selector base and digests the loaded text instead.
    pub fn key(&self) -> String {
        let mut digest = Sha256::new();
        if let Some(text) = &self.text {
            digest.update(b"coff");
            digest.update(self.code.base.to_le_bytes());
            digest.update(self.code.limit.to_le_bytes());
            digest.update(Sha256::digest(text));
            return format!("coff-{:x}-{:.8x}", self.code.base, digest.finalize());
        }
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

/// A DJGPP program's text: its offset in the selectors, its size and
/// relocation-free runs to recognize it by.
struct Coff {
    text: u32,
    size: u32,
    anchors: Vec<(usize, [u8; 32])>,
    /// The base and text of the last layout reported in this session.
    reported: Option<(u32, Arc<[u8]>)>,
    /// Changed text seen at the previous check, reported if it persists.
    pending: Option<Box<[u8]>>,
    next_check: u64,
}

impl Coff {
    fn new(image: &SymbolicImage) -> Option<Self> {
        let text = image
            .objects()
            .iter()
            .find(|o| o.metadata().is_executable())?;
        // Self-patching programs change a few operands; most runs survive.
        let anchors: Vec<_> = anchors(image)
            .into_iter()
            .map(|anchor| (anchor.offset as usize, anchor.bytes))
            .collect();
        (!anchors.is_empty()).then(|| Self {
            text: text.metadata().preferred_base,
            size: text.metadata().virtual_size,
            anchors,
            reported: None,
            pending: None,
            next_check: 0,
        })
    }
}

enum Program {
    Le {
        anchors: Vec<Anchor>,
        sizes: BTreeMap<u32, u32>,
    },
    Coff(Coff),
}

/// Probes the running session for its live layout. An LE program's layout is
/// found once per session epoch; a COFF program's is followed after that.
pub struct Watcher {
    exe: Arc<[u8]>,
    program: Program,
    epoch: Option<u64>,
    next_probe: u64,
    probes: u32,
    found: bool,
}
/// Steps between probes while flat 32-bit code runs; about 10 ms at 100 MHz.
const PROBE_STEPS: u64 = 1_000_000;
/// One probe in this many also searches unaligned bases, in about 50 ms.
const EXHAUSTIVE_EVERY: u32 = 32;
/// Guest time between checks of a found COFF layout for a moved block or
/// patched code. A check compares the text, about 360 KiB for Quake.
const COFF_CHECK_NANOS: u64 = 500_000_000;

impl Watcher {
    /// `None` unless `exe` is a 32-bit LE or DJGPP COFF program with code to
    /// anchor.
    pub fn new(exe: Arc<[u8]>) -> Option<Self> {
        let program = if let Ok(le) = LeImage::discover(&exe, LeLimits::default()) {
            let image = SymbolicImage::from_le(&le, ImageLimits::default()).ok()?;
            let anchors = anchors(&image);
            let sizes = image
                .objects()
                .iter()
                .map(|o| (o.object_number(), object_size(o)))
                .collect();
            (!anchors.is_empty()).then_some(Program::Le { anchors, sizes })?
        } else {
            let image = program_image(&exe, None).ok()?;
            Program::Coff(Coff::new(&image)?)
        };
        Some(Self {
            exe,
            program,
            epoch: None,
            next_probe: 0,
            probes: 0,
            found: false,
        })
    }
    pub fn exe(&self) -> &Arc<[u8]> {
        &self.exe
    }
    /// The layout, as soon as anchored code runs in this session, and for a
    /// COFF program again when its block moves or its text settles changed.
    pub fn poll(&mut self, session: &Session) -> Option<Layout> {
        if self.epoch != Some(session.epoch()) {
            self.epoch = Some(session.epoch());
            self.next_probe = 0;
            self.probes = 0;
            self.found = false;
            if let Program::Coff(coff) = &mut self.program {
                coff.reported = None;
                coff.pending = None;
                coff.next_check = 0;
            }
        }
        if let Program::Coff(_) = self.program {
            return self.poll_coff(session);
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
        let Program::Le { anchors, .. } = &self.program else {
            return None;
        };
        let bindings = discover_with(session, &image, anchors, search).ok()?;
        let layout = Layout::new(&image, bindings).ok()?;
        self.found = true;
        Some(layout)
    }
    fn anchored_pc(&self, session: &Session, search: Search) -> bool {
        let Program::Le { anchors, sizes } = &self.program else {
            return false;
        };
        let Ok(ram_end) = usize::try_from(session.memory_usage().installed_bytes) else {
            return false;
        };
        let Some(ram) = session.ram_view(0x100000..ram_end) else {
            return false;
        };
        let pc = u64::from(session.debug_pc());
        anchors
            .iter()
            .zip(scan(ram, anchors, search))
            .any(|(anchor, bases)| {
                let size = u64::from(sizes.get(&anchor.object).copied().unwrap_or(0));
                bases
                    .iter()
                    .any(|&base| pc >= u64::from(base) && pc < u64::from(base) + size)
            })
    }
    /// The live CS base while program text runs, once three quarters of the
    /// text's anchors match there. The checks after the first layout compare
    /// the text with the reported one.
    fn poll_coff(&mut self, session: &Session) -> Option<Layout> {
        let Program::Coff(coff) = &mut self.program else {
            return None;
        };
        if !self.found {
            if session.steps() < self.next_probe {
                return None;
            }
            self.next_probe = session.steps() + PROBE_STEPS;
        } else {
            if session.emulated_nanos() < coff.next_check {
                return None;
            }
            coff.next_check = session.emulated_nanos() + COFF_CHECK_NANOS;
        }
        let registers = session.registers();
        let cs = registers.segment_cache(SegmentRegister::Cs);
        let code = CodeState {
            base: cs.base,
            limit: cs.limit,
            default32: cs.default32 && session.control_registers().cr0 & 0x8000_0001 == 1,
            ip: registers.ip,
        };
        let start = coff.candidate(code)?;
        let live = session.ram_view(start..start + coff.size as usize)?;
        let text = coff.accept(cs.base, live)?;
        let image = program_image(&self.exe, Some(&text)).ok()?;
        let layout = Layout::coff(&image, cs.base, cs.selector, text).ok()?;
        coff.next_check = session.emulated_nanos() + COFF_CHECK_NANOS;
        self.found = true;
        Some(layout)
    }
}

/// The code segment a COFF probe sees: its base and limit, whether it runs
/// 32-bit protected code without paging, and the EIP.
#[derive(Clone, Copy)]
struct CodeState {
    base: u32,
    limit: u32,
    default32: bool,
    ip: u32,
}

impl Coff {
    /// The linear start of the text when program text runs in `code` and
    /// the segment's limit covers it.
    fn candidate(&self, code: CodeState) -> Option<usize> {
        let offset = code.ip.wrapping_sub(self.text);
        if !code.default32 || offset >= self.size || code.limit < self.text + (self.size - 1) {
            return None;
        }
        usize::try_from(code.base.checked_add(self.text)?).ok()
    }

    /// The text to report for `live`, the text at `base`: the first one, one
    /// at a new base, or changed text seen unchanged at two checks in a row.
    /// Three quarters of the anchors must match.
    fn accept(&mut self, base: u32, live: &[u8]) -> Option<Arc<[u8]>> {
        if live.len() != self.size as usize {
            return None;
        }
        let matching = self
            .anchors
            .iter()
            .filter(|(at, bytes)| live[*at..*at + 32] == *bytes)
            .count();
        if matching * 4 < self.anchors.len() * 3 {
            return None;
        }
        match &self.reported {
            Some((reported, text)) if *reported == base && **text == *live => {
                self.pending = None;
                return None;
            }
            // Patched code is reported once it has settled.
            Some((reported, _)) if *reported == base && self.pending.as_deref() != Some(live) => {
                self.pending = Some(live.into());
                return None;
            }
            _ => {}
        }
        let text: Arc<[u8]> = live.into();
        self.reported = Some((base, text.clone()));
        self.pending = None;
        Some(text)
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

#[cfg(test)]
mod tests {
    use super::*;

    // An inline module's #[path] names a directory that does not exist here.
    mod fixture {
        include!("../../tests/fixtures/djgpp_coff.rs");
    }
    use fixture::{TEXT_VADDR, data_vaddr, djgpp_exe};

    const BASE: u32 = 0x12_0000;

    /// Varied synthetic code bytes, so every stride has an anchor.
    fn text() -> Vec<u8> {
        let mut state = 0x1234_5678u32;
        (0..4096)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    fn program() -> (Vec<u8>, Vec<u8>) {
        let text = text();
        (djgpp_exe(&text, &[1, 2, 3, 4], 0x2000, 0, &[]), text)
    }

    fn running(ip: u32) -> CodeState {
        CodeState {
            base: BASE,
            limit: 0xedffff,
            default32: true,
            ip,
        }
    }

    #[test]
    fn coff_programs_are_watched_and_other_files_are_not() {
        let (exe, _) = program();
        let watcher = Watcher::new(exe.into()).unwrap();
        assert!(matches!(watcher.program, Program::Coff(_)));
        assert!(Watcher::new(vec![b'M', b'Z', 0, 0].into()).is_none());
    }

    #[test]
    fn coff_text_is_found_where_its_code_segment_runs() {
        let (exe, text) = program();
        let image = program_image(&exe, None).unwrap();
        let mut coff = Coff::new(&image).unwrap();
        assert_eq!(coff.anchors.len(), 8);
        let start = (BASE + TEXT_VADDR) as usize;
        assert_eq!(coff.candidate(running(TEXT_VADDR + 9)), Some(start));
        // The stub's 16-bit code, EIPs outside the text and a segment that
        // does not cover the text are not the program running.
        for code in [
            CodeState {
                default32: false,
                ..running(TEXT_VADDR)
            },
            running(TEXT_VADDR - 1),
            running(TEXT_VADDR + 4096),
            CodeState {
                limit: TEXT_VADDR + 4094,
                ..running(TEXT_VADDR)
            },
        ] {
            assert_eq!(coff.candidate(code), None);
        }
        // Other bytes there are not the program's text.
        assert!(coff.accept(BASE, &[0x90; 4096]).is_none());
        assert_eq!(coff.accept(BASE, &text).as_deref(), Some(&text[..]));
        assert!(coff.accept(BASE, &text).is_none());
    }

    #[test]
    fn patched_text_is_reported_once_settled_and_a_moved_block_at_once() {
        let (exe, text) = program();
        let image = program_image(&exe, None).unwrap();
        let mut coff = Coff::new(&image).unwrap();
        coff.accept(BASE, &text).unwrap();
        let mut patched = text.clone();
        patched[100..104].copy_from_slice(&0x18_007c_u32.to_le_bytes());
        assert!(coff.accept(BASE, &patched).is_none());
        let mut again = patched.clone();
        again[200] ^= 1;
        // Still changing: the newer text waits for the next check.
        assert!(coff.accept(BASE, &again).is_none());
        assert_eq!(coff.accept(BASE, &again).as_deref(), Some(&again[..]));
        assert!(coff.accept(BASE, &again).is_none());
        // A block moved by DPMI 0503h is a new layout at once.
        assert_eq!(coff.accept(0x30_0000, &again).as_deref(), Some(&again[..]));
    }

    #[test]
    fn coff_layouts_name_their_base_and_loaded_text() {
        let (exe, text) = program();
        let image = program_image(&exe, Some(&text)).unwrap();
        let layout = Layout::coff(&image, BASE, 0xaf, text.clone().into()).unwrap();
        let data = data_vaddr(4096);
        assert_eq!(
            layout.bases(),
            [
                (1, BASE + TEXT_VADDR),
                (2, BASE + data),
                (3, BASE + data + 4)
            ]
        );
        assert_eq!(
            layout.code,
            CodeSpace {
                base: BASE,
                limit: TEXT_VADDR + 4095
            }
        );
        // Analysis binds the sections at their offsets in the selectors.
        assert_eq!(layout.bindings[0].linear_base, TEXT_VADDR);
        assert_eq!(layout.bindings[0].normal.unwrap().value, 0xaf);
        assert!(layout.key().starts_with("coff-120000-"));
        assert_eq!(locate(&layout.objects, BASE + TEXT_VADDR + 5), Some((1, 5)));
        let mut patched = text.clone();
        patched[7] ^= 0xff;
        let image = program_image(&exe, Some(&patched)).unwrap();
        let other = Layout::coff(&image, BASE, 0xaf, patched.into()).unwrap();
        assert_ne!(layout.key(), other.key());
        assert_eq!(layout.bases(), other.bases());
        let moved = Layout::coff(&image, 0x30_0000, 0xaf, other.text.clone().unwrap()).unwrap();
        assert!(moved.key().starts_with("coff-300000-"));
        assert!(Layout::coff(&image, u32::MAX - 0x100, 0xaf, text.into()).is_err());
    }
}
