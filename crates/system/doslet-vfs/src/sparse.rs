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
#[derive(Clone)]
pub struct OverlayFs {
    bytes: Box<[u8]>,
    used: usize,
    patches: Box<[Option<Patch>]>,
    lengths: [Option<Length>; 64],
    staging: Box<[u8]>,
    pending: Option<(usize, u32, usize)>,
    chunked: bool,
}
impl OverlayFs {
    pub fn space(&self) -> (usize, usize) {
        (self.bytes.len() - self.used, self.bytes.len())
    }
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: vec![0; capacity].into_boxed_slice(),
            used: 0,
            patches: vec![None; (capacity / 4096).max(64)].into_boxed_slice(),
            lengths: [None; 64],
            staging: vec![0; 65536].into_boxed_slice(),
            pending: None,
            chunked: capacity >= 4096,
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
        if let Some((pending_file, start, len)) = self.pending
            && pending_file == file
            && position >= start
            && u64::from(position) < u64::from(start) + len as u64
        {
            return self.staging[(position - start) as usize];
        }
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
    fn reserve_extents(
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

impl OverlayFs {
    pub fn snapshot_capacity(&self) -> usize {
        24 + self.bytes.len() + self.patches.len() * 40 + 64 * 24
    }
    pub fn encode(&mut self, out: &mut alloc::vec::Vec<u8>) {
        self.finish_pending();
        out.extend_from_slice(&(self.used as u64).to_le_bytes());
        out.extend_from_slice(&(self.patches.iter().flatten().count() as u32).to_le_bytes());
        out.extend_from_slice(&(self.lengths.iter().flatten().count() as u32).to_le_bytes());
        out.extend_from_slice(&self.bytes[..self.used]);
        for patch in self.patches.iter().flatten() {
            for value in [
                patch.file as u64,
                u64::from(patch.position),
                patch.offset as u64,
                patch.len as u64,
            ] {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        for length in self.lengths.iter().flatten() {
            out.extend_from_slice(&(length.file as u64).to_le_bytes());
            out.extend_from_slice(&length.len.to_le_bytes());
            out.extend_from_slice(&length.base_visible.to_le_bytes());
        }
    }
    pub fn decode(&mut self, input: &mut &[u8], mapping: &[usize]) -> Result<(), u16> {
        fn number(input: &mut &[u8], count: usize) -> Result<u64, u16> {
            let raw = input.get(..count).ok_or(13u16)?;
            let mut value = [0; 8];
            value[..count].copy_from_slice(raw);
            *input = &input[count..];
            Ok(u64::from_le_bytes(value))
        }
        let used = usize::try_from(number(input, 8)?).map_err(|_| 13u16)?;
        let patches = number(input, 4)? as usize;
        let lengths = number(input, 4)? as usize;
        if used > self.bytes.len() || patches > self.patches.len() || lengths > 64 {
            return Err(8);
        }
        let bytes = input.get(..used).ok_or(13u16)?;
        *input = &input[used..];
        let mut new_patches = vec![None; self.patches.len()].into_boxed_slice();
        let mut new_lengths = [None; 64];
        for slot in new_patches.iter_mut().take(patches) {
            let file = *mapping
                .get(usize::try_from(number(input, 8)?).map_err(|_| 13u16)?)
                .ok_or(13u16)?;
            let position = u32::try_from(number(input, 8)?).map_err(|_| 13u16)?;
            let offset = usize::try_from(number(input, 8)?).map_err(|_| 13u16)?;
            let len = usize::try_from(number(input, 8)?).map_err(|_| 13u16)?;
            if offset.checked_add(len).is_none_or(|end| end > used)
                || u64::from(position) + len as u64 > u64::from(u32::MAX)
            {
                return Err(13);
            }
            *slot = Some(Patch {
                file,
                position,
                offset,
                len,
            });
        }
        for slot in new_lengths.iter_mut().take(lengths) {
            let file = *mapping
                .get(usize::try_from(number(input, 8)?).map_err(|_| 13u16)?)
                .ok_or(13u16)?;
            let len = number(input, 4)? as u32;
            let base_visible = number(input, 4)? as u32;
            *slot = Some(Length {
                file,
                len,
                base_visible,
                dirty: false,
            });
        }
        self.chunked = self.bytes.len() >= 4096
            && new_patches
                .iter()
                .flatten()
                .all(|patch| patch.position % 4096 == 0 && patch.len == 4096);
        self.bytes[..used].copy_from_slice(bytes);
        self.used = used;
        self.patches = new_patches;
        self.lengths = new_lengths;
        Ok(())
    }
    pub fn acknowledge_all(&mut self) {
        for entry in self.lengths.iter_mut().flatten() {
            entry.dirty = false;
        }
    }
    pub fn any_dirty(&self) -> bool {
        self.lengths.iter().flatten().any(|entry| entry.dirty)
    }
}

impl OverlayFs {
    pub fn reserve(
        &mut self,
        file: usize,
        position: u32,
        count: usize,
        original: &[u8],
    ) -> Result<&mut [u8], u16> {
        if !self.chunked {
            return self.reserve_extents(file, position, count, original.len() as u32);
        }
        self.finish_pending();
        let end = position
            .checked_add(u32::try_from(count).map_err(|_| 1u16)?)
            .ok_or(1u16)?;
        let length_slot = self
            .lengths
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.file == file))
            .or_else(|| self.lengths.iter().position(Option::is_none))
            .ok_or(8u16)?;
        if count == 0 {
            let previous = self.lengths[length_slot];
            let base_visible = previous
                .map_or(original.len() as u32, |entry| entry.base_visible)
                .min(position);
            for patch in self
                .patches
                .iter()
                .flatten()
                .filter(|patch| patch.file == file)
            {
                let start = patch.position;
                let within = position.saturating_sub(start) as usize;
                if within < patch.len {
                    self.bytes[patch.offset + within..patch.offset + patch.len].fill(0);
                }
            }
            self.lengths[length_slot] = Some(Length {
                file,
                len: position,
                base_visible,
                dirty: true,
            });
            return Ok(&mut self.staging[..0]);
        }
        if count > self.staging.len() {
            return Err(8);
        }
        let first = position / 4096;
        let last = (end - 1) / 4096;
        let needed = (first..=last)
            .filter(|chunk| {
                !self
                    .patches
                    .iter()
                    .flatten()
                    .any(|patch| patch.file == file && patch.position == chunk * 4096)
            })
            .count();
        if needed > self.patches.iter().filter(|patch| patch.is_none()).count()
            || needed * 4096 > self.bytes.len() - self.used
        {
            return Err(8);
        }
        for chunk in first..=last {
            let start = chunk * 4096;
            if self
                .patches
                .iter()
                .flatten()
                .any(|patch| patch.file == file && patch.position == start)
            {
                continue;
            }
            let offset = self.used;
            for within in 0..4096 {
                self.bytes[offset + within] = self.byte(file, start + within as u32, original);
            }
            let slot = self.patches.iter().position(Option::is_none).unwrap();
            self.patches[slot] = Some(Patch {
                file,
                position: start,
                offset,
                len: 4096,
            });
            self.used += 4096;
        }
        let previous = self.length(file, original.len() as u32);
        let base_visible =
            self.lengths[length_slot].map_or(original.len() as u32, |entry| entry.base_visible);
        self.lengths[length_slot] = Some(Length {
            file,
            len: previous.max(end),
            base_visible,
            dirty: true,
        });
        self.staging[..count].fill(0);
        self.pending = Some((file, position, count));
        Ok(&mut self.staging[..count])
    }
    pub fn finish_pending(&mut self) {
        let Some((file, start, len)) = self.pending.take() else {
            return;
        };
        let mut done = 0;
        while done < len {
            let position = start + done as u32;
            let chunk = position / 4096 * 4096;
            let patch = self
                .patches
                .iter()
                .flatten()
                .find(|patch| patch.file == file && patch.position == chunk)
                .unwrap();
            let within = (position - chunk) as usize;
            let count = (len - done).min(4096 - within);
            self.bytes[patch.offset + within..patch.offset + within + count]
                .copy_from_slice(&self.staging[done..done + count]);
            done += count;
        }
    }
}
impl OverlayFs {
    /// Fill a previously reserved write in bounded windows. This preserves the
    /// contiguous write-buffer adapter without retaining its borrow across waits.
    pub fn write_window(&mut self, file: usize, offset: usize, input: &[u8]) -> Result<(), u16> {
        if self.chunked {
            let Some((pending_file, _, length)) = self.pending else {
                return Err(1);
            };
            if pending_file != file
                || offset
                    .checked_add(input.len())
                    .is_none_or(|end| end > length)
            {
                return Err(1);
            }
            self.staging[offset..offset + input.len()].copy_from_slice(input);
        } else {
            let patch = self
                .patches
                .iter()
                .rev()
                .flatten()
                .find(|patch| patch.file == file)
                .ok_or(1u16)?;
            if offset
                .checked_add(input.len())
                .is_none_or(|end| end > patch.len)
            {
                return Err(1);
            }
            self.bytes[patch.offset + offset..patch.offset + offset + input.len()]
                .copy_from_slice(input);
        }
        Ok(())
    }
}

impl OverlayFs {
    pub fn read_into(&self, file: usize, position: u32, original: &[u8], output: &mut [u8]) {
        output.fill(0);
        let visible = self
            .lengths
            .iter()
            .flatten()
            .find(|entry| entry.file == file)
            .map_or(original.len() as u32, |entry| entry.base_visible);
        let lower_len = output
            .len()
            .min(visible.saturating_sub(position) as usize)
            .min(original.len().saturating_sub(position as usize));
        if lower_len > 0 {
            output[..lower_len]
                .copy_from_slice(&original[position as usize..position as usize + lower_len]);
        }
        let start = u64::from(position);
        let end = start + output.len() as u64;
        for patch in self
            .patches
            .iter()
            .flatten()
            .filter(|patch| patch.file == file)
        {
            let lo = start.max(u64::from(patch.position));
            let hi = end.min(u64::from(patch.position) + patch.len as u64);
            if lo < hi {
                output[(lo - start) as usize..(hi - start) as usize].copy_from_slice(
                    &self.bytes[patch.offset + (lo - u64::from(patch.position)) as usize
                        ..patch.offset + (hi - u64::from(patch.position)) as usize],
                );
            }
        }
        if let Some((pending_file, position, length)) = self.pending
            && pending_file == file
        {
            let lo = start.max(u64::from(position));
            let hi = end.min(u64::from(position) + length as u64);
            if lo < hi {
                output[(lo - start) as usize..(hi - start) as usize].copy_from_slice(
                    &self.staging
                        [(lo - u64::from(position)) as usize..(hi - u64::from(position)) as usize],
                );
            }
        }
    }
}

#[cfg(feature = "save-state")]
#[path = "sparse_saved.rs"]
mod saved_state;
