//! Optional, bounded offline probes. Addresses and memory ranges come from data.
use doslet_runtime::Session;
use serde_json::{Value, json};
use std::{error::Error, fs, path::Path};

pub struct Probes {
    pcs: Vec<u32>,
    frame_pcs: Vec<u32>,
    memory: Vec<(u32, usize)>,
    dma_blocks: u64,
    events: Vec<Value>,
}

impl Probes {
    pub fn load(path: &Path, s: &Session) -> Result<Self, Box<dyn Error>> {
        let config: Value = serde_json::from_slice(&fs::read(path)?)?;
        let number = |v: &Value| -> Result<u32, Box<dyn Error>> {
            Ok(u32::try_from(
                v.as_u64().ok_or("expected unsigned integer")?,
            )?)
        };
        let mut pcs = Vec::new();
        for p in config["pcs"].as_array().ok_or("missing pcs")? {
            pcs.push(number(p)?);
        }
        pcs.sort_unstable();
        pcs.dedup();
        if pcs.len() > 128 {
            return Err("probe limit: 128 PCs".into());
        }
        let frame_pcs = config
            .get("frame_pcs")
            .map(|v| {
                v.as_array()
                    .ok_or("frame_pcs must be an array")?
                    .iter()
                    .map(number)
                    .collect::<Result<Vec<_>, Box<dyn Error>>>()
            })
            .transpose()?
            .unwrap_or_default();
        if frame_pcs.len() > 128 || frame_pcs.iter().any(|pc| !pcs.contains(pc)) {
            return Err("frame_pcs must be a subset of pcs".into());
        }
        let mut memory = Vec::new();
        for m in config["memory"].as_array().ok_or("missing memory")? {
            let address = number(&m[0])?;
            let length = number(&m[1])? as usize;
            if length == 0 || length > 128 {
                return Err("probe memory length must be 1..128".into());
            }
            let mut bytes = vec![0; length];
            if !s.debug_read_memory(address, &mut bytes) {
                return Err("probe memory outside RAM".into());
            }
            memory.push((address, length));
        }
        if memory.len() > 8 {
            return Err("probe limit: eight memory ranges".into());
        }
        Ok(Self {
            pcs,
            frame_pcs,
            memory,
            dma_blocks: s.audio_status().dsp.block_irq_deadlines,
            events: Vec::new(),
        })
    }

    pub fn after(
        &mut self,
        s: &Session,
        pc: u32,
        registers: [u32; 8],
        before_ns: u64,
        irq_entry: bool,
    ) -> Result<(), Box<dyn Error>> {
        self.record(s, pc, registers, before_ns, irq_entry, true)
    }

    pub fn hardware_after(&mut self, s: &Session, before_ns: u64) -> Result<(), Box<dyn Error>> {
        self.record(s, s.debug_pc(), [0; 8], before_ns, false, false)
    }

    fn record(
        &mut self,
        s: &Session,
        pc: u32,
        registers: [u32; 8],
        before_ns: u64,
        irq_entry: bool,
        executed: bool,
    ) -> Result<(), Box<dyn Error>> {
        let blocks = s.audio_status().dsp.block_irq_deadlines;
        let block_change = blocks != self.dma_blocks;
        let probe = executed && !irq_entry && self.pcs.binary_search(&pc).is_ok();
        if !block_change && !probe && !irq_entry {
            return Ok(());
        }
        if self.events.len() >= 100_000 {
            return Err("probe event bound exceeded".into());
        }
        let mut memory = Vec::new();
        for &(address, length) in &self.memory {
            let mut bytes = vec![0; length];
            if !s.debug_read_memory(address, &mut bytes) {
                return Err("probe memory became unreadable".into());
            }
            memory.push(bytes);
        }
        let mut event = json!({"before_ns":before_ns,"after_ns":s.emulated_nanos(),
            "pc":pc,"next":s.debug_pc(),"registers":registers,
            "irq_entry":irq_entry,"probe":probe,"dma_block":block_change,
            "dma_blocks":blocks,"memory_after":memory});
        if probe && self.frame_pcs.contains(&pc) {
            let frame = s.presentation_frame();
            let mut rgba = vec![0; frame.width() * frame.height() * 4];
            frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
            let hash = rgba.iter().fold(0xcbf29ce484222325_u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
            });
            event["frame"] = json!({"width":frame.width(),"height":frame.height(),
                "rgba_fnv64":format!("{hash:016x}")});
        }
        self.events.push(event);
        self.dma_blocks = blocks;
        Ok(())
    }

    pub fn write(&self, path: &Path, start_ns: u64) -> Result<(), Box<dyn Error>> {
        fs::write(
            path,
            serde_json::to_vec(&json!({"start_ns":start_ns,
            "pcs":self.pcs,"memory_ranges":self.memory,
            "register_order":["eax","ecx","edx","ebx","esp","ebp","esi","edi"],
            "events":self.events}))?,
        )?;
        Ok(())
    }
}
