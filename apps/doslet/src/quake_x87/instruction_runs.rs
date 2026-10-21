//! Executed-stream boundary inventory. These are boundary-only upper bounds,
//! not AOT lowering coverage, speed estimates, or instruction timing samples.
use super::*;
use doslet_pc::InstructionObservation;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const POLICIES: [&str; 5] = [
    "scalar; stop at every x87, CALL/RET and memory-dependent loop exit",
    "admit FLD32/FMUL32/FADD32/FISTP32; retain other boundaries",
    "admit every x87; retain CALL/RET and memory-loop exits",
    "admit every x87 and CALL/RET; retain memory-loop exits",
    "admit every x87, CALL/RET and memory-loop exits",
];
const MAX_IDENTITIES: usize = 262_144;
const MAX_INTERVALS: usize = 131_072;
const MAX_HISTOGRAM_LENGTHS: usize = 262_144;
const MAX_BOUNDARY_SITES: usize = 262_144;
const MAX_LOOP_LINKS: usize = 1_048_576;
const MAX_X87_SITES: usize = 8_192;
const TOP_X87_SITES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct X87SiteKey {
    linear: u32,
    code32: bool,
    opcode: u8,
    modrm: u8,
    length: u8,
    bytes: [u8; 15],
}

#[derive(Default)]
struct X87Inventory {
    // Dense counters avoid allocating while classifying each observed x87
    // instruction: [opcode D8..DF][ModR/M reg field][register or memory].
    forms: [[[u64; 2]; 8]; 8],
    missing_modrm: u64,
    sites: HashMap<X87SiteKey, u64>,
    overflow_executions: u64,
}

impl X87Inventory {
    fn with_capacity() -> Self {
        Self {
            sites: HashMap::with_capacity(MAX_X87_SITES),
            ..Self::default()
        }
    }

    fn record(&mut self, key: Key) {
        let bytes = &key.bytes[..usize::from(key.available)];
        let mut at = 0;
        let mut address32 = key.code32;
        while let Some(&prefix) = bytes.get(at) {
            match prefix {
                0x66 | 0xf2 | 0xf3 | 0xf0 | 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 => {}
                0x67 => address32 = !key.code32,
                _ => break,
            }
            at += 1;
        }
        let Some(&opcode) = bytes.get(at).filter(|&&op| (0xd8..=0xdf).contains(&op)) else {
            return;
        };
        let Some(&modrm) = bytes.get(at + 1) else {
            self.missing_modrm += 1;
            return;
        };

        let reg = usize::from((modrm >> 3) & 7);
        let memory = usize::from(modrm >> 6 != 3);
        self.forms[usize::from(opcode - 0xd8)][reg][memory] += 1;

        let available_len = bytes.len().min(15);
        let decoded_len = modrm_end(bytes, at + 1, address32).unwrap_or(at + 2);
        let length = available_len.min(decoded_len);
        let mut raw = [0; 15];
        raw[..length].copy_from_slice(&bytes[..length]);
        let site = X87SiteKey {
            linear: key.linear,
            code32: key.code32,
            opcode,
            modrm,
            length: length as u8,
            bytes: raw,
        };
        if let Some(count) = self.sites.get_mut(&site) {
            *count += 1;
        } else if self.sites.len() < MAX_X87_SITES {
            // The table reserves its full bounded capacity before replay, so
            // observing a new site does not grow or reallocate the table.
            self.sites.insert(site, 1);
        } else {
            self.overflow_executions += 1;
        }
    }

    fn report(&self) {
        eprintln!("x87 live-byte form inventory (opcode / ModR/M.reg / memory-vs-register):");
        for opcode_index in 0..8 {
            for reg in 0..8 {
                let [register, memory] = self.forms[opcode_index][reg];
                if memory != 0 || register != 0 {
                    eprintln!(
                        "  {:02x} /{reg}: memory={memory} register={register}",
                        0xd8 + opcode_index as u8
                    );
                }
            }
        }
        eprintln!(
            "x87 entries without ModR/M={}; site-table capacity={} distinct encodings; overflow executions={}",
            self.missing_modrm, MAX_X87_SITES, self.overflow_executions
        );

        let mut top: Vec<_> = self.sites.iter().collect();
        top.sort_unstable_by(|(left_key, left_count), (right_key, right_count)| {
            right_count
                .cmp(left_count)
                .then_with(|| left_key.linear.cmp(&right_key.linear))
                .then_with(|| left_key.opcode.cmp(&right_key.opcode))
                .then_with(|| left_key.modrm.cmp(&right_key.modrm))
                .then_with(|| left_key.code32.cmp(&right_key.code32))
                .then_with(|| left_key.length.cmp(&right_key.length))
                .then_with(|| left_key.bytes.cmp(&right_key.bytes))
        });
        eprintln!("top {} x87 live encodings:", TOP_X87_SITES.min(top.len()));
        for (site, count) in top.into_iter().take(TOP_X87_SITES) {
            let instruction = doslet_x86::x87_decode::decode(site.opcode, site.modrm);
            eprintln!(
                "  linear={:08x} code32={} count={} bytes={:02x?} decoded={instruction:?}",
                site.linear,
                site.code32,
                count,
                &site.bytes[..usize::from(site.length)]
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    Scalar,
    X87,
    CallRet,
    Interrupt,
    Io,
    Rep,
    Indirect,
    Unknown,
}
impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Scalar => "Scalar",
            Self::X87 => "X87",
            Self::CallRet => "CALL/RET",
            Self::Interrupt => "INT/IRET",
            Self::Io => "I/O",
            Self::Rep => "REP",
            Self::Indirect => "indirect/far jump",
            Self::Unknown => "unknown decoder form",
        }
    }
}
#[derive(Clone, Copy, Debug)]
enum Flow {
    Next,
    Conditional {
        target: u32,
        next: u32,
        condition: u8,
    },
    Jump(u32),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    #[default]
    Unknown,
    Register,
    Memory,
    Derived,
}
#[derive(Clone, Copy, Debug)]
enum Flags {
    Keep,
    Set { memory: bool, registers: u8 },
    Unknown,
}
#[derive(Clone, Copy, Debug)]
enum Transfer {
    None,
    Clear(u8),
    Copy { to: u8, from: Option<u8> },
    Forget,
}
#[derive(Clone, Copy, Debug)]
struct Shape {
    kind: Kind,
    flow: Flow,
    flags: Flags,
    transfer: Transfer,
    subset: bool,
    length: Option<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    linear: u32,
    ip: u32,
    code32: bool,
    available: u8,
    bytes: [u8; 15],
}
impl From<InstructionObservation> for Key {
    fn from(i: InstructionObservation) -> Self {
        Self {
            linear: i.linear,
            ip: i.ip,
            code32: i.code32,
            available: i.available,
            bytes: i.bytes,
        }
    }
}

fn modrm_end(bytes: &[u8], at: usize, address32: bool) -> Option<usize> {
    let m = *bytes.get(at)?;
    let mode = m >> 6;
    let rm = m & 7;
    let mut end = at + 1;
    if mode == 3 {
        return Some(end);
    }
    if address32 {
        if rm == 4 {
            let sib = *bytes.get(end)?;
            end += 1;
            if mode == 0 && sib & 7 == 5 {
                end += 4;
            }
        } else if mode == 0 && rm == 5 {
            end += 4;
        }
        end += match mode {
            1 => 1,
            2 => 4,
            _ => 0,
        };
    } else {
        end += match mode {
            0 if rm == 6 => 2,
            1 => 1,
            2 => 2,
            _ => 0,
        };
    }
    (end <= bytes.len()).then_some(end)
}

fn classify(key: Key) -> Shape {
    let bytes = &key.bytes[..usize::from(key.available)];
    let mut shape = Shape {
        kind: Kind::Unknown,
        flow: Flow::Next,
        flags: Flags::Unknown,
        transfer: Transfer::Forget,
        subset: false,
        length: None,
    };
    let mut at = 0;
    let mut operand32 = key.code32;
    let mut address32 = key.code32;
    let mut rep = false;
    let mut locked = false;
    while let Some(&prefix) = bytes.get(at) {
        match prefix {
            0x66 => operand32 = !key.code32,
            0x67 => address32 = !key.code32,
            0xf2 | 0xf3 => rep = true,
            0xf0 => locked = true,
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 => {}
            _ => break,
        }
        at += 1;
    }
    let Some(&op) = bytes.get(at) else {
        return shape;
    };
    let ordinary_length = doslet_x86::translation::instruction_length(bytes, key.code32);
    shape.length = ordinary_length;
    shape.kind = if ordinary_length.is_some() {
        Kind::Scalar
    } else {
        Kind::Unknown
    };
    let m = bytes.get(at + 1).copied();
    let memory = m.is_some_and(|m| m >> 6 != 3);
    let reg = m.map(|m| (m >> 3) & 7).unwrap_or(0);
    let rm = m.map(|m| m & 7).unwrap_or(0);
    let registers = (1 << reg) | if memory { 0 } else { 1 << rm };
    let grouped_registers = if memory { 0 } else { 1 << rm };
    // This whitelist tracks common integer flag producers. Everything else
    // invalidates provenance rather than inheriting an earlier CMP accidentally.
    shape.flags = match op {
        0x00..=0x3d if op & 7 <= 5 && !matches!(op >> 3, 2 | 3) => Flags::Set {
            memory: op & 7 <= 3 && memory,
            registers: if op & 7 <= 3 { registers } else { 1 },
        },
        0x40..=0x4f => Flags::Set {
            memory: false,
            registers: 1 << (op & 7),
        },
        0x80..=0x83 if !matches!(reg, 2 | 3) => Flags::Set {
            memory,
            registers: grouped_registers,
        },
        0x84 | 0x85 => Flags::Set { memory, registers },
        0xa8 | 0xa9 => Flags::Set {
            memory: false,
            registers: 1,
        },
        0xf6 | 0xf7 if matches!(reg, 0 | 3) => Flags::Set {
            memory,
            registers: grouped_registers,
        },
        0xf6 | 0xf7 if reg == 2 => Flags::Keep,
        0xfe | 0xff if reg <= 1 => Flags::Set {
            memory,
            registers: grouped_registers,
        },
        0x70..=0x7f
        | 0x88..=0x8f
        | 0x90
        | 0xa0..=0xa3
        | 0xb0..=0xbf
        | 0x50..=0x5f
        | 0xc6
        | 0xc7
        | 0xe9
        | 0xeb => Flags::Keep,
        _ => Flags::Unknown,
    };
    let byte_flags = matches!(op, 0x80 | 0x82 | 0x84 | 0xa8 | 0xf6 | 0xfe)
        || op <= 0x3d && op & 7 <= 5 && op & 1 == 0;
    if matches!(shape.flags, Flags::Set { memory: false, .. }) && (byte_flags || !operand32) {
        // Avoid treating AH/CH/DH/BH as ESP/EBP/ESI/EDI, or pretending a
        // partial-register update preserved tracked whole-register provenance.
        shape.flags = Flags::Unknown;
    }
    shape.transfer = match op {
        0x8b if operand32 => Transfer::Copy {
            to: reg,
            from: (!memory).then_some(rm),
        },
        0x89 if operand32 && !memory => Transfer::Copy {
            to: rm,
            from: Some(reg),
        },
        0xb8..=0xbf => Transfer::Clear(op & 7),
        0xc6 | 0xc7 if !memory => Transfer::Clear(rm),
        0x8d => Transfer::Clear(reg), // address calculation, not a loaded value
        0xa1 => Transfer::Copy { to: 0, from: None },
        0x90 | 0x70..=0x7f | 0xe9 | 0xeb => Transfer::None,
        0x38..=0x3d | 0x84 | 0x85 | 0xa8 | 0xa9 => Transfer::None,
        0x80..=0x83 if reg == 7 => Transfer::None,
        _ => Transfer::Forget,
    };
    if operand32 && !memory && reg == rm && matches!(op, 0x29 | 0x2b | 0x31 | 0x33 | 0x39 | 0x3b) {
        // SUB/XOR/CMP r,r produce constant flags independent of value origin.
        shape.flags = Flags::Set {
            memory: false,
            registers: 0,
        };
        if !matches!(op, 0x39 | 0x3b) {
            shape.transfer = Transfer::Clear(rm);
        }
    }
    if (0xd8..=0xdf).contains(&op) {
        shape.kind = Kind::X87;
        shape.length = modrm_end(bytes, at + 1, address32).and_then(|n| u8::try_from(n).ok());
        shape.flags = Flags::Keep;
        // FNSTSW AX makes whole-EAX MOV provenance unavailable.
        shape.transfer = if op == 0xdf && m == Some(0xe0) {
            Transfer::Clear(0)
        } else {
            Transfer::None
        };
        shape.subset = memory && matches!((op, reg), (0xd9, 0) | (0xd8, 0 | 1) | (0xdb, 3));
        if rep || locked || shape.length.is_none() || !m.is_some_and(|m|matches!(doslet_x86::x87_decode::decode(op,m),doslet_x86::x87_decode::Encoding::I387(i) if i.implemented())) {
            shape.kind=Kind::Unknown;
            shape.subset=false;
        }
    } else if rep && matches!(op, 0x6c..=0x6f | 0xa4..=0xa7 | 0xaa..=0xaf) {
        shape.kind = Kind::Rep;
        shape.length = Some((at + 1) as u8);
    } else if matches!(op, 0xe8 | 0x9a | 0xc2 | 0xc3 | 0xca | 0xcb)
        || op == 0xff && matches!(reg, 2 | 3)
    {
        shape.kind = Kind::CallRet;
    } else if matches!(op, 0xcc..=0xcf | 0xf1) {
        shape.kind = Kind::Interrupt;
    } else if matches!(op, 0xe4..=0xe7 | 0xec..=0xef | 0x6c..=0x6f) {
        shape.kind = Kind::Io;
    } else if op == 0xff && matches!(reg, 4 | 5) || op == 0xea {
        shape.kind = Kind::Indirect;
    }
    let relative = if matches!(op, 0x70..=0x7f | 0xe0..=0xe3 | 0xeb) {
        bytes.get(at + 1).map(|&v| (at + 2, i32::from(v as i8)))
    } else if op == 0xe9
        || op == 0x0f && bytes.get(at + 1).is_some_and(|v| (0x80..=0x8f).contains(v))
    {
        let start = at + if op == 0x0f { 2 } else { 1 };
        if operand32 {
            bytes
                .get(start..start + 4)
                .map(|v| (start + 4, i32::from_le_bytes(v.try_into().unwrap())))
        } else {
            bytes.get(start..start + 2).map(|v| {
                (
                    start + 2,
                    i32::from(i16::from_le_bytes(v.try_into().unwrap())),
                )
            })
        }
    } else {
        None
    };
    if let Some((length, displacement)) = relative {
        let next_ip = key.ip.wrapping_add(length as u32);
        let target_ip = next_ip.wrapping_add_signed(displacement);
        let mask = if operand32 || matches!(op, 0x70..=0x7f | 0xe0..=0xe3 | 0xeb) && key.code32 {
            u32::MAX
        } else {
            0xffff
        };
        let base = key.linear.wrapping_sub(key.ip);
        let next = base.wrapping_add(next_ip & if key.code32 { u32::MAX } else { 0xffff });
        let target = base.wrapping_add(target_ip & mask);
        shape.length = Some(length as u8);
        shape.kind = Kind::Scalar;
        shape.flow = if matches!(op, 0xe9 | 0xeb) {
            Flow::Jump(target)
        } else {
            Flow::Conditional {
                target,
                next,
                condition: if op == 0x0f {
                    bytes[at + 1] & 15
                } else if op < 0x80 {
                    op & 15
                } else {
                    16
                },
            }
        };
        shape.flags = Flags::Keep;
        shape.transfer = Transfer::None;
    }
    if shape.length.is_none() && shape.kind == Kind::Scalar {
        shape.kind = Kind::Unknown;
    }
    shape
}

fn unexpected_post(key: Key, shape: Shape, post: u32) -> bool {
    match shape.flow {
        Flow::Conditional { target, next, .. } => post != target && post != next,
        Flow::Jump(target) => post != target,
        Flow::Next if matches!(shape.kind, Kind::Scalar | Kind::X87) => {
            shape.length.is_some_and(|length| {
                let mask = if key.code32 { u32::MAX } else { 0xffff };
                let next = key
                    .linear
                    .wrapping_sub(key.ip)
                    .wrapping_add(key.ip.wrapping_add(u32::from(length)) & mask);
                post != next
            })
        }
        _ => false,
    }
}

#[derive(Default)]
struct Provenance {
    flags: Origin,
    memory_registers: u8,
}
impl Provenance {
    fn update(&mut self, shape: Shape) {
        self.flags = match shape.flags {
            Flags::Keep => self.flags,
            Flags::Unknown => Origin::Unknown,
            Flags::Set { memory: true, .. } => Origin::Memory,
            Flags::Set { registers, .. } if registers & self.memory_registers != 0 => {
                Origin::Derived
            }
            Flags::Set { .. } => Origin::Register,
        };
        match shape.transfer {
            Transfer::Forget => self.memory_registers = 0,
            Transfer::Clear(reg) => self.memory_registers &= !(1 << reg),
            Transfer::Copy { to, from } => {
                let memory = from.is_none_or(|from| self.memory_registers & (1 << from) != 0);
                self.memory_registers =
                    (self.memory_registers & !(1 << to)) | if memory { 1 << to } else { 0 };
            }
            Transfer::None => {}
        }
    }
}

#[derive(Default)]
struct Runs {
    current: u64,
    histogram: BTreeMap<u64, u64>,
    excluded: u64,
    boundaries: HashMap<(&'static str, u32), u64>,
    empty: u64,
}
impl Runs {
    fn flush(&mut self) {
        if self.current != 0 {
            assert!(
                self.histogram.len() < MAX_HISTOGRAM_LENGTHS
                    || self.histogram.contains_key(&self.current),
                "histogram diagnostic storage cap exceeded"
            );
            *self.histogram.entry(self.current).or_default() += 1;
            self.current = 0;
        } else {
            self.empty += 1;
        }
    }
    fn boundary(&mut self, label: &'static str, address: u32, excluded: bool) {
        self.flush();
        self.excluded += u64::from(excluded);
        assert!(
            self.boundaries.len() < MAX_BOUNDARY_SITES
                || self.boundaries.contains_key(&(label, address)),
            "boundary diagnostic storage cap exceeded"
        );
        *self.boundaries.entry((label, address)).or_default() += 1;
    }
    fn report(&mut self, name: &str, instructions: u64) {
        self.flush();
        let runs: u64 = self.histogram.values().sum();
        let payload: u64 = self.histogram.iter().map(|(length, n)| length * n).sum();
        let quantile = |weighted: bool, percent: u64| {
            let total = if weighted { payload } else { runs };
            let mut cumulative = 0;
            for (&length, &n) in &self.histogram {
                cumulative += if weighted { length * n } else { n };
                if cumulative.saturating_mul(100) >= total.saturating_mul(percent) {
                    return length;
                }
            }
            0
        };
        eprintln!(
            "\nBOUNDARY-ONLY UPPER BOUND: {name}\npayload={payload} ({:.3}% observed entries), excluded={} ({:.3}%), runs={runs}, empty boundaries={}, mean={:.3}, max={}; run p50/p90/p99={}/{}/{}, instruction-weighted p50/p90/p99={}/{}/{}",
            payload as f64 * 100.0 / instructions.max(1) as f64,
            self.excluded,
            self.excluded as f64 * 100.0 / instructions.max(1) as f64,
            self.empty,
            payload as f64 / runs.max(1) as f64,
            self.histogram.keys().next_back().copied().unwrap_or(0),
            quantile(false, 50),
            quantile(false, 90),
            quantile(false, 99),
            quantile(true, 50),
            quantile(true, 90),
            quantile(true, 99)
        );
        let mut bins = BTreeMap::<u32, (u64, u64)>::new();
        for (&length, &n) in &self.histogram {
            let bin = length.ilog2();
            let row = bins.entry(bin).or_default();
            row.0 += n;
            row.1 += n * length;
        }
        for (bin, (n, work)) in bins {
            eprintln!(
                "  {:>9}..{:<9} runs={n:>12} instructions={work:>12} weighted_share={:.3}%",
                1u64 << bin,
                (1u64 << (bin + 1)) - 1,
                work as f64 * 100.0 / payload.max(1) as f64
            );
        }
        let mut top: Vec<_> = self.boundaries.iter().collect();
        top.sort_unstable_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for ((reason, address), n) in top.into_iter().take(20) {
            eprintln!("  boundary {address:08x} {reason}: {n}");
        }
        assert_eq!(payload + self.excluded, instructions);
    }
}

fn session(state: &[u8], media: &Session) -> Session {
    let mut session = borrowed_bus_session(state, media, true);
    session.set_dynamic_core(false);
    session.set_idle_loops(false);
    session.set_idle_observation(false);
    session.set_adaptive_idle(false);
    session.set_synchronization_analysis(false);
    session.set_adaptive_cpu(false);
    session.set_vga_wait_acceleration(false);
    session
}

const DEFAULT_UNKNOWN_SITES: [u32; 4] = [0x1310eb, 0x130cc5, 0x13147e, 0x155498];

fn site_arguments(value: Option<&str>) -> Result<Vec<u32>, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_UNKNOWN_SITES.to_vec());
    };
    let sites: Result<Vec<_>, _> = value
        .split(',')
        .map(|part| {
            let part = part.trim();
            let hex = part
                .strip_prefix("0x")
                .or_else(|| part.strip_prefix("0X"))
                .unwrap_or(part);
            u32::from_str_radix(hex, 16)
                .map_err(|_| format!("invalid hexadecimal linear address {part:?}"))
        })
        .collect();
    let sites = sites?;
    if sites.is_empty() || sites.len() > 64 {
        return Err("QUAKE_RUN_SITES must contain 1..=64 hexadecimal linear addresses".into());
    }
    Ok(sites)
}

/// Read a few sites from the identical warmed cache without a histogram replay.
/// `QUAKE_RUN_SITES=1310eb,130cc5,...` overrides the four default linear sites.
#[test]
#[ignore = "requires local Quake; targeted code inspection, not an executed-stream sample"]
fn quake_instruction_site_bytes() {
    let sites = site_arguments(std::env::var("QUAKE_RUN_SITES").ok().as_deref()).unwrap();
    let warmup = benchmark_setting("QUAKE_BENCH_WARMUP_STEPS", 10_000_000, true);
    let (state, media) = demo_state();
    eprintln!(
        "site probe cached source sha256={:x}; warmup={warmup}",
        Sha256::digest(&state)
    );
    let mut session = session(&state, &media);
    advance_exactly(&mut session, warmup, None);
    let state = session.save_state().unwrap();
    let registers = session.registers();
    let base = registers
        .segment_cache(doslet_x86::SegmentRegister::Cs)
        .base;
    eprintln!(
        "site probe measured-start sha256={:x}; current CS={:04x} base={base:08x} code32={}",
        Sha256::digest(&state),
        registers.cs,
        registers.code32()
    );
    eprintln!(
        "These are BASELINE BYTES in the warmed cached state, not captured executed SMC versions. Both code modes below are diagnostic interpretations, not observed entry modes."
    );
    for linear in sites {
        let bytes = (1..=15)
            .rev()
            .find_map(|length| session.debug_code_bytes(linear, length));
        let Some(bytes) = bytes else {
            eprintln!("site {linear:08x}: unavailable immutable RAM/code view");
            continue;
        };
        eprintln!("site {linear:08x} baseline bytes={bytes:02x?}");
        let mut raw = [0; 15];
        raw[..bytes.len()].copy_from_slice(bytes);
        for code32 in [false, true] {
            let key = Key {
                linear,
                ip: linear.wrapping_sub(base),
                code32,
                available: bytes.len() as u8,
                bytes: raw,
            };
            eprintln!(
                "  code32={code32}: ordinary_length={:?}, ordinary_flow={:?}, local_shape={:?}",
                doslet_x86::translation::instruction_length(bytes, code32),
                doslet_x86::translation::near_flow(bytes, code32),
                classify(key)
            );
        }
    }
}

fn report_unknown_variants(shapes: &HashMap<Key, Shape>, runs: &Runs) {
    let mut sites: Vec<_> = runs
        .boundaries
        .iter()
        .filter_map(|(&(reason, address), &count)| {
            (reason == Kind::Unknown.label()).then_some((address, count))
        })
        .collect();
    sites.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    eprintln!(
        "Captured executed Unknown byte variants for top20 Unknown boundary sites (maximum16 variants/site; variant frequency is not collected):"
    );
    for (address, count) in sites.into_iter().take(20) {
        let mut variants: Vec<_> = shapes
            .iter()
            .filter_map(|(key, shape)| {
                (key.linear == address && shape.kind == Kind::Unknown).then_some(key)
            })
            .collect();
        variants.sort_unstable_by_key(|key| (key.code32, key.ip, key.available, key.bytes));
        eprintln!(
            "  unknown site {address:08x}: {count} entries, {} observed byte identities",
            variants.len()
        );
        for key in variants.iter().take(16) {
            eprintln!(
                "    CSbase={:08x} EIP={:08x} code32={} available={} bytes={:02x?}",
                key.linear.wrapping_sub(key.ip),
                key.ip,
                key.code32,
                key.available,
                &key.bytes[..usize::from(key.available)]
            );
        }
        if variants.len() > 16 {
            eprintln!(
                "    {} further observed identities omitted",
                variants.len() - 16
            );
        }
    }
}

#[test]
#[ignore = "requires local Quake; two exhaustive diagnostic passes, not a host timing benchmark"]
fn quake_executed_instruction_run_histogram() {
    let steps = benchmark_setting("QUAKE_RUN_STEPS", 400_000_000, false);
    let warmup = benchmark_setting("QUAKE_BENCH_WARMUP_STEPS", 10_000_000, true);
    let (state, media) = demo_state();
    eprintln!(
        "Quake instruction inventory source sha256={:x}; warmup={warmup}, measured scheduling steps={steps}, passes=2",
        Sha256::digest(&state)
    );
    let mut warm = session(&state, &media);
    advance_exactly(&mut warm, warmup, None);
    let state = warm.save_state().unwrap();
    eprintln!(
        "exact measured start state sha256={:x}",
        Sha256::digest(&state)
    );
    let mut shapes = HashMap::<Key, Shape>::new();
    // Namespace intervals by CS base and code mode; EIP alone aliases DJGPP's
    // protected selectors and real-mode service code.
    let mut intervals = BTreeSet::<(u32, bool, u32, u32)>::new();
    let mut exit_intervals = HashMap::<Key, Vec<(u32, u32)>>::new();
    let mut first_pass_checkpoints = Vec::new();
    let mut x87_inventory = X87Inventory::with_capacity();
    let mut runs: [Runs; 5] = std::array::from_fn(|_| Runs::default());
    for pass in 0..2 {
        let mut observed = session(&state, &media);
        let mut ordinary = (pass == 1).then(|| session(&state, &media));
        let mut audio = Sha256::new();
        let mut ordinary_audio = Sha256::new();
        let mut provenance = Provenance::default();
        let mut counts = BTreeMap::<Kind, u64>::new();
        let mut precision = BTreeMap::<(u16, u16, bool), u64>::new();
        let mut predicates = BTreeMap::<(&'static str, Origin), u64>::new();
        let mut no_instruction = 0u64;
        let mut entries = 0u64;
        let mut rep_completions = 0u64;
        let mut prior_rep = None;
        let mut rep_continuations = 0u64;
        let mut redirected_entries = 0u64;
        let mut expected_entry = None;
        let start = observed.steps();
        if pass == 1 {
            let mut links = 0usize;
            for (&key, shape) in &shapes {
                if matches!(shape.flow, Flow::Conditional { .. }) {
                    let base = key.linear.wrapping_sub(key.ip);
                    let mut containing = Vec::new();
                    for &(context, mode, low, high) in &intervals {
                        if base == context
                            && key.code32 == mode
                            && low <= key.linear
                            && key.linear <= high
                        {
                            links += 1;
                            assert!(
                                links <= MAX_LOOP_LINKS,
                                "loop membership diagnostic storage cap exceeded"
                            );
                            containing.push((low, high));
                        }
                    }
                    exit_intervals.insert(key, containing);
                }
            }
        }
        for checkpoint in 1..=8 {
            let target = start + steps * checkpoint / 8;
            while observed.steps() < target {
                assert_eq!(
                    observed.status(),
                    SessionStatus::Running,
                    "{:?}",
                    observed.fault()
                );
                let before = observed.steps();
                let observation = observed.debug_observed_step_configured();
                assert_eq!(
                    observed.status(),
                    SessionStatus::Running,
                    "{:?}",
                    observed.fault()
                );
                assert_eq!(
                    observed.steps(),
                    before + 1,
                    "diagnostic step must remain one scheduling unit"
                );
                if (observed.steps() - start).is_multiple_of(10_000) {
                    drain(&mut observed, Some(&mut audio));
                }
                if observation.service_writes_unobserved
                    || observation.service_device_accesses_unobserved
                    || observation.trace.incomplete
                {
                    provenance = Provenance::default();
                    prior_rep = None;
                    expected_entry = None;
                    if pass == 1 {
                        for run in &mut runs {
                            run.boundary("runtime service / incomplete trace", 0, false);
                        }
                    }
                }
                let Some(entry) = observation.instruction else {
                    no_instruction += 1;
                    provenance = Provenance::default();
                    prior_rep = None;
                    if pass == 1 {
                        for run in &mut runs {
                            run.boundary(
                                if observation.irq.is_some() {
                                    "hardware IRQ"
                                } else {
                                    "service/HLT, no x86 entry"
                                },
                                0,
                                false,
                            );
                        }
                    }
                    continue;
                };
                if expected_entry.is_some_and(|expected| expected != entry.linear) {
                    provenance = Provenance::default();
                    prior_rep = None;
                    if pass == 1 {
                        for run in &mut runs {
                            run.boundary("post-service entry discontinuity", entry.linear, false);
                        }
                    }
                }
                let key = Key::from(entry);
                assert!(
                    shapes.len() < MAX_IDENTITIES || shapes.contains_key(&key),
                    "instruction identity diagnostic storage cap exceeded"
                );
                let shape = *shapes.entry(key).or_insert_with(|| classify(key));
                if pass == 1 {
                    x87_inventory.record(key);
                }
                let post = observed.debug_pc();
                expected_entry = Some(post);
                let redirected = unexpected_post(key, shape, post);
                redirected_entries += u64::from(redirected);
                entries += 1;
                *counts.entry(shape.kind).or_default() += 1;
                if shape.kind == Kind::Rep {
                    rep_continuations += u64::from(prior_rep == Some(entry.linear));
                    rep_completions += u64::from(post != entry.linear);
                    prior_rep = (post == entry.linear).then_some(entry.linear);
                } else {
                    prior_rep = None;
                }
                if shape.kind == Kind::X87
                    && let Some(fpu) = observed.debug_cpu().fpu.as_ref()
                {
                    let cw = fpu.control();
                    *precision
                        .entry(((cw >> 8) & 3, (cw >> 10) & 3, cw & 0x3f == 0x3f))
                        .or_default() += 1;
                }
                let mut memory_exit = false;
                match shape.flow {
                    Flow::Jump(target) | Flow::Conditional { target, .. }
                        if target < entry.linear && post == target && pass == 0 =>
                    {
                        let interval = (
                            entry.linear.wrapping_sub(entry.ip),
                            entry.code32,
                            target,
                            entry.linear,
                        );
                        assert!(
                            intervals.len() < MAX_INTERVALS || intervals.contains(&interval),
                            "backward-edge diagnostic storage cap exceeded"
                        );
                        intervals.insert(interval);
                    }
                    _ => {}
                }
                if let Flow::Conditional {
                    target: branch_target,
                    next,
                    condition,
                } = shape.flow
                {
                    let actual = if post == branch_target {
                        Some(branch_target)
                    } else if post == next {
                        Some(next)
                    } else {
                        None
                    };
                    if pass == 1 {
                        let exits = actual.is_some_and(|destination| {
                            exit_intervals.get(&key).is_some_and(|loops| {
                                loops
                                    .iter()
                                    .any(|&(low, high)| destination < low || destination > high)
                            })
                        });
                        if !exit_intervals.contains_key(&key) {
                            *predicates
                                .entry(("loop membership unavailable", Origin::Unknown))
                                .or_default() += 1;
                        }
                        // CF dependencies, ADC/SBB, partial/undefined flags and
                        // LOOP's ECX condition are not modeled as memory proof.
                        let origin = if matches!(condition, 2 | 3 | 6 | 7 | 16) {
                            Origin::Unknown
                        } else {
                            provenance.flags
                        };
                        if exits {
                            *predicates.entry(("loop exit", origin)).or_default() += 1;
                            memory_exit = matches!(origin, Origin::Memory | Origin::Derived);
                        } else {
                            *predicates
                                .entry(("conditional nonexit", origin))
                                .or_default() += 1;
                        }
                        if actual.is_none() {
                            *predicates
                                .entry(("conditional unexpected post-PC", Origin::Unknown))
                                .or_default() += 1;
                        }
                    }
                }
                if pass == 1 {
                    for (policy, run) in runs.iter_mut().enumerate() {
                        let blocked = match shape.kind {
                            Kind::Scalar => false,
                            Kind::X87 => policy == 0 || policy == 1 && !shape.subset,
                            Kind::CallRet => policy < 3,
                            _ => true,
                        };
                        if blocked {
                            run.boundary(shape.kind.label(), entry.linear, true);
                        } else {
                            run.current += 1;
                            if memory_exit && policy < 4 {
                                run.boundary("memory-dependent loop exit", entry.linear, false);
                            }
                        }
                    }
                }
                provenance.update(shape);
                if matches!(
                    shape.kind,
                    Kind::CallRet
                        | Kind::Interrupt
                        | Kind::Io
                        | Kind::Rep
                        | Kind::Indirect
                        | Kind::Unknown
                ) {
                    provenance = Provenance::default();
                }
                if redirected {
                    provenance = Provenance::default();
                    prior_rep = None;
                    if pass == 1 {
                        for run in &mut runs {
                            run.boundary(
                                "unexpected post-PC / possible exception",
                                entry.linear,
                                false,
                            );
                        }
                    }
                }
            }
            drain(&mut observed, Some(&mut audio));
            let state_digest = Sha256::digest(observed.save_state().unwrap());
            let audio_digest = audio.clone().finalize();
            if pass == 0 {
                first_pass_checkpoints.push((state_digest, audio_digest));
            } else {
                assert_eq!(
                    (state_digest, audio_digest),
                    first_pass_checkpoints[checkpoint as usize - 1],
                    "inventory and measurement windows differ"
                );
            }
            if let Some(ordinary) = &mut ordinary {
                let remaining = target - ordinary.steps();
                advance_exactly(ordinary, remaining, Some(&mut ordinary_audio));
                assert_eq!(observed.emulated_nanos(), ordinary.emulated_nanos());
                assert!(
                    observed.save_state().unwrap() == ordinary.save_state().unwrap(),
                    "observed vs ordinary full state differs at checkpoint {checkpoint}"
                );
                assert_eq!(
                    audio.clone().finalize(),
                    ordinary_audio.clone().finalize(),
                    "observed vs ordinary PCM/MIDI differs"
                );
            }
            eprintln!(
                "inventory pass{} checkpoint{checkpoint}/8: {} scheduling steps, {entries} observed x86 entries, {no_instruction} noninstruction steps, {} decoded identities, {} backward-edge intervals",
                pass + 1,
                observed.steps() - start,
                shapes.len(),
                intervals.len()
            );
        }
        eprintln!(
            "pass{} entry classes {counts:?}; REP continuations={rep_continuations}, post-PC departures={rep_completions}; unexpected post-PC transitions={redirected_entries}; CW(PC,RC,allmasked) {precision:?}",
            pass + 1
        );
        if pass == 1 {
            x87_inventory.report();
            eprintln!("predicate inventory {predicates:?}");
            eprintln!(
                "Definitions: scheduling steps include service/IRQ/HLT and each REP quantum. x86 entries count actual post-service instruction attempts; handled exceptions may prevent completion. REP continuations are NOT additional architectural instruction completions. Non-REP entries={} ; REP entries={}, REP post-PC departures={rep_completions}. Histograms exclude all REP quanta as boundaries. Interrupt/service boundaries split runs without adding x86 entries. Unknown decoder forms are conservative barriers. All five policies are boundary-only upper bounds: they do not prove AOT lowering, grants, numeric admission or speed. Memory exits use observed backward-edge intervals and actual conditional-branch destinations; Memory means direct memory flag producer, Derived means tracked MOV-load register provenance, Unknown is unproven. This is not a dominance/induction proof. Existing ordinary replay state+PCM/MIDI matched at8checkpoints.",
                entries - counts.get(&Kind::Rep).copied().unwrap_or(0),
                counts.get(&Kind::Rep).copied().unwrap_or(0)
            );
            for (run, name) in runs.iter_mut().zip(POLICIES) {
                run.report(name, entries);
            }
            report_unknown_variants(&shapes, &runs[0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shape(bytes: &[u8]) -> Shape {
        let mut raw = [0; 15];
        raw[..bytes.len()].copy_from_slice(bytes);
        classify(Key {
            linear: 0x101000,
            ip: 0x1000,
            code32: true,
            available: bytes.len() as u8,
            bytes: raw,
        })
    }
    #[test]
    fn classes_use_instruction_boundaries_and_prefixes() {
        assert_eq!(shape(&[0xd9, 0x44, 0x24, 4]).kind, Kind::X87);
        assert_eq!(shape(&[0xd9, 0x44, 0x24, 4]).length, Some(4));
        assert!(shape(&[0xd8, 0x0d, 0, 0, 0, 0]).subset);
        assert!(!shape(&[0xd8, 0xc1]).subset);
        assert_eq!(shape(&[0xff, 0xd0]).kind, Kind::CallRet);
        assert_eq!(shape(&[0xff, 0xe0]).kind, Kind::Indirect);
        assert_eq!(shape(&[0xf3, 0xa5]).kind, Kind::Rep);
        assert_eq!(shape(&[0xcd, 0x21]).kind, Kind::Interrupt);
        assert_eq!(shape(&[0x66, 0xef]).kind, Kind::Io);
        assert!(matches!(
            shape(&[0x75, 0xfc]).flow,
            Flow::Conditional {
                target: 0x100ffe,
                next: 0x101002,
                ..
            }
        ));
    }
    #[test]
    fn memory_predicate_provenance_is_invalidated_conservatively() {
        let mut p = Provenance::default();
        p.update(shape(&[0x83, 0x3d, 0, 0, 0, 0, 0]));
        assert_eq!(p.flags, Origin::Memory);
        p.update(shape(&[0x90]));
        assert_eq!(p.flags, Origin::Memory);
        p.update(shape(&[0x31, 0xc0]));
        assert_eq!(p.flags, Origin::Register);
        p.update(shape(&[0x8b, 0x05, 0, 0, 0, 0]));
        p.update(shape(&[0x83, 0xf8, 0]));
        assert_eq!(p.flags, Origin::Derived);
        p.update(shape(&[0x0f, 0x31]));
        assert_eq!(p.flags, Origin::Unknown);
    }
    #[test]
    fn histogram_counts_every_admitted_and_excluded_entry() {
        let mut r = Runs {
            current: 3,
            ..Runs::default()
        };
        r.boundary("x87", 1, true);
        r.boundary("x87", 1, true);
        r.current = 7;
        r.boundary("loop exit", 2, false);
        assert_eq!(r.histogram.get(&3), Some(&1));
        assert_eq!(r.histogram.get(&7), Some(&1));
        assert_eq!(r.excluded, 2);
        assert_eq!(r.empty, 1);
    }
    #[test]
    fn grouped_opcode_extension_is_not_a_register_dependency() {
        let mut p = Provenance::default();
        p.update(shape(&[0x8b, 0x3d, 0, 0, 0, 0])); // EDI loaded from memory.
        p.update(shape(&[0x83, 0xfb, 0])); // /7 CMP EBX,0 does not read EDI.
        assert_eq!(p.flags, Origin::Register);
        p.update(shape(&[0x83, 0xff, 0])); // CMP EDI,0 does read it.
        assert_eq!(p.flags, Origin::Derived);
    }
    #[test]
    fn self_operations_and_fnstsw_do_not_leave_stale_memory_provenance() {
        for bytes in [&[0x31, 0xc0][..], &[0x29, 0xc0], &[0x39, 0xc0]] {
            let mut p = Provenance::default();
            p.update(shape(&[0x8b, 0x05, 0, 0, 0, 0]));
            p.update(shape(bytes));
            assert_eq!(p.flags, Origin::Register);
        }
        let mut p = Provenance::default();
        p.update(shape(&[0x8b, 0x05, 0, 0, 0, 0]));
        p.update(shape(&[0xdf, 0xe0]));
        p.update(shape(&[0x83, 0xf8, 0]));
        assert_eq!(p.flags, Origin::Register);
    }
    #[test]
    fn exception_redirect_splits_even_if_next_entry_matches_previous_post_pc() {
        let mut key = Key {
            linear: 0x101000,
            ip: 0x1000,
            code32: true,
            available: 2,
            bytes: [0; 15],
        };
        key.bytes[..2].copy_from_slice(&[0x8b, 0x01]); // A memory MOV can fault.
        let decoded = classify(key);
        assert_eq!(decoded.kind, Kind::Scalar);
        assert!(!unexpected_post(key, decoded, 0x101002));
        assert!(unexpected_post(key, decoded, 0x102000));
        key.bytes[..2].copy_from_slice(&[0x75, 0xfc]);
        let decoded = classify(key);
        assert!(!unexpected_post(key, decoded, 0x100ffe));
        assert!(!unexpected_post(key, decoded, 0x101002));
        assert!(unexpected_post(key, decoded, 0x102000));
    }
    #[test]
    fn site_addresses_are_explicitly_hexadecimal_and_bounded() {
        assert_eq!(site_arguments(None).unwrap(), DEFAULT_UNKNOWN_SITES);
        assert_eq!(
            site_arguments(Some("1310eb, 0X130cc5,0x155498")).unwrap(),
            vec![0x1310eb, 0x130cc5, 0x155498]
        );
        assert!(site_arguments(Some("")).is_err());
        assert!(site_arguments(Some("0xnope")).is_err());
        assert!(site_arguments(Some(&vec!["1"; 65].join(","))).is_err());
    }
}
