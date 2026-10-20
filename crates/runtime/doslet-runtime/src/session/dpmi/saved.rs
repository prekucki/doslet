//! Portable DPMI host metadata. Descriptor tables, gates and real-mode shims
//! live in guest RAM and are saved with it; this adds the host's own records.
use super::*;
use alloc::{vec, vec::Vec};
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct DpmiMemoryStats {
    #[prost(uint64, tag = "1")]
    requested_bytes: u64,
    #[prost(uint64, tag = "2")]
    committed_bytes: u64,
    #[prost(uint64, tag = "3")]
    peak_requested_bytes: u64,
    #[prost(uint64, tag = "4")]
    peak_committed_bytes: u64,
    #[prost(uint64, tag = "5")]
    allocations: u64,
    #[prost(uint64, tag = "6")]
    frees: u64,
    #[prost(uint64, tag = "7")]
    resizes: u64,
    #[prost(uint64, tag = "8")]
    failures: u64,
}
impl State for MemoryStats {
    type Wire = DpmiMemoryStats;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            requested_bytes: self.requested_bytes,
            committed_bytes: self.committed_bytes,
            peak_requested_bytes: self.peak_requested_bytes,
            peak_committed_bytes: self.peak_committed_bytes,
            allocations: self.allocations,
            frees: self.frees,
            resizes: self.resizes,
            failures: self.failures,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Ok(Self {
            requested_bytes: w.requested_bytes,
            committed_bytes: w.committed_bytes,
            peak_requested_bytes: w.peak_requested_bytes,
            peak_committed_bytes: w.peak_committed_bytes,
            allocations: w.allocations,
            frees: w.frees,
            resizes: w.resizes,
            failures: w.failures,
        })
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DpmiStats {
    #[prost(bool, tag = "1")]
    entered: bool,
    #[prost(uint64, tag = "2")]
    calls: u64,
    #[prost(uint64, tag = "3")]
    direct: u64,
    #[prost(uint64, tag = "4")]
    reflected: u64,
    #[prost(uint64, tag = "5")]
    unsupported: u64,
    #[prost(uint32, tag = "6")]
    last_function: u32,
    #[prost(uint32, repeated, tag = "7")]
    recent_functions: Vec<u32>,
    #[prost(uint32, repeated, tag = "8")]
    unsupported_functions: Vec<u32>,
    #[prost(message, optional, tag = "9")]
    memory: Option<DpmiMemoryStats>,
}

/// One record of a fixed-size host table, with its slot.
#[derive(Clone, PartialEq, prost::Message)]
pub struct DpmiBlock {
    #[prost(uint32, tag = "1")]
    slot: u32,
    #[prost(uint32, tag = "2")]
    handle: u32,
    #[prost(uint32, tag = "3")]
    address: u32,
    #[prost(uint32, tag = "4")]
    requested: u32,
    #[prost(uint32, tag = "5")]
    committed: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DpmiFrame {
    #[prost(uint32, tag = "1")]
    slot: u32,
    #[prost(message, optional, tag = "2")]
    context: Option<<CpuContext as State>::Wire>,
    #[prost(uint32, optional, tag = "3")]
    structure: Option<u32>,
    #[prost(bool, tag = "4")]
    callback: bool,
    #[prost(bool, tag = "5")]
    timer: bool,
    #[prost(bool, tag = "6")]
    exception: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DpmiCallback {
    #[prost(uint32, tag = "1")]
    slot: u32,
    #[prost(uint32, tag = "2")]
    code: u32,
    #[prost(uint32, tag = "3")]
    ip: u32,
    #[prost(uint32, tag = "4")]
    data: u32,
    #[prost(uint32, tag = "5")]
    offset: u32,
    #[prost(uint32, tag = "6")]
    structure: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DpmiHost {
    /// 1 reflect, 2 direct.
    #[prost(uint32, tag = "1")]
    pub(super) mode: u32,
    #[prost(message, optional, tag = "2")]
    stats: Option<DpmiStats>,
    /// One bit per LDT descriptor, lowest index in the low bit.
    #[prost(bytes = "vec", tag = "3")]
    pub(super) used: Vec<u8>,
    /// `(descriptor << 16) | segment` of every DOS-block selector.
    #[prost(uint32, repeated, tag = "4")]
    dos_segments: Vec<u32>,
    #[prost(uint32, repeated, tag = "5")]
    exception_selectors: Vec<u32>,
    #[prost(uint32, repeated, tag = "6")]
    exception_offsets: Vec<u32>,
    #[prost(message, repeated, tag = "7")]
    blocks: Vec<DpmiBlock>,
    #[prost(message, optional, tag = "8")]
    memory_stats: Option<DpmiMemoryStats>,
    #[prost(uint32, tag = "9")]
    pub(super) memory_start: u32,
    #[prost(uint32, tag = "10")]
    pub(super) memory_end: u32,
    #[prost(uint32, tag = "11")]
    next_handle: u32,
    #[prost(message, repeated, tag = "12")]
    frames: Vec<DpmiFrame>,
    #[prost(uint32, tag = "13")]
    pub(super) depth: u32,
    #[prost(uint32, tag = "14")]
    private_segment: u32,
    #[prost(bool, tag = "15")]
    bits32: bool,
    #[prost(message, repeated, tag = "16")]
    callbacks: Vec<DpmiCallback>,
    #[prost(bool, tag = "17")]
    direct_return_pending: bool,
}

fn slot<T>(table: &[Option<T>], index: u32, name: &'static str) -> Result<usize, InvalidState> {
    let index = index as usize;
    match table.get(index) {
        Some(None) => Ok(index),
        _ => Err(InvalidState(name)),
    }
}

impl State for Host {
    type Wire = DpmiHost;
    fn save(&self) -> Self::Wire {
        let mut used = vec![0u8; DESCRIPTORS / 8];
        for (index, _) in self.used.iter().enumerate().filter(|(_, used)| **used) {
            used[index / 8] |= 1 << (index % 8);
        }
        let stats = &self.stats;
        Self::Wire {
            mode: match self.mode {
                Mode::Reflect => 1,
                Mode::Direct => 2,
            },
            stats: Some(DpmiStats {
                entered: stats.entered,
                calls: stats.calls,
                direct: stats.direct,
                reflected: stats.reflected,
                unsupported: stats.unsupported,
                last_function: u32::from(stats.last_function),
                recent_functions: stats.recent_functions.map(u32::from).to_vec(),
                unsupported_functions: stats.unsupported_functions.map(u32::from).to_vec(),
                memory: Some(stats.memory.save()),
            }),
            used,
            dos_segments: self
                .dos_segments
                .iter()
                .enumerate()
                .filter(|(_, segment)| **segment != 0)
                .map(|(index, &segment)| ((index as u32) << 16) | u32::from(segment))
                .collect(),
            exception_selectors: self.exceptions.map(|(s, _)| u32::from(s)).to_vec(),
            exception_offsets: self.exceptions.map(|(_, o)| o).to_vec(),
            blocks: self
                .memory
                .blocks
                .iter()
                .enumerate()
                .filter(|(_, block)| **block != Block::default())
                .map(|(slot, block)| DpmiBlock {
                    slot: slot as u32,
                    handle: block.handle,
                    address: block.address,
                    requested: block.requested,
                    committed: block.committed,
                })
                .collect(),
            memory_stats: Some(self.memory.stats.save()),
            memory_start: self.memory.start,
            memory_end: self.memory.end,
            next_handle: self.memory.next_handle,
            frames: self
                .frames
                .iter()
                .enumerate()
                .filter_map(|(slot, frame)| {
                    let frame = frame.as_ref()?;
                    Some(DpmiFrame {
                        slot: slot as u32,
                        context: Some(frame.context.save()),
                        structure: frame.structure,
                        callback: frame.callback,
                        timer: frame.timer,
                        exception: frame.exception,
                    })
                })
                .collect(),
            depth: self.depth as u32,
            private_segment: u32::from(self.private_segment),
            bits32: self.bits32,
            callbacks: self
                .callbacks
                .iter()
                .enumerate()
                .filter_map(|(slot, callback)| {
                    let c = callback.as_ref()?;
                    Some(DpmiCallback {
                        slot: slot as u32,
                        code: u32::from(c.code),
                        ip: c.ip,
                        data: u32::from(c.data),
                        offset: c.offset,
                        structure: c.structure,
                    })
                })
                .collect(),
            direct_return_pending: self.direct_return_pending,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let mode = match w.mode {
            1 => Mode::Reflect,
            2 => Mode::Direct,
            _ => return Err(InvalidState("DPMI host mode")),
        };
        let saved = doslet_state::required(w.stats, "DpmiHost.stats")?;
        let functions = |values: Vec<u32>, name| -> Result<[u16; 16], InvalidState> {
            let values = values
                .into_iter()
                .map(|v| doslet_state::narrow(v, name))
                .collect::<Result<Vec<u16>, _>>()?;
            doslet_state::narrow(values, name)
        };
        let stats = Stats {
            entered: saved.entered,
            calls: saved.calls,
            direct: saved.direct,
            reflected: saved.reflected,
            unsupported: saved.unsupported,
            last_function: doslet_state::narrow(saved.last_function, "DpmiStats.last_function")?,
            recent_functions: functions(saved.recent_functions, "DpmiStats.recent_functions")?,
            unsupported_functions: functions(
                saved.unsupported_functions,
                "DpmiStats.unsupported_functions",
            )?,
            memory: MemoryStats::load(doslet_state::required(saved.memory, "DpmiStats.memory")?)?,
        };
        if w.used.len() != DESCRIPTORS / 8 {
            return Err(InvalidState("DpmiHost.used length"));
        }
        let used = core::array::from_fn(|i| w.used[i / 8] & (1 << (i % 8)) != 0);
        let mut dos_segments = [0u16; DESCRIPTORS];
        for entry in w.dos_segments {
            let index = (entry >> 16) as usize;
            let slot = dos_segments
                .get_mut(index)
                .filter(|segment| **segment == 0)
                .ok_or(InvalidState("DpmiHost.dos_segments"))?;
            *slot = entry as u16;
        }
        let selectors: [u32; 32] =
            doslet_state::narrow(w.exception_selectors, "DpmiHost.exception_selectors")?;
        let offsets: [u32; 32] =
            doslet_state::narrow(w.exception_offsets, "DpmiHost.exception_offsets")?;
        let mut exceptions = [(0u16, 0u32); 32];
        for (exception, (selector, offset)) in exceptions
            .iter_mut()
            .zip(selectors.into_iter().zip(offsets))
        {
            *exception = (
                doslet_state::narrow(selector, "DpmiHost.exception_selectors")?,
                offset,
            );
        }
        if w.memory_start > w.memory_end {
            return Err(InvalidState("DpmiHost.memory range"));
        }
        let mut memory = memory::Memory::new(w.memory_start, w.memory_end);
        memory.stats = MemoryStats::load(doslet_state::required(
            w.memory_stats,
            "DpmiHost.memory_stats",
        )?)?;
        memory.next_handle = w.next_handle;
        let mut filled = [false; 512];
        for block in w.blocks {
            let slot = block.slot as usize;
            if slot >= filled.len() || filled[slot] {
                return Err(InvalidState("DpmiHost.blocks slot"));
            }
            filled[slot] = true;
            let end = u64::from(block.address) + u64::from(block.committed);
            if block.address < w.memory_start || end > u64::from(w.memory_end) {
                return Err(InvalidState("DpmiHost.blocks range"));
            }
            memory.blocks[slot] = Block {
                handle: block.handle,
                address: block.address,
                requested: block.requested,
                committed: block.committed,
            };
        }
        let mut frames: [Option<Frame>; 16] = core::array::from_fn(|_| None);
        for frame in w.frames {
            let index = slot(&frames, frame.slot, "DpmiHost.frames slot")?;
            frames[index] = Some(Frame {
                context: CpuContext::load(doslet_state::required(
                    frame.context,
                    "DpmiFrame.context",
                )?)?,
                structure: frame.structure,
                callback: frame.callback,
                timer: frame.timer,
                exception: frame.exception,
            });
        }
        let depth = w.depth as usize;
        if depth > frames.len() {
            return Err(InvalidState("DpmiHost.depth"));
        }
        let mut callbacks: [Option<Callback>; 32] = [None; 32];
        for callback in w.callbacks {
            let index = slot(&callbacks, callback.slot, "DpmiHost.callbacks slot")?;
            callbacks[index] = Some(Callback {
                code: doslet_state::narrow(callback.code, "DpmiCallback.code")?,
                ip: callback.ip,
                data: doslet_state::narrow(callback.data, "DpmiCallback.data")?,
                offset: callback.offset,
                structure: callback.structure,
            });
        }
        Ok(Self {
            mode,
            stats,
            used,
            dos_segments,
            exceptions,
            memory,
            frames,
            depth,
            private_segment: doslet_state::narrow(w.private_segment, "DpmiHost.private_segment")?,
            bits32: w.bits32,
            callbacks,
            direct_return_pending: w.direct_return_pending,
        })
    }
}
