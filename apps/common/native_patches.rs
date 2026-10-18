//! Native patch manifests. Only this setup adapter reads host files or hashes EXEs.
use doslet_runtime::session::patches::{
    MAX_PATCH_BYTES, MAX_PATCH_SPANS, MAX_PATCHES, MemoryPatch, PatchGuard, PatchWrite,
};
use sha2::{Digest, Sha256};
use std::{
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

fn parse(text: &str, base: &Path, digest: &[u8]) -> Result<MemoryPatch, Box<dyn Error>> {
    let doc = text.parse::<DocumentMut>()?;
    let table = doc.as_table();
    fields(
        table,
        &[
            "version",
            "name",
            "executable_sha256",
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
    for write in writes {
        fields(
            write,
            &[
                "address",
                "expected",
                "expected_zeroes",
                "replacement",
                "file",
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
    Ok(patch)
}

pub fn load(paths: &[PathBuf], program: &[u8]) -> Result<Vec<MemoryPatch>, Box<dyn Error>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
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
        let parsed = parse(
            &(header + "[[writes]]\naddress=0xf1800\nexpected='00 00'\nreplacement='aB cD'"),
            Path::new("."),
            &[0; 32],
        )
        .unwrap();
        assert_eq!(parsed.writes[0].replacement, [0xab, 0xcd]);
    }
}
