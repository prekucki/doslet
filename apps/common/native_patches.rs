//! Native patch manifests. Only this setup adapter reads host files or hashes EXEs.
use doslet_runtime::session::patches::{
    MAX_PATCH_BYTES, MAX_PATCH_SPANS, MAX_PATCHES, MemoryPatch, PatchGuard, PatchWrite,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, TableLike};

const MAX_MANIFEST_BYTES: usize = 512 * 1024;

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()).into());
    }
    Ok(bytes)
}

fn fields(table: &dyn TableLike, allowed: &[&str]) -> Result<(), Box<dyn Error>> {
    for (key, _) in table.iter() {
        if !allowed.contains(&key) {
            return Err(format!("unknown patch field {key}").into());
        }
    }
    Ok(())
}

fn string<'a>(table: &'a dyn TableLike, key: &str) -> Result<&'a str, Box<dyn Error>> {
    table
        .get(key)
        .and_then(Item::as_str)
        .ok_or_else(|| format!("{key} must be a string").into())
}

fn number(table: &dyn TableLike, key: &str) -> Result<u64, Box<dyn Error>> {
    table
        .get(key)
        .and_then(Item::as_integer)
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(|| format!("{key} must be a nonnegative integer").into())
}

fn address(table: &dyn TableLike) -> Result<u32, Box<dyn Error>> {
    Ok(u32::try_from(number(table, "address")?)?)
}

fn hex(text: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) || digits.len() > MAX_PATCH_BYTES * 2 {
        return Err("hex bytes must contain 1..65536 complete byte pairs".into());
    }
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let hi = char::from(pair[0]).to_digit(16).ok_or("invalid hex byte")?;
            let lo = char::from(pair[1]).to_digit(16).ok_or("invalid hex byte")?;
            Ok((hi * 16 + lo) as u8)
        })
        .collect()
}

/// Absolute addresses of a manifest taken with its program's LE objects at
/// `bases` (object 1 first). A DOS extender places the objects elsewhere with
/// a DPMI host or another memory configuration.
#[derive(Clone, Debug, PartialEq)]
struct Relocations {
    bases: Vec<u32>,
    /// Per write: offsets in its replacement of absolute addresses and of
    /// 32-bit displacements, as in JMP or CALL rel32.
    writes: Vec<(Vec<u32>, Vec<u32>)>,
}

/// A patch as written and, when it names its objects, its relocations.
type Manifest = (MemoryPatch, Option<Relocations>);

fn offsets(table: &dyn TableLike, key: &str, length: usize) -> Result<Vec<u32>, Box<dyn Error>> {
    let Some(item) = table.get(key) else {
        return Ok(Vec::new());
    };
    let values = item
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of byte offsets"))?;
    values
        .iter()
        .map(|value| {
            let offset = value
                .as_integer()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&offset| offset as usize + 4 <= length)
                .ok_or_else(|| format!("{key} offsets must address 4 bytes of the replacement"))?;
            Ok(offset)
        })
        .collect()
}

fn parse(text: &str, base: &Path, digest: &[u8]) -> Result<Manifest, Box<dyn Error>> {
    let doc = text.parse::<DocumentMut>()?;
    let table = doc.as_table();
    fields(
        table,
        &[
            "version",
            "name",
            "executable_sha256",
            "objects",
            "timeout_steps",
            "writes",
            "guards",
        ],
    )?;
    if number(table, "version")? != 1 {
        return Err("patch version must be 1".into());
    }
    if hex(string(table, "executable_sha256")?)? != digest {
        return Err("executable SHA-256 does not match this patch".into());
    }
    let bases = match table.get("objects") {
        None => None,
        Some(item) => Some(
            item.as_array()
                .filter(|values| (1..=64).contains(&values.len()))
                .ok_or("objects must list 1..64 LE object bases")?
                .iter()
                .map(|value| {
                    value
                        .as_integer()
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or("objects must hold 32-bit linear addresses")
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    let writes = table
        .get("writes")
        .and_then(Item::as_array_of_tables)
        .ok_or("patch requires [[writes]]")?;
    if writes.is_empty() || writes.len() > MAX_PATCH_SPANS {
        return Err("patch requires 1..128 writes".into());
    }
    let mut patch = MemoryPatch {
        name: string(table, "name")?.to_owned(),
        timeout_steps: number(table, "timeout_steps")?,
        writes: Vec::new(),
        guards: Vec::new(),
    };
    let mut relocations = Vec::new();
    for write in writes {
        fields(
            write,
            &[
                "address",
                "expected",
                "expected_zeroes",
                "replacement",
                "file",
                "abs32",
                "rel32",
            ],
        )?;
        let replacement = match (write.get("replacement"), write.get("file")) {
            (Some(_), None) => hex(string(write, "replacement")?)?,
            (None, Some(_)) => {
                let file = string(write, "file")?;
                if file.is_empty() {
                    return Err("patch file path must not be empty".into());
                }
                read(&base.join(file), MAX_PATCH_BYTES)?
            }
            _ => return Err("write requires exactly one of replacement or file".into()),
        };
        let expected = match (write.get("expected"), write.get("expected_zeroes")) {
            (Some(_), None) => hex(string(write, "expected")?)?,
            (None, Some(_)) => {
                let length = number(write, "expected_zeroes")?;
                if length == 0 || length > MAX_PATCH_BYTES as u64 {
                    return Err("expected_zeroes must be 1..65536".into());
                }
                vec![0; length as usize]
            }
            _ => return Err("write requires exactly one of expected or expected_zeroes".into()),
        };
        if replacement.len() != expected.len() {
            return Err("expected/replacement lengths differ".into());
        }
        let absolute = offsets(write, "abs32", replacement.len())?;
        let relative = offsets(write, "rel32", replacement.len())?;
        let mut fields: Vec<u32> = absolute.iter().chain(&relative).copied().collect();
        fields.sort_unstable();
        if fields.windows(2).any(|pair| pair[1] < pair[0] + 4) {
            return Err("abs32 and rel32 fields must not overlap".into());
        }
        if !fields.is_empty() && bases.is_none() {
            return Err("abs32 and rel32 require the objects the addresses were taken at".into());
        }
        relocations.push((absolute, relative));
        patch.writes.push(PatchWrite {
            address: address(write)?,
            expected,
            replacement,
        });
    }
    if let Some(guards) = table.get("guards") {
        for guard in guards
            .as_array_of_tables()
            .ok_or("guards must use [[guards]]")?
        {
            fields(guard, &["address", "expected"])?;
            patch.guards.push(PatchGuard {
                address: address(guard)?,
                expected: hex(string(guard, "expected")?)?,
            });
        }
    }
    if patch.writes.len() + patch.guards.len() > MAX_PATCH_SPANS
        || patch
            .writes
            .iter()
            .map(|w| w.expected.len())
            .chain(patch.guards.iter().map(|g| g.expected.len()))
            .sum::<usize>()
            > MAX_PATCH_BYTES
    {
        return Err("patch exceeds 128 spans or 64 KiB".into());
    }
    let relocations = bases.map(|bases| Relocations {
        bases,
        writes: relocations,
    });
    Ok((patch, relocations))
}

fn manifests(paths: &[PathBuf], program: &[u8]) -> Result<Vec<Manifest>, Box<dyn Error>> {
    if paths.len() > MAX_PATCHES {
        return Err("at most 32 memory patches are supported".into());
    }
    let digest = Sha256::digest(program);
    paths
        .iter()
        .map(|path| {
            let result = (|| {
                let bytes = read(path, MAX_MANIFEST_BYTES)?;
                parse(
                    std::str::from_utf8(&bytes)?,
                    path.parent().unwrap_or(Path::new(".")),
                    &digest,
                )
            })();
            result.map_err(|e: Box<dyn Error>| format!("patch {}: {e}", path.display()).into())
        })
        .collect()
}

/// The patches as written. Relocatable ones wait at the addresses of the
/// layout they name until [`relocate`] moves them.
pub fn load(paths: &[PathBuf], program: &[u8]) -> Result<Vec<MemoryPatch>, Box<dyn Error>> {
    Ok(manifests(paths, program)?
        .into_iter()
        .map(|(patch, _)| patch)
        .collect())
}

/// One loaded LE object: `(number, base, size)`.
pub type Span = (u32, u32, u32);

fn object(spans: &[Span], address: u32) -> Option<Span> {
    spans
        .iter()
        .copied()
        .find(|&(_, base, size)| address.wrapping_sub(base) < size)
}

/// `address` moved with its object; addresses outside every object stay.
fn rebase(address: u32, from: &[Span], to: &[Span]) -> Result<u32, Box<dyn Error>> {
    let Some((number, base, _)) = object(from, address) else {
        return Ok(address);
    };
    let offset = address - base;
    let (_, target, size) = to
        .iter()
        .copied()
        .find(|span| span.0 == number)
        .ok_or_else(|| format!("the live layout lacks LE object {number}"))?;
    if offset >= size {
        return Err(format!("{address:#x} lies outside the live LE object {number}").into());
    }
    Ok(target + offset)
}

fn relocate_patch(
    patch: &MemoryPatch,
    relocations: &Relocations,
    sizes: &BTreeMap<u32, u32>,
    live: &[Span],
) -> Result<MemoryPatch, Box<dyn Error>> {
    let from = relocations
        .bases
        .iter()
        .enumerate()
        .map(|(index, &base)| {
            let number = index as u32 + 1;
            let size = sizes
                .get(&number)
                .ok_or_else(|| format!("the program has no LE object {number}"))?;
            Ok((number, base, *size))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let mut moved = patch.clone();
    for guard in &mut moved.guards {
        guard.address = rebase(guard.address, &from, live)?;
    }
    for (index, (write, (absolute, relative))) in
        moved.writes.iter_mut().zip(&relocations.writes).enumerate()
    {
        let source = write.address;
        write.address = rebase(source, &from, live)?;
        let field = |bytes: &[u8], offset: u32| {
            let at = offset as usize;
            u32::from_le_bytes(bytes[at..at + 4].try_into().expect("validated field"))
        };
        for &offset in absolute {
            let value = field(&write.replacement, offset);
            if object(&from, value).is_none() {
                return Err(format!(
                    "abs32 at offset {offset} of write {} holds {value:#x}, outside every LE object",
                    index + 1
                )
                .into());
            }
            let at = offset as usize;
            write.replacement[at..at + 4]
                .copy_from_slice(&rebase(value, &from, live)?.to_le_bytes());
        }
        for &offset in relative {
            let end = source.wrapping_add(offset + 4);
            let target = end.wrapping_add(field(&write.replacement, offset));
            if object(&from, end).is_none() && object(&from, target).is_none() {
                return Err(format!(
                    "rel32 at offset {offset} of write {} neither starts nor ends in an LE object",
                    index + 1
                )
                .into());
            }
            let moved_end = write.address.wrapping_add(offset + 4);
            let displacement = rebase(target, &from, live)?.wrapping_sub(moved_end);
            let at = offset as usize;
            write.replacement[at..at + 4].copy_from_slice(&displacement.to_le_bytes());
        }
    }
    Ok(moved)
}

/// The plan with every manifest that names its LE object bases moved to the
/// `live` layout. `sizes` holds the loaded size of each object number.
#[allow(dead_code, reason = "only the desktop follows live layouts")]
pub fn relocate(
    paths: &[PathBuf],
    program: &[u8],
    sizes: &BTreeMap<u32, u32>,
    live: &[Span],
) -> Result<Vec<MemoryPatch>, Box<dyn Error>> {
    paths
        .iter()
        .zip(manifests(paths, program)?)
        .map(|(path, (patch, relocations))| match relocations {
            None => Ok(patch),
            Some(relocations) => relocate_patch(&patch, &relocations, sizes, live)
                .map_err(|e| format!("patch {}: {e}", path.display()).into()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_manifest_rejects_ambiguous_and_malformed_edits() {
        let header = format!(
            "version=1\nname='test'\nexecutable_sha256='{}'\ntimeout_steps=100\n",
            "00".repeat(32)
        );
        for body in [
            "[[writes]]\naddress=-1\nexpected='00'\nreplacement='01'",
            "[[writes]]\naddress=0\nexpected='0'\nreplacement='01'",
            "[[writes]]\naddress=0\nexpected='zz'\nreplacement='01'",
            "[[writes]]\naddress=0\nexpected='00'\nexpected_zeroes=1\nreplacement='01'",
            "[[writes]]\naddress=0\nexpected='00'\nreplacement='01'\nfile='ambiguous.bin'",
            "[[writes]]\naddress=0\nexpected='00'\nreplacement='0102'",
            "[[writes]]\naddress=0\nexpected='00'\nreplacement='01'\ntypo=true",
            "[[writes]]\naddress=0\nexpected_zeroes=65537\nreplacement='01'",
        ] {
            assert!(
                parse(&(header.clone() + body), Path::new("."), &[0; 32]).is_err(),
                "{body}"
            );
        }
        let (parsed, relocations) = parse(
            &(header + "[[writes]]\naddress=0xf1800\nexpected='00 00'\nreplacement='aB cD'"),
            Path::new("."),
            &[0; 32],
        )
        .unwrap();
        assert!(relocations.is_none());
        assert_eq!(parsed.writes[0].replacement, [0xab, 0xcd]);
    }

    #[test]
    fn relocatable_writes_follow_their_objects_to_the_live_layout() {
        let header = format!(
            "version=1\nname='moved'\nexecutable_sha256='{}'\ntimeout_steps=100\nobjects=[0x100000, 0x200000]\n",
            "00".repeat(32)
        );
        // A JMP rel32 in object 1 reaches a payload in fixed BIOS RAM, whose
        // operand addresses object 2 and whose own JMP returns to object 1.
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x200040u32.to_le_bytes());
        payload.push(0xe9);
        payload.extend_from_slice(&0x100020u32.wrapping_sub(0xf1809).to_le_bytes());
        let body = format!(
            "[[guards]]\naddress=0x100000\nexpected='55'\n\
             [[writes]]\naddress=0x100010\nexpected='00 00 00 00 00'\nreplacement='e9 eb 17 ff ff'\nrel32=[1]\n\
             [[writes]]\naddress=0xf1800\nexpected_zeroes=9\nreplacement='{}'\nabs32=[0]\nrel32=[5]\n",
            payload
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let (patch, relocations) =
            parse(&(header.clone() + &body), Path::new("."), &[0; 32]).unwrap();
        let relocations = relocations.unwrap();
        assert_eq!(relocations.bases, [0x100000, 0x200000]);
        let sizes = BTreeMap::from([(1, 0x1000), (2, 0x1000)]);
        let live = [(1, 0x150000, 0x1000), (2, 0x260000, 0x1000)];
        let moved = relocate_patch(&patch, &relocations, &sizes, &live).unwrap();
        assert_eq!(moved.guards[0].address, 0x150000);
        assert_eq!(moved.writes[0].address, 0x150010);
        let jump = i32::from_le_bytes(moved.writes[0].replacement[1..5].try_into().unwrap());
        assert_eq!(0x150015i64 + i64::from(jump), 0xf1800);
        let payload = &moved.writes[1].replacement;
        assert_eq!(moved.writes[1].address, 0xf1800);
        assert_eq!(payload[..4], 0x260040u32.to_le_bytes());
        let back = u32::from_le_bytes(payload[5..9].try_into().unwrap());
        assert_eq!(0xf1809u32.wrapping_add(back), 0x150020);
        // The same layout leaves every byte as written.
        let same = [(1, 0x100000, 0x1000), (2, 0x200000, 0x1000)];
        assert_eq!(
            relocate_patch(&patch, &relocations, &sizes, &same).unwrap(),
            patch
        );
        let outside = body
            .replace("abs32=[0]", "abs32=[5]")
            .replace("rel32=[5]", "rel32=[0]");
        let (patch, relocations) =
            parse(&(header.clone() + &outside), Path::new("."), &[0; 32]).unwrap();
        assert!(relocate_patch(&patch, &relocations.unwrap(), &sizes, &live).is_err());
        for invalid in [
            body.replace("rel32=[5]", "rel32=[2]"),
            body.replace("rel32=[5]", "rel32=[6]"),
        ] {
            assert!(parse(&(header.clone() + &invalid), Path::new("."), &[0; 32]).is_err());
        }
        let unplaced = header.replace("objects=[0x100000, 0x200000]\n", "") + &body;
        assert!(parse(&unplaced, Path::new("."), &[0; 32]).is_err());
    }
}
