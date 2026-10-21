//! Bounded procedure declarations shared by native and browser owners.
use doslet_runtime::wasm_procedures::{
    Binding, CodePart, CodeSegment, CopyKernel, ExecutionSite, FiniteComputation, FiniteLoopGuard,
    KernelInterrupts, RleKernel, bundle,
};
use doslet_x86_wasm::static_program::procedure_memory::X87Procedure;
use std::error::Error;
const MAX_FUNCTIONS: usize = 4096;
const MAX_SOURCES: usize = 65_536;
fn word(value: &serde_json::Value, key: &str) -> Result<u32, Box<dyn Error>> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("invalid AOT field {key}").into())
}
fn byte(value: &serde_json::Value, key: &str) -> Result<u8, Box<dyn Error>> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| format!("invalid AOT x87 field {key}").into())
}
fn x87_procedure(value: &serde_json::Value) -> Result<Option<X87Procedure>, Box<dyn Error>> {
    let Some(metadata) = value.get("x87") else {
        return Ok(None);
    };
    if metadata.is_null() {
        return Ok(None);
    }
    let object = metadata
        .as_object()
        .ok_or("invalid AOT x87 metadata object")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "read_mask" | "write_mask") {
            return Err(format!("unexpected AOT x87 field {key}").into());
        }
    }
    Ok(Some(X87Procedure {
        read_mask: byte(metadata, "read_mask")?,
        write_mask: byte(metadata, "write_mask")?,
    }))
}
fn terminals(value: &serde_json::Value, key: &str) -> Result<Vec<u32>, Box<dyn Error>> {
    let values = value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .filter(|values| values.len() <= 256)
        .ok_or("invalid finite terminal list")?;
    values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| "invalid finite terminal address".into())
        })
        .collect()
}
#[allow(
    dead_code,
    reason = "native legacy sidecars already have parsed manifests"
)]
pub fn parse(bytes: &[u8]) -> Result<Vec<Binding>, Box<dyn Error>> {
    if bytes.len() > bundle::MAX_METADATA_BYTES {
        return Err("oversized AOT procedure metadata".into());
    }
    let manifest: serde_json::Value = serde_json::from_slice(bytes)?;
    if word(&manifest, "version")? != 2 {
        return Err("unsupported AOT procedure bundle version".into());
    }
    bindings(&manifest)
}
pub fn bindings(manifest: &serde_json::Value) -> Result<Vec<Binding>, Box<dyn Error>> {
    if word(manifest, "memory_abi_version")? != doslet_runtime::wasm_procedures::MEMORY_ABI_VERSION
    {
        return Err(
            "AOT memory ABI differs; regenerate the procedure bundle with this compiler".into(),
        );
    }
    // Modules for flat DOS/4GW code predate the field. A DJGPP module names
    // its program's code segment: base and the limit its code needs.
    let code_segment = match manifest.get("code_segment") {
        None | Some(serde_json::Value::Null) => CodeSegment::FLAT,
        Some(segment) => CodeSegment {
            base: word(segment, "base")?,
            limit: word(segment, "limit")?,
        },
    };
    let entries = manifest
        .get("functions")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing AOT functions")?;
    if entries.is_empty() || entries.len() > MAX_FUNCTIONS {
        return Err("invalid AOT function count".into());
    }
    let mut bindings = Vec::with_capacity(entries.len());
    let mut source_count = 0usize;
    let mut execution_count = 0usize;
    for entry in entries {
        let sources = entry
            .get("sources")
            .and_then(serde_json::Value::as_array)
            .ok_or("missing AOT sources")?;
        source_count = source_count
            .checked_add(sources.len())
            .filter(|&n| n <= MAX_SOURCES)
            .ok_or("too many AOT sources")?;
        let mut code = Vec::with_capacity(sources.len());
        for source in sources {
            let values = source
                .get("bytes")
                .and_then(serde_json::Value::as_array)
                .ok_or("missing AOT source bytes")?;
            if !(1..=15).contains(&values.len()) {
                return Err("invalid AOT instruction length".into());
            }
            let mut expected = Vec::with_capacity(values.len());
            for value in values {
                expected.push(
                    value
                        .as_u64()
                        .and_then(|v| u8::try_from(v).ok())
                        .ok_or("invalid AOT source byte")?,
                );
            }
            code.push(CodePart {
                linear: word(source, "linear")?,
                expected,
            });
        }
        let execution_sites = match entry.get("execution_sites") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => {
                let sites = value
                    .as_array()
                    .filter(|sites| !sites.is_empty())
                    .ok_or("invalid AOT execution site list")?;
                execution_count = execution_count
                    .checked_add(sites.len())
                    .filter(|&count| count <= MAX_SOURCES)
                    .ok_or("too many AOT execution sites")?;
                let mut result = Vec::with_capacity(sites.len());
                let mut end = 0u64;
                for site in sites {
                    let ip = word(site, "ip")?;
                    let length = u8::try_from(word(site, "length")?)?;
                    if !(1..=15).contains(&length)
                        || u64::from(ip) < end
                        || u64::from(ip) + u64::from(length) > u64::from(u32::MAX) + 1
                    {
                        return Err("invalid or overlapping AOT execution sites".into());
                    }
                    result.push(ExecutionSite { ip, length });
                    end = u64::from(ip) + u64::from(length);
                }
                if result
                    .binary_search_by_key(&word(entry, "entry")?, |site| site.ip)
                    .is_err()
                {
                    return Err("AOT execution sites omit the entry".into());
                }
                Some(result)
            }
        };
        bindings.push(Binding {
            x87: x87_procedure(entry)?,
            execution_sites,
            function_id: word(entry, "id")?,
            entry_ip: word(entry, "entry")?,
            maximum_instructions: word(entry, "maximum_work")?,
            kernel: match entry.get("semantic_kernel") {
                None | Some(serde_json::Value::Null) => None,
                Some(kernel)
                    if kernel.get("kind").and_then(serde_json::Value::as_str)
                        == Some("rle-decode") =>
                {
                    None
                }
                Some(kernel) => {
                    if kernel.get("kind").and_then(serde_json::Value::as_str)
                        != Some("paired-strided-copy")
                    {
                        return Err("unsupported AOT semantic kernel".into());
                    }
                    let kernel = CopyKernel {
                        stride: word(kernel, "stride")?,
                        max_work_per_call: word(kernel, "maximum_work")?,
                        prefix_nops: word(kernel, "prefix_nops")?,
                        tail_nops: word(kernel, "tail_nops")?,
                    };
                    if !kernel.is_valid() {
                        return Err("invalid AOT semantic kernel metadata".into());
                    }
                    Some(kernel)
                }
            },
            rle_kernel: match entry.get("semantic_kernel") {
                Some(kernel)
                    if kernel.get("kind").and_then(serde_json::Value::as_str)
                        == Some("rle-decode") =>
                {
                    if u64::from(word(kernel, "maximum_work")?) != RleKernel.maximum_work() {
                        return Err("invalid RLE kernel work declaration".into());
                    }
                    Some(RleKernel)
                }
                _ => None,
            },
            finite_computation: match entry.get("finite_computation") {
                None | Some(serde_json::Value::Null) => None,
                Some(finite) => Some(FiniteComputation {
                    exits: terminals(finite, "exits")?,
                    returns: terminals(finite, "returns")?,
                    guards: match finite.get("guards") {
                        None => Vec::new(),
                        Some(guards) => {
                            let guards = guards
                                .as_array()
                                .filter(|array| array.len() <= 1)
                                .ok_or("invalid finite loop guard list")?;
                            let mut result = Vec::with_capacity(guards.len());
                            for guard in guards {
                                result.push(FiniteLoopGuard {
                                    entry_ip: word(guard, "entry")?,
                                    counter: u8::try_from(word(guard, "counter")?)?,
                                    decrement: guard
                                        .get("decrement")
                                        .and_then(serde_json::Value::as_bool)
                                        .ok_or("invalid finite loop direction")?,
                                    maximum_trips: word(guard, "maximum_trips")?,
                                });
                            }
                            result
                        }
                    },
                }),
            },
            kernel_interrupts: match entry
                .get("kernel_interrupts")
                .and_then(serde_json::Value::as_str)
            {
                None | Some("defer") => KernelInterrupts::Defer,
                Some("before-irq") => KernelInterrupts::BeforeNextIrq,
                Some(_) => return Err("unsupported AOT kernel interrupt policy".into()),
            },
            code_segment,
            code,
        });
    }
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> serde_json::Value {
        json!({"version":2, "memory_abi_version":doslet_runtime::wasm_procedures::MEMORY_ABI_VERSION,
            "functions":[{"id":7,"entry":65536,"maximum_work":3,
                "sources":[{"linear":65536,"bytes":[64,195]}]}]})
    }
    fn decode(value: &serde_json::Value) -> Result<Vec<Binding>, Box<dyn Error>> {
        parse(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn browser_and_native_keep_identical_ids_source_bytes_and_finite_guards() {
        let mut value = manifest();
        value["functions"][0]["finite_computation"] = json!({
            "exits":[65550], "returns":[65560],
            "guards":[{"entry":65536,"counter":1,"decrement":true,"maximum_trips":19}]});
        value["functions"][0]["kernel_interrupts"] = json!("before-irq");
        let mut parsed = decode(&value).unwrap();
        let binding = parsed.pop().unwrap();
        assert_eq!(
            (
                binding.function_id,
                binding.entry_ip,
                binding.maximum_instructions
            ),
            (7, 65536, 3)
        );
        assert_eq!(binding.code[0].linear, 65536);
        assert_eq!(binding.code[0].expected, [64, 195]);
        assert_eq!(binding.kernel_interrupts, KernelInterrupts::BeforeNextIrq);
        let finite = binding.finite_computation.unwrap();
        assert_eq!(finite.exits, [65550]);
        assert_eq!(finite.returns, [65560]);
        assert_eq!(finite.guards[0].counter, 1);
        assert!(finite.guards[0].decrement);
        assert_eq!(finite.guards[0].maximum_trips, 19);
    }

    #[test]
    fn code_segment_defaults_to_flat_and_applies_to_every_binding() {
        let binding = decode(&manifest()).unwrap().pop().unwrap();
        assert_eq!(binding.code_segment, CodeSegment::FLAT);
        let mut value = manifest();
        value["code_segment"] = json!({"base": 0x120000, "limit": 0xedffff});
        let binding = decode(&value).unwrap().pop().unwrap();
        assert_eq!(
            binding.code_segment,
            CodeSegment {
                base: 0x120000,
                limit: 0xedffff
            }
        );
        value["code_segment"] = json!({"base": 0x120000});
        assert!(decode(&value).is_err());
    }

    #[test]
    fn semantic_kernels_preserve_native_work_reservations() {
        let mut value = manifest();
        value["functions"][0]["semantic_kernel"] =
            json!({"kind":"rle-decode", "maximum_work":RleKernel.maximum_work()});
        let binding = decode(&value).unwrap().pop().unwrap();
        assert!(binding.rle_kernel.is_some());
        assert!(binding.kernel.is_none());
        value["functions"][0]["semantic_kernel"]["maximum_work"] = json!(1);
        assert!(decode(&value).is_err());
        value["functions"][0]["semantic_kernel"] = json!({"kind":"unknown"});
        assert!(decode(&value).is_err());
    }

    #[test]
    fn malformed_metadata_is_rejected_before_export_registration() {
        for (field, invalid) in [
            ("version", json!(1)),
            ("memory_abi_version", json!(0)),
            ("functions", json!([])),
        ] {
            let mut value = manifest();
            value[field] = invalid;
            assert!(decode(&value).is_err(), "accepted invalid {field}");
        }
        for invalid in [
            json!([]),
            json!([256]),
            json!([-1]),
            json!([1.5]),
            json!(vec![1; 16]),
        ] {
            let mut value = manifest();
            value["functions"][0]["sources"][0]["bytes"] = invalid;
            assert!(decode(&value).is_err());
        }
        for key in ["id", "entry", "maximum_work"] {
            let mut value = manifest();
            value["functions"][0][key] = json!(u64::from(u32::MAX) + 1);
            assert!(decode(&value).is_err());
        }
        let mut value = manifest();
        value["functions"][0]["kernel_interrupts"] = json!("unknown");
        assert!(decode(&value).is_err());
    }

    #[test]
    fn x87_permissions_are_optional_and_parse_byte_masks() {
        let legacy = decode(&manifest()).unwrap().pop().unwrap();
        assert!(legacy.x87.is_none());

        let mut value = manifest();
        value["functions"][0]["x87"] = serde_json::Value::Null;
        assert!(decode(&value).unwrap().pop().unwrap().x87.is_none());

        value["functions"][0]["x87"] = json!({"read_mask":0,"write_mask":255});
        let x87 = decode(&value).unwrap().pop().unwrap().x87.unwrap();
        assert_eq!(x87.read_mask, 0);
        assert_eq!(x87.write_mask, u8::MAX);
    }

    #[test]
    fn malformed_x87_permissions_are_rejected() {
        for invalid in [
            json!(false),
            json!([]),
            json!({}),
            json!({"read_mask":1}),
            json!({"write_mask":1}),
            json!({"read_mask":-1,"write_mask":0}),
            json!({"read_mask":256,"write_mask":0}),
            json!({"read_mask":1.5,"write_mask":0}),
            json!({"read_mask":"1","write_mask":0}),
            json!({"read_mask":0,"write_mask":0,"other":1}),
        ] {
            let mut value = manifest();
            value["functions"][0]["x87"] = invalid;
            assert!(decode(&value).is_err());
        }
    }

    #[test]
    fn explicit_execution_sites_preserve_guard_bytes_and_reject_malformed_lists() {
        let legacy = decode(&manifest()).unwrap().pop().unwrap();
        assert!(legacy.execution_sites.is_none());
        let mut value = manifest();
        value["functions"][0]["sources"][0]["bytes"] = json!([64, 217, 232]);
        value["functions"][0]["execution_sites"] = json!([{"ip":65536,"length":1}]);
        let binding = decode(&value).unwrap().pop().unwrap();
        assert_eq!(
            binding.execution_sites.unwrap(),
            [ExecutionSite {
                ip: 65536,
                length: 1
            }]
        );
        assert_eq!(binding.code[0].expected, [64, 217, 232]);
        for invalid in [
            json!([]),
            json!({}),
            json!([{"ip":65537,"length":1}]),
            json!([{"ip":65536,"length":0}]),
            json!([{"ip":65536,"length":16}]),
            json!([{"ip":65536,"length":2},{"ip":65537,"length":1}]),
            json!([{"ip":65536,"length":1},{"ip":65536,"length":1}]),
            json!([{"ip":65536}]),
        ] {
            value["functions"][0]["execution_sites"] = invalid;
            assert!(decode(&value).is_err());
        }
    }
}
