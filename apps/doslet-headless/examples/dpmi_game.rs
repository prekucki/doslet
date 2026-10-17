//! Bounded local-game DPMI experiment. Guest writes stay in a RAM overlay.
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus, session::dpmi::Mode};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const BASIS: u64 = 0xcbf29ce484222325;
fn hash_into(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash = (*hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
}
fn hash(bytes: &[u8]) -> u64 {
    let mut h = BASIS;
    hash_into(&mut h, bytes);
    h
}

struct Run {
    session: Session,
    midi: u64,
    pcm: u64,
    midi_count: u64,
    pcm_count: u64,
    keys: Vec<u8>,
    key_index: usize,
    jit: bool,
}
impl Run {
    fn new(args: &[String], mode: Option<Mode>) -> Result<Self> {
        let jit = args.get(5).is_some_and(|s| s == "jit");
        let settings = native_input::LaunchSettings {
            no_dynamic_core: !jit,
            dpmi: mode,
            cdrom: args
                .get(3)
                .filter(|s| s.as_str() != "-")
                .map(|p| (b'D', p.into())),
            ..Default::default()
        };
        let mut session = native_input::load_game_with_settings(Path::new(&args[0]), &settings)?;
        if jit && !session.dynamic_core_enabled() {
            return Err("JIT unavailable in this build".into());
        }
        session.start();
        Ok(Self {
            session,
            midi: BASIS,
            pcm: BASIS,
            midi_count: 0,
            pcm_count: 0,
            keys: args.get(4).map_or("", String::as_str).as_bytes().to_vec(),
            key_index: 0,
            jit,
        })
    }
    fn advance(&mut self, budget: u32) -> Result<()> {
        if self.jit {
            self.session.prepare_dynamic(4);
        }
        self.session.advance(budget);
        let mut midi = [0; 4096];
        loop {
            let count = self.session.drain_midi(&mut midi);
            if count == 0 {
                break;
            }
            hash_into(&mut self.midi, &midi[..count]);
            self.midi_count += count as u64;
        }
        let mut pcm = [[0; 2]; 4096];
        loop {
            let count = self.session.drain_pcm(&mut pcm);
            if count == 0 {
                break;
            }
            for sample in pcm[..count].as_flattened() {
                hash_into(&mut self.pcm, &sample.to_le_bytes());
            }
            self.pcm_count += count as u64;
        }
        if self.session.waiting_for_key() {
            let p = self.session.ram_view(0x41A..0x41E).unwrap();
            if p[..2] == p[2..]
                && let Some(&key) = self.keys.get(self.key_index)
            {
                let ascii = if key == b'|' { 13 } else { key };
                self.session
                    .push_key(if ascii == 13 { 0x1C } else { 0 }, ascii)?;
                self.key_index += 1;
            }
        }
        if let Some(fault) = self.session.fault() {
            return Err(fault.to_string().into());
        }
        Ok(())
    }
    fn run(&mut self, steps: u64) -> Result<Duration> {
        let start = Instant::now();
        while self.session.steps() < steps && self.session.status() == SessionStatus::Running {
            self.advance((steps - self.session.steps()).min(1000) as u32)?;
        }
        Ok(start.elapsed())
    }
    fn report(&self, mode: &str, elapsed: Duration) {
        let s = &self.session;
        println!(
            "mode={mode} jit={} elapsed={elapsed:?} steps={} guest_ns={} status={:?} video={:?}",
            self.jit,
            s.steps(),
            s.emulated_nanos(),
            s.status(),
            s.video_mode()
        );
        println!("dpmi={:?}", s.dpmi_stats());
        println!(
            "pc={:04x}:{:08x} regs={:?}",
            s.registers().cs,
            s.registers().ip,
            s.registers()
        );
        println!("console={}", String::from_utf8_lossy(s.console()));
        println!(
            "vram={:016x} midi={:016x}/{} pcm={:016x}/{}",
            hash(s.vram_view()),
            self.midi,
            self.midi_count,
            self.pcm,
            self.pcm_count
        );
        if let Some(frame) = s.frame() {
            println!(
                "pixels={:016x} palette={:016x}",
                hash(frame.pixels),
                hash(frame.palette.as_flattened())
            );
        }
        for block in s.dpmi_blocks() {
            println!("block={block:?}");
        }
        if self.jit {
            println!("dynamic={:?}", s.dynamic_stats());
        }
    }
}

fn same(a: &Run, b: &Run) {
    let (a_s, b_s) = (&a.session, &b.session);
    assert_eq!(a_s.steps(), b_s.steps());
    assert_eq!(a_s.emulated_nanos(), b_s.emulated_nanos());
    assert_eq!(a_s.status(), b_s.status());
    assert_eq!(a_s.registers(), b_s.registers());
    assert_eq!(a_s.eflags(), b_s.eflags());
    assert_eq!(a_s.control_registers(), b_s.control_registers());
    assert_eq!(a_s.debug_x87(), b_s.debug_x87());
    assert_eq!(a_s.a20_enabled(), b_s.a20_enabled());
    // Exposed RAM, excluding the separate VGA aperture. Report only the first
    // difference instead of formatting megabytes on failure.
    for range in [0..0xA0000, 0xB0000..0x1000000] {
        let left = a_s.ram_view(range.clone()).unwrap();
        let right = b_s.ram_view(range.clone()).unwrap();
        if let Some(i) = left.iter().zip(right).position(|(a, b)| a != b) {
            panic!(
                "RAM mismatch at {:#x}, step {}: {:02x} != {:02x}",
                range.start + i,
                a_s.steps(),
                left[i],
                right[i]
            );
        }
    }
    assert_eq!(a_s.vram_view(), b_s.vram_view());
    assert_eq!(a_s.video_mode(), b_s.video_mode());
    match (a_s.frame(), b_s.frame()) {
        (Some(a), Some(b)) => {
            assert_eq!(
                (a.width, a.height, a.revision),
                (b.width, b.height, b.revision)
            );
            assert_eq!(a.pixels, b.pixels);
            assert_eq!(a.palette, b.palette);
        }
        (None, None) => {}
        _ => panic!("scanout availability differs"),
    }
    assert_eq!(a_s.midi_stats(), b_s.midi_stats());
    assert_eq!(a_s.file_access_stats(), b_s.file_access_stats());
    #[cfg(feature = "profiling")]
    assert_eq!(a_s.execution_stats(), b_s.execution_stats());
    assert_eq!(
        (a.midi, a.pcm, a.midi_count, a.pcm_count),
        (b.midi, b.pcm, b.midi_count, b.pcm_count)
    );
    assert_eq!(a_s.console(), b_s.console());
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err(
            "usage: dpmi_game EXE STEPS off|reflect|direct|compare|bench [ISO|-] [KEYS] [jit]"
                .into(),
        );
    }
    let steps: u64 = args[1].parse()?;
    match args[2].as_str() {
        "compare" => {
            let mut a = Run::new(&args, Some(Mode::Reflect))?;
            let mut b = Run::new(&args, Some(Mode::Direct))?;
            let start = Instant::now();
            let mut checkpoints = 0;
            while a.session.steps() < steps && a.session.status() == SessionStatus::Running {
                let budget = (steps - a.session.steps()).min(1000) as u32;
                a.advance(budget)?;
                b.advance(budget)?;
                // The replaced real IRET has a different intermediate context.
                if a.session.steps().is_multiple_of(1_000_000)
                    && a.session.registers() == b.session.registers()
                {
                    same(&a, &b);
                    checkpoints += 1;
                }
            }
            for _ in 0..2 {
                if a.session.registers() == b.session.registers() {
                    break;
                }
                a.advance(1)?;
                b.advance(1)?;
            }
            same(&a, &b);
            b.report("compare", start.elapsed());
            println!(
                "Matched registers/control/flags, RAM, VRAM, pixels/palette, guest time, MIDI and PCM at {checkpoints} checkpoints and final state."
            );
        }
        "bench" => {
            let mut times = [[Duration::ZERO; 3]; 2];
            // Loading excluded; warm both paths, then three alternating pairs.
            for round in 0..4 {
                let mut runs = [None, None];
                for position in 0..2 {
                    let i = (round + position) % 2;
                    let mode = [Mode::Reflect, Mode::Direct][i];
                    let mut run = Run::new(&args, Some(mode))?;
                    let elapsed = run.run(steps)?;
                    if round > 0 {
                        times[i][round - 1] = elapsed;
                        println!("round={round} mode={mode:?} elapsed={elapsed:?}");
                    }
                    runs[i] = Some(run);
                }
                same(runs[0].as_ref().unwrap(), runs[1].as_ref().unwrap());
            }
            times[0].sort();
            times[1].sort();
            println!(
                "median reflect={:?} direct={:?} direct/reflect={:.4}; jit={} (JIT timing includes synchronous compilation)",
                times[0][1],
                times[1][1],
                times[1][1].as_secs_f64() / times[0][1].as_secs_f64(),
                args.get(5).is_some_and(|s| s == "jit")
            );
        }
        mode => {
            let setting = match mode {
                "off" => None,
                "reflect" => Some(Mode::Reflect),
                "direct" => Some(Mode::Direct),
                _ => return Err("invalid mode".into()),
            };
            let mut run = Run::new(&args, setting)?;
            let elapsed = run.run(steps)?;
            run.report(mode, elapsed);
        }
    }
    Ok(())
}
