//! Offline register-code preparation. Source signatures remain runtime guards;
//! neither a preferred address nor a profile proves a live loader layout.
use doslet_aot::{
    DefaultBitness, ImageLimits, SymbolicImage,
    cfg::{CfgLimits, StaticCfg},
};
use doslet_executable::{LeError, LeImage, LeLimits};
use doslet_x86_wasm::Translation;
use serde_json::json;

pub type Prepared = (Vec<u8>, Translation);
pub fn prepare(exe: &[u8]) -> crate::Result<Option<Prepared>> {
    // Other DOS formats keep the ordinary interpreter. Malformed LE input is
    // reported rather than silently treated as a successful translation.
    if !exe.starts_with(b"LE") && !exe.starts_with(b"MZ") {
        return Ok(None);
    }
    let le = match LeImage::discover(exe, LeLimits::default()) {
        Ok(le) => le,
        Err(LeError::NotFound | LeError::UnsupportedLx) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let cfg = StaticCfg::analyze(&image, &[], CfgLimits::default())?;
    let relocations: Vec<_> = image.relocations().collect();
    let mut nodes = Vec::new();
    // Loaders choose live object bases (DOS4GW differs with and without a DPMI
    // host). Declared spans let the runtime verify and apply one delta each.
    let mut objects = std::collections::BTreeMap::new();
    for node in cfg.nodes().iter().take(100_000) {
        if node.bitness != DefaultBitness::Bits32 {
            continue;
        }
        let object = image
            .object(node.address.object_number)
            .ok_or("missing CFG object")?;
        let Some(bytes) = object
            .bytes()
            .get(node.address.offset as usize..node.address.offset as usize + 15)
        else {
            continue;
        };
        // Relocated operands require a bound compiler. This conservative backend
        // leaves them to the interpreter instead of inventing load addresses.
        let first = relocations.partition_point(|r| {
            r.source.object_number < node.address.object_number
                || r.source.object_number == node.address.object_number
                    && r.source.offset < node.address.offset.saturating_sub(8)
        });
        if relocations[first..]
            .iter()
            .take_while(|r| {
                r.source.object_number == node.address.object_number
                    && u64::from(r.source.offset)
                        < u64::from(node.address.offset) + u64::from(node.byte_len)
            })
            .any(|r| {
                r.source.object_number == node.address.object_number
                    && u64::from(r.source.offset)
                        < u64::from(node.address.offset) + u64::from(node.byte_len)
                    && u64::from(r.source.offset) + u64::from(r.kind.width())
                        > u64::from(node.address.offset)
            })
        {
            continue;
        }
        let Some(pc) = object
            .metadata()
            .preferred_base
            .checked_add(node.address.offset)
        else {
            continue;
        };
        let metadata = object.metadata();
        objects.insert(
            node.address.object_number,
            (metadata.preferred_base, metadata.virtual_size),
        );
        nodes.push(json!({"pc":pc,"bits":32,"bytes":bytes,"count":0}));
    }
    let relocatable: Vec<_> = objects
        .values()
        .map(|(base, size)| json!({"base":base,"size":size}))
        .collect();
    let profile =
        serde_json::to_vec(&json!({"version":1,"relocatable":relocatable,"nodes":nodes}))?;
    let translation = Translation::from_bytes(&profile)?;
    Ok(Some((profile, translation)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_le_code_produces_a_module_and_real_mode_falls_back() {
        // One file-backed LE code32 object; no game data or loader assumptions.
        let mut exe = vec![0; 272];
        exe[..2].copy_from_slice(b"LE");
        exe[8..10].copy_from_slice(&2u16.to_le_bytes());
        for (offset, value) in [
            (20, 1u32),
            (24, 1),
            (40, 16),
            (48, 8),
            (56, 28),
            (64, 196),
            (68, 1),
            (72, 220),
            (104, 224),
            (108, 232),
            (128, 256),
            (196, 16),
            (200, 0x10000),
            (204, 0x2005),
            (208, 1),
            (212, 1),
        ] {
            exe[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        exe[220..224].copy_from_slice(&[0, 0, 1, 0]);
        exe[256..259].copy_from_slice(&[0x40, 0xeb, 0xfd]);
        let (profile, translation) = prepare(&exe).unwrap().unwrap();
        assert!(!translation.blocks().is_empty());
        assert!(translation.wasm().starts_with(b"\0asm"));
        let nodes: serde_json::Value = serde_json::from_slice(&profile).unwrap();
        assert_eq!(nodes["nodes"][0]["pc"], 0x10000);
        assert_eq!(
            translation.regions(),
            [doslet_x86_wasm::Region {
                base: 0x10000,
                size: 16
            }]
        );
        assert_eq!(nodes["nodes"][0]["bytes"][0], 0x40);
        assert!(prepare(&[0xb8, 0, 0, 0xcd, 0x21]).unwrap().is_none());
        assert!(prepare(b"LE").is_err());
    }
}
