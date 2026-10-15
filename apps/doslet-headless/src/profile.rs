//! Host-only observations, outside the measured Session::advance boundary.
use doslet_runtime::Session;
use doslet_x86::{Access, SegmentRegister};
use std::{collections::BTreeMap, time::Duration};
type Key = (u16, u32, u32);
pub struct Profile {
    pub cpu: Duration,
    pub scanout: Duration,
    hot: BTreeMap<Key, (u64, [u8; 16])>,
    seed: u32,
    samples: u64,
    pixels: Vec<u8>,
    revision: Option<u64>,
    next_frame: u64,
    conversions: u64,
}
impl Profile {
    pub fn new() -> Self {
        Self {
            cpu: Duration::ZERO,
            scanout: Duration::ZERO,
            hot: BTreeMap::new(),
            seed: 0x12345678,
            samples: 0,
            pixels: Vec::new(),
            revision: None,
            next_frame: 0,
            conversions: 0,
        }
    }
    pub fn sample(&mut self, session: &Session) -> u32 {
        let r = session.registers();
        let cache = r.segment_cache(SegmentRegister::Cs);
        let length = cache.limit.saturating_sub(r.ip).saturating_add(1).min(16) as u8;
        if let Ok(mut address) = r.address(SegmentRegister::Cs, r.ip, length, Access::Execute) {
            if !session.a20_enabled() {
                address &= !(1 << 20);
            }
            if let Some(data) =
                session.ram_view(address as usize..address as usize + usize::from(length))
            {
                let key = (r.cs, r.ip, address);
                // Sampling storage is bounded and entirely host-owned.
                if self.hot.len() < 16384 || self.hot.contains_key(&key) {
                    let entry = self.hot.entry(key).or_insert((0, [0; 16]));
                    entry.0 += 1;
                    entry.1[..data.len()].copy_from_slice(data);
                    self.samples += 1;
                }
            }
        }
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        // Vary the interval to avoid aliasing against a deterministic guest loop.
        1000 + self.seed % 9001
    }
    pub fn frame(&mut self, session: &Session) {
        if session.emulated_nanos() < self.next_frame {
            return;
        }
        self.next_frame = session.emulated_nanos() + 16_666_667;
        let Some(frame) = session.frame() else {
            return;
        };
        if self.revision == Some(frame.revision) {
            return;
        }
        self.pixels.resize(frame.width * frame.height * 4, 0);
        let start = std::time::Instant::now();
        frame.write_rgba(&mut self.pixels).unwrap();
        self.scanout += start.elapsed();
        self.revision = Some(frame.revision);
        self.conversions += 1;
    }
    pub fn print(&self, session: &Session, elapsed: Duration) {
        let stats = session.execution_stats();
        println!(
            "CPU profile (instrumented native run; sample/host overhead excluded from CPU boundary)"
        );
        println!(
            "Session execution: {:.3} s; RGBA conversion: {:.3} s / {} changed frames; total observed: {:.3} s",
            self.cpu.as_secs_f64(),
            self.scanout.as_secs_f64(),
            self.conversions,
            elapsed.as_secs_f64()
        );
        println!("Exact execution counters: {stats:?}");
        println!("Decode cache counters: {:?}", session.decode_cache_stats());
        println!(
            "Native dynamic core counters: {:?}",
            session.dynamic_stats()
        );
        let mut entries: Vec<_> = self.hot.iter().collect();
        entries.sort_unstable_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
        println!(
            "Hot guest PCs: {} randomized samples (frequency estimates, not instruction counts)",
            self.samples
        );
        for ((cs, ip, linear), (count, bytes)) in entries.into_iter().take(16) {
            println!(
                "{cs:04X}:{ip:08X} linear={linear:08X} samples={count} ({:.2}%) bytes={bytes:02X?}",
                *count as f64 * 100.0 / self.samples as f64
            );
        }
    }
}
