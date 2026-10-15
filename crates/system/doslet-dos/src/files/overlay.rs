use alloc::{boxed::Box, vec};

#[derive(Clone, Copy)]
struct Patch {
    file: usize,
    position: u32,
    offset: usize,
    len: usize,
}
#[derive(Clone, Copy)]
struct Length {
    file: usize,
    len: u32,
    base_visible: u32,
    dirty: bool,
}
pub(super) struct Overlay {
    bytes: Box<[u8]>,
    used: usize,
    patches: [Option<Patch>; 64],
    lengths: [Option<Length>; 64],
}
impl Overlay {
    pub fn space(&self) -> (usize, usize) {
        (self.bytes.len() - self.used, self.bytes.len())
    }
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: vec![0; capacity].into_boxed_slice(),
            used: 0,
            patches: [None; 64],
            lengths: [None; 64],
        }
    }
    pub fn length(&self, file: usize, original: u32) -> u32 {
        self.lengths
            .iter()
            .flatten()
            .find(|entry| entry.file == file)
            .map_or(original, |entry| entry.len)
    }
    pub fn byte(&self, file: usize, position: u32, original: &[u8]) -> u8 {
        for patch in self.patches.iter().rev().flatten() {
            if patch.file == file
                && position >= patch.position
                && u64::from(position) < u64::from(patch.position) + patch.len as u64
            {
                return self.bytes[patch.offset + (position - patch.position) as usize];
            }
        }
        let base_visible = self
            .lengths
            .iter()
            .flatten()
            .find(|entry| entry.file == file)
            .map_or(original.len() as u32, |entry| entry.base_visible);
        if position >= base_visible {
            0
        } else {
            original.get(position as usize).copied().unwrap_or(0)
        }
    }
    pub fn has_patches(&self, file: usize) -> bool {
        self.patches
            .iter()
            .flatten()
            .any(|patch| patch.file == file)
            || self
                .lengths
                .iter()
                .flatten()
                .any(|entry| entry.file == file)
    }
    pub fn reserve(
        &mut self,
        file: usize,
        position: u32,
        count: usize,
        original_len: u32,
    ) -> Result<&mut [u8], u16> {
        let end = position.checked_add(count as u32).ok_or(1u16)?;
        let length_slot = self
            .lengths
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.file == file))
            .or_else(|| self.lengths.iter().position(Option::is_none))
            .ok_or(8u16)?;
        if count == 0 {
            let base_visible = self.lengths[length_slot]
                .map_or(original_len, |entry| entry.base_visible)
                .min(position);
            for patch in self
                .patches
                .iter_mut()
                .flatten()
                .filter(|patch| patch.file == file)
            {
                patch.len = patch
                    .len
                    .min(position.saturating_sub(patch.position) as usize);
            }
            self.lengths[length_slot] = Some(Length {
                file,
                len: position,
                base_visible,
                dirty: true,
            });
            return Ok(&mut self.bytes[..0]);
        }
        if count > self.bytes.len() - self.used {
            return Err(8);
        }
        let slot = self.patches.iter().position(Option::is_none).ok_or(8u16)?;
        let offset = self.used;
        self.patches[slot] = Some(Patch {
            file,
            position,
            offset,
            len: count,
        });
        let previous = self.length(file, original_len);
        let base_visible =
            self.lengths[length_slot].map_or(original_len, |entry| entry.base_visible);
        self.lengths[length_slot] = Some(Length {
            file,
            len: previous.max(end),
            base_visible,
            dirty: true,
        });
        self.used += count;
        Ok(&mut self.bytes[offset..offset + count])
    }
    pub fn dirty(&self, file: usize) -> bool {
        self.lengths
            .iter()
            .flatten()
            .any(|entry| entry.file == file && entry.dirty)
    }
    pub fn dirty_files(&self) -> impl Iterator<Item = usize> + '_ {
        self.lengths
            .iter()
            .flatten()
            .filter(|entry| entry.dirty)
            .map(|entry| entry.file)
    }
    pub fn acknowledge(&mut self, file: usize) {
        for entry in self
            .lengths
            .iter_mut()
            .flatten()
            .filter(|entry| entry.file == file)
        {
            entry.dirty = false;
        }
    }
}
