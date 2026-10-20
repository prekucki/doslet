use alloc::{boxed::Box, vec};
use doslet_pcm::{EMPTY, Frame};

/// Optional host taps; not emulated device state and not part of save states.
/// The normal mixed output continues unchanged for deterministic consumers.
pub(crate) struct SourceQueue {
    frames: Box<[doslet_pcm::SourceFrame]>,
    head: usize,
    len: usize,
}
impl Default for SourceQueue {
    fn default() -> Self {
        Self {
            frames: vec![doslet_pcm::SourceFrame::default(); 4096].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }
}
impl SourceQueue {
    pub fn is_full(&self) -> bool {
        self.len == self.frames.len()
    }
    pub fn push(&mut self, frame: doslet_pcm::SourceFrame) -> bool {
        if self.is_full() {
            return false;
        }
        self.frames[(self.head + self.len) % self.frames.len()] = frame;
        self.len += 1;
        true
    }
    pub fn drain(&mut self, output: &mut [doslet_pcm::SourceFrame]) -> usize {
        let count = output.len().min(self.len);
        for frame in &mut output[..count] {
            *frame = self.frames[self.head];
            self.head = (self.head + 1) % self.frames.len();
        }
        self.len -= count;
        count
    }
}

/// One preallocated owner for mixed SB16 DMA/FM stereo frames. The host drains into
/// supplied storage; a full queue stops execution instead of discarding audio.
pub(crate) struct PcmQueue {
    frames: Box<[Frame]>,
    head: usize,
    len: usize,
    generated: u64,
    nonzero: u64,
}
impl Default for PcmQueue {
    fn default() -> Self {
        Self {
            frames: vec![EMPTY; 4096].into_boxed_slice(),
            head: 0,
            len: 0,
            generated: 0,
            nonzero: 0,
        }
    }
}
impl PcmQueue {
    pub fn is_full(&self) -> bool {
        self.len == self.frames.len()
    }
    pub fn push(&mut self, frame: Frame) -> bool {
        if self.is_full() {
            return false;
        }
        self.frames[(self.head + self.len) % self.frames.len()] = frame;
        self.len += 1;
        self.generated += 1;
        self.nonzero += u64::from(frame.samples != [0; 2]);
        true
    }
    pub fn drain(&mut self, output: &mut [[i16; 2]]) -> usize {
        let count = self.len.min(output.len());
        for frame in &mut output[..count] {
            *frame = self.frames[self.head].samples;
            self.head = (self.head + 1) % self.frames.len();
        }
        self.len -= count;
        count
    }
    pub fn drain_timed(&mut self, output: &mut [Frame]) -> usize {
        let count = self.len.min(output.len());
        for frame in &mut output[..count] {
            *frame = self.frames[self.head];
            self.head = (self.head + 1) % self.frames.len();
        }
        self.len -= count;
        count
    }
    pub fn stats(&self) -> (u64, u64) {
        (self.generated, self.nonzero)
    }
}

#[cfg(feature = "save-state")]
#[path = "audio_saved.rs"]
mod saved_state;
