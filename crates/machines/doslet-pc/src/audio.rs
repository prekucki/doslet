use alloc::{boxed::Box, vec};

/// One preallocated owner for produced SB16 stereo frames. The host drains into
/// supplied storage; a full queue stops execution instead of discarding audio.
pub(crate) struct PcmQueue {
    frames: Box<[[i16; 2]]>,
    head: usize,
    len: usize,
}
impl Default for PcmQueue {
    fn default() -> Self {
        Self {
            frames: vec![[0; 2]; 4096].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }
}
impl PcmQueue {
    pub fn is_full(&self) -> bool {
        self.len == self.frames.len()
    }
    pub fn push(&mut self, frame: [i16; 2]) -> bool {
        if self.is_full() {
            return false;
        }
        self.frames[(self.head + self.len) % self.frames.len()] = frame;
        self.len += 1;
        true
    }
    pub fn drain(&mut self, output: &mut [[i16; 2]]) -> usize {
        let count = self.len.min(output.len());
        for frame in &mut output[..count] {
            *frame = self.frames[self.head];
            self.head = (self.head + 1) % self.frames.len();
        }
        self.len -= count;
        count
    }
}
