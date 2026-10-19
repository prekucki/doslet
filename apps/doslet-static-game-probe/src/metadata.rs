//! Bounded setup-only wire format for static code assumptions, not native code.
use crate::static_session::{CodeContext, CodePart, Metadata, ObjectBinding, RegionMetadata};
use doslet_x86::SegmentCache;
use doslet_x86_wasm::static_program::{Address, MAX_REGIONS};

const MAGIC: &[u8; 4] = b"DSM2";
const MAX_WIRE_BYTES: usize = 64 * 1024 * 1024;
const MAX_OBJECTS: usize = 65535;
const MAX_PARTS: usize = 1_048_576;
const MAX_CODE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub field: &'static str,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "static metadata byte{}: {}", self.offset, self.field)
    }
}
impl core::error::Error for Error {}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn error(&self, field: &'static str) -> Error {
        Error {
            offset: self.offset,
            field,
        }
    }
    fn take(&mut self, length: usize, field: &'static str) -> Result<&'a [u8], Error> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| self.error(field))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| self.error(field))?;
        self.offset = end;
        Ok(value)
    }
    fn u8(&mut self, field: &'static str) -> Result<u8, Error> {
        Ok(self.take(1, field)?[0])
    }
    fn u16(&mut self, field: &'static str) -> Result<u16, Error> {
        let b = self.take(2, field)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self, field: &'static str) -> Result<u32, Error> {
        let b = self.take(4, field)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn boolean(&mut self, field: &'static str) -> Result<bool, Error> {
        match self.u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.error(field)),
        }
    }
    fn count(&mut self, max: usize, min_bytes: usize, field: &'static str) -> Result<usize, Error> {
        let count = self.u32(field)? as usize;
        if count > max
            || count
                .checked_mul(min_bytes)
                .is_none_or(|n| n > self.bytes.len() - self.offset)
        {
            return Err(self.error(field));
        }
        Ok(count)
    }
    fn reserve<T>(&self, values: &mut Vec<T>, count: usize) -> Result<(), Error> {
        values
            .try_reserve_exact(count)
            .map_err(|_| self.error("allocation"))
    }
}

pub fn decode(bytes: &[u8]) -> Result<Metadata, Error> {
    let mut r = Reader { bytes, offset: 0 };
    if bytes.len() > MAX_WIRE_BYTES {
        return Err(r.error("header/size"));
    }
    let magic = r.take(4, "header")?;
    let legacy = magic == b"DSM1";
    if !legacy && magic != MAGIC {
        return Err(r.error("header/version"));
    }
    let cs = SegmentCache {
        selector: r.u16("CS selector")?,
        base: r.u32("CS base")?,
        limit: r.u32("CS limit")?,
        access: r.u8("CS access")?,
        flags: r.u8("CS flags")?,
        default32: r.boolean("CS default32")?,
        usable: r.boolean("CS usable")?,
    };
    let count = r.count(MAX_OBJECTS, 13, "object count")?;
    let mut objects = Vec::new();
    r.reserve(&mut objects, count)?;
    for _ in 0..count {
        objects.push(ObjectBinding {
            space: r.u32("object space")?,
            linear_base: r.u32("object base")?,
            size: r.u32("object size")?,
            default32: r.boolean("object mode")?,
        });
    }
    let count = r.count(MAX_REGIONS, if legacy { 20 } else { 24 }, "region count")?;
    let mut regions = Vec::new();
    r.reserve(&mut regions, count)?;
    let mut total_parts = 0usize;
    let mut total_code = 0usize;
    for _ in 0..count {
        let id = r.u32("region ID")?;
        let entry = Address {
            space: r.u32("entry space")?,
            offset: r.u32("entry offset")?,
        };
        let required_steps = r.u32("region work")?;
        let wait_proof_steps = if legacy {
            0
        } else {
            r.u32("wait proof bound")?
        };
        let count = r.count(MAX_PARTS - total_parts, 5, "part count")?;
        total_parts += count;
        let mut parts = Vec::new();
        r.reserve(&mut parts, count)?;
        for _ in 0..count {
            let linear = r.u32("part address")?;
            let length = usize::from(r.u8("part length")?);
            if length == 0 || length > MAX_CODE_BYTES - total_code {
                return Err(r.error("part length/code limit"));
            }
            total_code += length;
            let bytes = r.take(length, "part bytes")?;
            let mut expected = Vec::new();
            r.reserve(&mut expected, length)?;
            expected.extend_from_slice(bytes);
            parts.push(CodePart { linear, expected });
        }
        regions.push(RegionMetadata {
            id,
            entry,
            required_steps,
            wait_proof_steps,
            parts,
        });
    }
    if r.offset != bytes.len() {
        return Err(r.error("trailing bytes"));
    }
    Ok(Metadata {
        objects,
        regions,
        context: CodeContext { cs },
    })
}

pub fn encode(metadata: &Metadata) -> Result<Vec<u8>, Error> {
    let error = |field| Error { offset: 0, field };
    if metadata.objects.len() > MAX_OBJECTS || metadata.regions.len() > MAX_REGIONS {
        return Err(error("object/region count"));
    }
    let mut length = 26usize
        .checked_add(metadata.objects.len() * 13)
        .ok_or_else(|| error("length"))?;
    let mut total_parts = 0usize;
    let mut total_code = 0usize;
    for region in &metadata.regions {
        total_parts = total_parts
            .checked_add(region.parts.len())
            .ok_or_else(|| error("part count"))?;
        if total_parts > MAX_PARTS {
            return Err(error("part count"));
        }
        length = length.checked_add(24).ok_or_else(|| error("length"))?;
        for part in &region.parts {
            if !(1..=255).contains(&part.expected.len()) {
                return Err(error("part length"));
            }
            total_code = total_code
                .checked_add(part.expected.len())
                .ok_or_else(|| error("code length"))?;
            length = length
                .checked_add(5 + part.expected.len())
                .ok_or_else(|| error("length"))?;
        }
    }
    if total_code > MAX_CODE_BYTES || length > MAX_WIRE_BYTES {
        return Err(error("wire/code limit"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| error("allocation"))?;
    bytes.extend_from_slice(MAGIC);
    let cs = metadata.context.cs;
    bytes.extend_from_slice(&cs.selector.to_le_bytes());
    bytes.extend_from_slice(&cs.base.to_le_bytes());
    bytes.extend_from_slice(&cs.limit.to_le_bytes());
    bytes.extend_from_slice(&[
        cs.access,
        cs.flags,
        u8::from(cs.default32),
        u8::from(cs.usable),
    ]);
    let push = |bytes: &mut Vec<u8>, value: u32| bytes.extend_from_slice(&value.to_le_bytes());
    push(&mut bytes, metadata.objects.len() as u32);
    for object in &metadata.objects {
        push(&mut bytes, object.space);
        push(&mut bytes, object.linear_base);
        push(&mut bytes, object.size);
        bytes.push(u8::from(object.default32));
    }
    push(&mut bytes, metadata.regions.len() as u32);
    for region in &metadata.regions {
        push(&mut bytes, region.id);
        push(&mut bytes, region.entry.space);
        push(&mut bytes, region.entry.offset);
        push(&mut bytes, region.required_steps);
        push(&mut bytes, region.wait_proof_steps);
        push(&mut bytes, region.parts.len() as u32);
        for part in &region.parts {
            push(&mut bytes, part.linear);
            bytes.push(part.expected.len() as u8);
            bytes.extend_from_slice(&part.expected);
        }
    }
    debug_assert_eq!(bytes.len(), length);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Metadata {
        Metadata {
            context: CodeContext {
                cs: SegmentCache {
                    selector: 8,
                    base: 0,
                    limit: u32::MAX,
                    access: 0x9b,
                    flags: 0xc0,
                    default32: true,
                    usable: true,
                },
            },
            objects: vec![ObjectBinding {
                space: 1,
                linear_base: 0x1000,
                size: 4,
                default32: true,
            }],
            regions: vec![RegionMetadata {
                id: 0,
                entry: Address {
                    space: 1,
                    offset: 0,
                },
                required_steps: 2,
                wait_proof_steps: 0,
                parts: vec![CodePart {
                    linear: 0x1000,
                    expected: vec![0x40, 0x90],
                }],
            }],
        }
    }
    #[test]
    fn roundtrip_and_all_truncations_are_checked() {
        let metadata = fixture();
        let bytes = encode(&metadata).unwrap();
        assert_eq!(decode(&bytes).unwrap(), metadata);
        for length in 0..bytes.len() {
            assert!(decode(&bytes[..length]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err());
        let mut empty = fixture();
        empty.regions.clear();
        empty.objects.push(ObjectBinding {
            space: 2,
            linear_base: 0x2000,
            size: 4,
            default32: false,
        });
        assert_eq!(decode(&encode(&empty).unwrap()).unwrap(), empty);
        let mut legacy = bytes.clone();
        legacy[3] = b'1';
        legacy.drain(55..59); // DSM1 omits the new per-region hint field.
        assert_eq!(decode(&legacy).unwrap(), metadata);
        let mut hinted = metadata.clone();
        hinted.regions[0].required_steps = 0;
        hinted.regions[0].parts.clear();
        hinted.regions[0].wait_proof_steps = 10;
        assert_eq!(decode(&encode(&hinted).unwrap()).unwrap(), hinted);
    }
    #[test]
    fn lengths_counts_booleans_and_versions_reject_before_publication() {
        let mut metadata = fixture();
        metadata.regions[0].parts[0].expected = vec![0; 256];
        assert!(encode(&metadata).is_err());
        let mut bytes = encode(&fixture()).unwrap();
        bytes[0] = 0;
        assert!(decode(&bytes).is_err());
        let mut bytes = encode(&fixture()).unwrap();
        bytes[16] = 2;
        assert!(decode(&bytes).is_err());
        let mut bytes = encode(&fixture()).unwrap();
        bytes[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&bytes).is_err());
    }
}
