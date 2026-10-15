//! DOS 8.3 find-first/find-next state lives in the caller's 21-byte DTA prefix.
use super::{DirectoryTree, Name};
#[derive(Clone, Copy)]
pub struct Search {
    directory: Name,
    pattern: [u8; 11],
    attributes: u8,
    next: u32,
    parent: u16,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: [u8; 13],
    pub attributes: u8,
    pub length: u32,
}
impl Search {
    pub fn state(&self) -> [u8; 21] {
        let mut state = [0; 21];
        state[0] = 3;
        state[1] = self.attributes;
        state[2..4].copy_from_slice(&self.parent.to_le_bytes());
        state[4] = self.directory.len as u8;
        state[5..16].copy_from_slice(&self.pattern);
        state[16..20].copy_from_slice(&self.next.to_le_bytes());
        state[20] = 0xD5;
        state
    }
}
impl DirectoryTree {
    pub fn start_search(&self, path: &[u8], attributes: u16) -> Result<Search, u16> {
        let path = if path.get(1) == Some(&b':') {
            if !path[0].eq_ignore_ascii_case(&b'C') {
                return Err(15);
            }
            &path[2..]
        } else {
            path
        };
        let (parent, mask) = match path.iter().rposition(|b| matches!(b, b'\\' | b'/')) {
            Some(i) => (&path[..=i], &path[i + 1..]),
            None => (&b""[..], path),
        };
        let directory = Name::relative(parent, self.current)?;
        if self.name_attributes(directory) != Ok(0x10) {
            return Err(3);
        }
        let parent = if directory.len == 0 {
            u16::MAX
        } else {
            let index = self
                .files
                .iter()
                .position(|file| {
                    file.name == directory
                        || (file.name.as_bytes().starts_with(directory.as_bytes())
                            && file.name.as_bytes().get(directory.len) == Some(&b'\\'))
                })
                .ok_or(3u16)?;
            if index >= usize::from(u16::MAX) {
                return Err(8);
            }
            index as u16
        };
        Ok(Search {
            directory,
            pattern: pattern(mask)?,
            attributes: attributes as u8 & 0x3F,
            next: 0,
            parent,
        })
    }
    pub fn resume_search(&self, state: &[u8; 21]) -> Result<Search, u16> {
        if state[0] != 3 || state[20] != 0xD5 {
            return Err(18);
        }
        let parent = u16::from_le_bytes([state[2], state[3]]);
        let length = usize::from(state[4]);
        let directory = if length == 0 && parent == u16::MAX {
            Name::ROOT
        } else {
            let file = self.files.get(usize::from(parent)).ok_or(18u16)?;
            if length >= super::NAME_BYTES
                || (file.name.len != length && file.name.as_bytes().get(length) != Some(&b'\\'))
            {
                return Err(18);
            }
            let mut directory = file.name;
            directory.len = length;
            directory
        };
        let mut pattern = [0; 11];
        pattern.copy_from_slice(&state[5..16]);
        Ok(Search {
            directory,
            pattern,
            attributes: state[1],
            next: u32::from_le_bytes(state[16..20].try_into().unwrap()),
            parent,
        })
    }
    pub fn next_search(&self, search: &mut Search) -> Option<Entry> {
        if search.attributes == 8 {
            return None;
        } // No volume label on this virtual drive.
        for (index, file) in self.files.iter().enumerate().skip(search.next as usize) {
            search.next = index as u32 + 1;
            if file.deleted {
                continue;
            }
            let path = file.name.as_bytes();
            let suffix = if search.directory.len == 0 {
                path
            } else {
                if !path.starts_with(search.directory.as_bytes())
                    || path.get(search.directory.len) != Some(&b'\\')
                {
                    continue;
                }
                &path[search.directory.len + 1..]
            };
            let split = suffix.iter().position(|b| *b == b'\\');
            let name = split.map_or(suffix, |i| &suffix[..i]);
            let attributes = if let Some(i) = split {
                if search.attributes & 0x10 == 0 {
                    continue;
                }
                let length = path.len() - suffix.len() + i;
                if self.files[..index].iter().any(|previous| {
                    !previous.deleted
                        && previous.name.as_bytes().starts_with(&path[..length])
                        && previous.name.as_bytes().get(length) == Some(&b'\\')
                }) {
                    continue;
                }
                0x10
            } else if file.directory {
                if search.attributes & 0x10 == 0 {
                    continue;
                }
                0x10
            } else {
                0x20
            };
            let Some(short) = short_name(name) else {
                continue;
            };
            if !search
                .pattern
                .iter()
                .zip(short)
                .all(|(a, b)| *a == b'?' || *a == b)
            {
                continue;
            }
            let mut result = [0; 13];
            result[..name.len()].copy_from_slice(name);
            let length = if attributes == 0x10 {
                0
            } else {
                self.overlay
                    .as_ref()
                    .map_or(file.bytes.len() as u32, |overlay| {
                        overlay.length(index, file.bytes.len() as u32)
                    })
            };
            return Some(Entry {
                name: result,
                attributes,
                length,
            });
        }
        None
    }
}
fn short_name(name: &[u8]) -> Option<[u8; 11]> {
    let dot = name.iter().position(|b| *b == b'.').unwrap_or(name.len());
    let (base, ext) = (&name[..dot], name.get(dot + 1..).unwrap_or_default());
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || ext.contains(&b'.') {
        return None;
    }
    let mut short = [b' '; 11];
    short[..base.len()].copy_from_slice(base);
    short[8..8 + ext.len()].copy_from_slice(ext);
    Some(short)
}
fn pattern(mask: &[u8]) -> Result<[u8; 11], u16> {
    if mask.is_empty()
        || !mask.is_ascii()
        || mask.iter().any(|b| *b <= 32 || b":\\/\"<>|".contains(b))
    {
        return Err(3);
    }
    let dot = mask.iter().position(|b| *b == b'.').unwrap_or(mask.len());
    let mut result = [b' '; 11];
    for (start, width, component) in [
        (0, 8, &mask[..dot]),
        (8, 3, mask.get(dot + 1..).unwrap_or_default()),
    ] {
        for (position, &byte) in component.iter().enumerate() {
            if byte == b'*' {
                result[start + position..start + width].fill(b'?');
                break;
            }
            if position == width || byte == b'.' {
                return Err(3);
            }
            result[start + position] = byte.to_ascii_uppercase();
        }
    }
    if dot == mask.len() && mask.contains(&b'*') {
        result[8..].fill(b'?');
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn masks_subdirectories_attributes_and_independent_dtas() {
        let mut drive = DirectoryTree::new();
        for (name, length) in [
            (b"GAME\\ONE.SAV".as_slice(), 3),
            (b"GAME\\TWO.SAV", 4),
            (b"GAME\\README", 1),
            (b"GAME\\SUB\\A.TXT", 2),
            (b"GAME\\SUB\\B.TXT", 3),
        ] {
            drive.mount(name, vec![42; length]).unwrap();
        }
        drive.change_directory(b"GAME").unwrap();
        let mut saves = drive.start_search(b"*.sav", 0).unwrap();
        let first = drive.next_search(&mut saves).unwrap();
        assert_eq!(&first.name[..8], b"ONE.SAV\0");
        assert_eq!(first.length, 3);
        let saved = saves.state();
        let mut other = drive.start_search(b"C:\\GAME\\SUB\\?.txt", 0).unwrap();
        assert_eq!(
            &drive.next_search(&mut other).unwrap().name[..6],
            b"A.TXT\0"
        );
        let mut resumed = drive.resume_search(&saved).unwrap();
        drive.change_directory(b"SUB").unwrap();
        assert_eq!(
            &drive.next_search(&mut resumed).unwrap().name[..8],
            b"TWO.SAV\0"
        );
        assert!(drive.next_search(&mut resumed).is_none());
        let mut all = drive.start_search(b"C:\\GAME\\*.*", 0x10).unwrap();
        let mut names = alloc::vec::Vec::new();
        while let Some(entry) = drive.next_search(&mut all) {
            names.push(entry);
        }
        assert_eq!(names.len(), 4); // Three files and one deduplicated directory.
        assert_eq!(names.iter().filter(|e| e.attributes == 0x10).count(), 1);
        assert!(names.iter().any(|e| &e.name[..7] == b"README\0"));
        assert!(drive.start_search(b"C:\\MISSING\\*.*", 0).is_err());
        assert!(drive.start_search(b"D:\\*.*", 0).is_err());
        assert!(drive.resume_search(&[0; 21]).is_err());
    }
    #[test]
    fn searches_include_overlay_files_and_current_lengths() {
        let mut drive = DirectoryTree::new().with_write_overlay(256);
        let handle = drive.create(b"SAVE.SAV", 0).unwrap();
        drive.write_buffer(handle, 5).unwrap().fill(42);
        drive.close(handle).unwrap();
        let mut search = drive.start_search(b"SAVE?.SAV", 0).unwrap();
        let entry = drive.next_search(&mut search).unwrap();
        assert_eq!(&entry.name[..9], b"SAVE.SAV\0");
        assert_eq!(entry.length, 5);
    }
}
