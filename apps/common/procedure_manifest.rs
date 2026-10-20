//! Bounded procedure declarations shared by native and browser owners.
use doslet_runtime::wasm_procedures::{
    Binding, CodePart, CopyKernel, FiniteComputation, FiniteLoopGuard, KernelInterrupts, RleKernel,
    bundle,
};
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
    let entries = manifest
        .get("functions")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing AOT functions")?;
    if entries.is_empty() || entries.len() > MAX_FUNCTIONS {
        return Err("invalid AOT function count".into());
    }
    let mut bindings = Vec::with_capacity(entries.len());
    let mut source_count = 0usize;
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
        bindings.push(Binding {
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
}
