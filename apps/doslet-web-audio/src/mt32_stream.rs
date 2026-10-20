//! Bounded timestamped CM-32L transport for the browser audio owner.
use doslet_mt32::Timeline;

const CAPACITY: usize = 16_384;

#[derive(Clone, Copy, Default)]
struct Byte {
    frame: u64,
    value: u8,
}

pub struct Mt32Stream {
    synth: Timeline,
    control: Box<[u8]>,
    pcm: Box<[u8]>,
    gm: bool,
    queue: Box<[Byte]>,
    head: usize,
    len: usize,
    cursor: u64,
    committed: u64,
    underruns: u64,
    rate: u32,
}

impl Mt32Stream {
    pub fn new(control: &[u8], pcm: &[u8], rate: u32, gm: bool) -> Result<Self, String> {
        let synth = Timeline::with_gm(control, pcm, rate, gm).map_err(|e| e.to_string())?;
        Ok(Self {
            synth,
            control: control.into(),
            pcm: pcm.into(),
            gm,
            queue: vec![Byte::default(); CAPACITY].into_boxed_slice(),
            head: 0,
            len: 0,
            cursor: 0,
            committed: 0,
            underruns: 0,
            rate,
        })
    }
    fn frame(&self, nanos: u64) -> u64 {
        (u128::from(nanos) * u128::from(self.rate) / 1_000_000_000) as u64
    }
    fn nanos_at(&self, frame: u64) -> u64 {
        // Ceil makes Timeline::render_until reach exactly this output frame.
        ((u128::from(frame) * 1_000_000_000 + u128::from(self.rate) - 1) / u128::from(self.rate))
            as u64
    }
    pub fn reset(&mut self, nanos: u64) {
        self.synth = Timeline::with_gm(&self.control, &self.pcm, self.rate, self.gm)
            .expect("ROM pair was validated at setup");
        self.cursor = self.frame(nanos);
        self.synth.rebase(self.nanos_at(self.cursor));
        self.committed = self.cursor;
        self.head = 0;
        self.len = 0;
        self.underruns = 0;
    }
    pub fn receive(&mut self, nanos: u64, bytes: &[u8]) -> Result<(), ()> {
        let frame = self.frame(nanos);
        if frame < self.committed || bytes.len() > CAPACITY - self.len {
            return Err(());
        }
        for &value in bytes {
            self.queue[(self.head + self.len) % CAPACITY] = Byte { frame, value };
            self.len += 1;
        }
        self.committed = frame;
        Ok(())
    }
    pub fn render(&mut self, output: &mut [[f32; 2]]) -> Result<usize, ()> {
        output.fill([0.0; 2]);
        let mut written = 0;
        while written < output.len() {
            while self.len > 0 && self.queue[self.head].frame <= self.cursor {
                self.synth
                    .push_byte(self.queue[self.head].value)
                    .map_err(|_| ())?;
                self.head = (self.head + 1) % CAPACITY;
                self.len -= 1;
            }
            let end = if self.len > 0 {
                self.queue[self.head].frame
            } else {
                self.committed
            };
            let count = (end.saturating_sub(self.cursor))
                .min((output.len() - written) as u64)
                .min(256) as usize;
            if count == 0 {
                break;
            }
            let rendered = self
                .synth
                .render_until(
                    self.nanos_at(self.cursor + count as u64),
                    &mut output[written..written + count],
                )
                .map_err(|_| ())?;
            if rendered != count {
                return Err(());
            }
            self.cursor += count as u64;
            written += count;
        }
        if written < output.len() {
            self.underruns += 1;
        }
        Ok(written)
    }
    pub fn nanos(&self) -> u64 {
        (u128::from(self.cursor) * 1_000_000_000 / u128::from(self.rate)) as u64
    }
    pub fn notes(&self) -> u64 {
        self.synth.note_events()
    }
    pub fn underruns(&self) -> u64 {
        self.underruns
    }
}
