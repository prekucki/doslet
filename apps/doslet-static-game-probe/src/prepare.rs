//! Native offline preparation. The caller declares load layouts; code admission
//! later validates the actual running machine independently of these declarations.
use crate::static_session::{CodeContext, CodePart, Metadata, ObjectBinding, RegionMetadata};
use doslet_aot::{
    DefaultBitness, ImageLimits, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
    cfg::{CfgLimits, StaticCfg},
    profile::{
        BoundGuidance, Context, GuidanceLimits, GuidanceReport, GuidanceStatus, Profile, Tier,
    },
    waits::{WaitCandidates, WaitEvidence, WaitLimits},
    wasm::{LoweringLimits, LoweringReport, PreparedProgram},
};
use doslet_executable::{LeImage, LeLimits};
use doslet_runtime::Session;
use doslet_x86::{SegmentCache, SegmentRegister};
use doslet_x86_wasm::static_program::{Address, CompileOptions, EntryAlias};

#[derive(Clone, Copy)]
pub struct ProfileInput<'a> {
    pub profile: &'a Profile,
    /// Explicit historical source layout, before carrying labels to current code.
    pub source_bindings: &'a [DosFlatObjectBinding],
    pub expected_identity: Option<&'a str>,
}

pub struct PreparedGame {
    pub wasm: Vec<u8>,
    pub metadata: Metadata,
    pub report: LoweringReport,
    pub guidance_report: Option<GuidanceReport>,
    pub rebased_guidance: bool,
    pub hot_instructions: usize,
    pub cold_instructions: usize,
    pub irq_instructions: usize,
    pub wait_candidates: usize,
    /// All discovered candidates, before profile/policy selection.
    pub wait_evidence: [usize; 3],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WaitPolicy {
    #[default]
    Off,
    /// All conservative static candidates; runtime proof is still mandatory.
    Static,
    /// Predicate candidates without proven local progress, including unresolved
    /// aliases. No tier is required; runtime proof decides every omission.
    External,
    /// Only candidates whose complete body has verified COLD observations.
    Cold,
}

fn select_waits(
    plan: Option<&WaitCandidates<'_, '_>>,
    guidance: Option<&BoundGuidance<'_, '_, '_>>,
    policy: WaitPolicy,
) -> Result<Vec<(doslet_aot::ObjectAddress, u32)>, String> {
    let Some(plan) = plan else {
        return Ok(Vec::new());
    };
    let selected = plan
        .candidates()
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            (policy != WaitPolicy::External || candidate.evidence != WaitEvidence::LocalProgress)
                && (policy != WaitPolicy::Cold
                    || guidance.is_some_and(|guidance| {
                        plan.node_indices(*index)
                            .expect("valid candidate")
                            .iter()
                            .all(|&node| {
                                let row = guidance.instructions()[node];
                                row.status == GuidanceStatus::Verified
                                    && row.tier == Tier::Cold
                                    && row.context == Context::Unknown
                            })
                    }))
        })
        .map(|(_, candidate)| (candidate.head, candidate.recommended_proof_steps));
    let mut result = Vec::new();
    result
        .try_reserve_exact(plan.candidates().len())
        .map_err(|_| "wait metadata allocation")?;
    result.extend(selected);
    result.sort_unstable_by_key(|&(head, _)| head);
    if result.windows(2).any(|rows| rows[0].0 == rows[1].0) {
        return Err("duplicate wait head".into());
    }
    Ok(result)
}

/// Bounded explicit declaration, never a preferred-base or descriptor guess.
/// The offline selector properties do not establish live CPU presence/rights.
/// `selector: null` explicitly declares an address-only object; the field is
/// still required, and normal selector fixups cannot target such an object.
/// The DPMI mode the captured layout ran under. DOS4GW places code at other
/// addresses with a DPMI host; captures without the field ran without one.
pub fn bindings_dpmi(bytes: &[u8]) -> Result<&'static str, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    match value.get("dpmi") {
        None => Ok("off"),
        Some(mode) => match mode.as_str() {
            Some("off") => Ok("off"),
            Some("reflect") => Ok("reflect"),
            Some("direct") => Ok("direct"),
            _ => Err("dpmi must be off, reflect or direct".into()),
        },
    }
}

pub fn bindings_from_json(exe: &[u8], bytes: &[u8]) -> Result<Vec<DosFlatObjectBinding>, String> {
    if bytes.len() > 64 * 1024 {
        return Err("binding declarations exceed64KiB".into());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let root = value.as_object().ok_or("bindings must be an object")?;
    if !root.contains_key("objects") || root.keys().any(|k| k != "objects" && k != "dpmi") {
        return Err("bindings require only objects and an optional dpmi mode".into());
    }
    bindings_dpmi(bytes)?;
    let le = LeImage::discover(exe, LeLimits::default()).map_err(|e| e.to_string())?;
    let rows = value["objects"]
        .as_array()
        .ok_or("binding objects must be an array")?;
    if rows.len() != le.objects().len() {
        return Err("binding count differs from executable".into());
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(rows.len())
        .map_err(|_| "binding allocation")?;
    for row in rows {
        let fields = row.as_object().ok_or("binding row must be an object")?;
        if fields.keys().any(|key| {
            !matches!(
                key.as_str(),
                "object_number" | "linear_base" | "selector" | "alias_selector"
            )
        }) {
            return Err("unknown binding field".into());
        }
        let number = u32::try_from(
            row["object_number"]
                .as_u64()
                .ok_or("object_number must be u32")?,
        )
        .map_err(|_| "invalid object_number")?;
        let base = u32::try_from(
            row["linear_base"]
                .as_u64()
                .ok_or("linear_base must be u32")?,
        )
        .map_err(|_| "invalid linear_base")?;
        let selector = fields.get("selector").ok_or("missing selector")?;
        let selector = if selector.is_null() {
            None
        } else {
            Some(
                u16::try_from(selector.as_u64().ok_or("selector must be u16 or null")?)
                    .map_err(|_| "invalid selector")?,
            )
        };
        let object = le
            .object(
                usize::try_from(number.checked_sub(1).ok_or("zero object number")?)
                    .map_err(|_| "object index")?,
            )
            .ok_or("unknown object number")?;
        let bits = if object.flags & 0x2000 != 0 {
            DefaultBitness::Bits32
        } else {
            DefaultBitness::Bits16
        };
        let normal = selector.map(|value| DosFlatSelector {
            value,
            base: if bits == DefaultBitness::Bits32 {
                0
            } else {
                base
            },
            limit: if bits == DefaultBitness::Bits32 {
                u32::MAX
            } else {
                0xffff
            },
            default_bitness: bits,
        });
        let alias = fields
            .get("alias_selector")
            .map(|value| {
                let value = u16::try_from(value.as_u64().ok_or("alias_selector must be u16")?)
                    .map_err(|_| "invalid alias_selector")?;
                Ok::<_, String>(DosFlatSelector {
                    value,
                    base,
                    limit: 0xffff,
                    default_bitness: DefaultBitness::Bits16,
                })
            })
            .transpose()?;
        result.push(DosFlatObjectBinding {
            object_number: number,
            linear_base: base,
            normal,
            alias,
        });
    }
    Ok(result)
}

fn live_flat_context_matches(declared: DosFlatSelector, live: SegmentCache) -> bool {
    declared.base == 0
        && declared.limit == u32::MAX
        && declared.default_bitness == DefaultBitness::Bits32
        && live.selector == declared.value
        && live.base == declared.base
        && live.limit == declared.limit
        && live.default32
        && live.usable
        && live.access & 0x98 == 0x98
}

pub fn prepare(
    exe: &[u8],
    session: &Session,
    bindings: &[DosFlatObjectBinding],
    profile: Option<ProfileInput<'_>>,
    wait_policy: WaitPolicy,
) -> Result<PreparedGame, String> {
    prepare_with_memory_write_helper(exe, session, bindings, profile, wait_policy, false)
}

/// Controlled experiment; the default preparation path retains its existing
/// imports and memory behavior for runtimes without a memory-write helper.
pub fn prepare_with_memory_write_helper(
    exe: &[u8],
    session: &Session,
    bindings: &[DosFlatObjectBinding],
    profile: Option<ProfileInput<'_>>,
    wait_policy: WaitPolicy,
    memory_write_helper: bool,
) -> Result<PreparedGame, String> {
    let le = LeImage::discover(exe, LeLimits::default()).map_err(|e| e.to_string())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default()).map_err(|e| e.to_string())?;
    let binding = DosFlatBindingPlan::new(&image, bindings, BindingLimits::default())
        .map_err(|e| e.to_string())?;
    let flat32_entry = image.header().entry.is_some_and(|entry| {
        binding
            .object_binding(entry.object_number)
            .and_then(|object| object.normal)
            .is_some_and(|normal| {
                normal.base == 0
                    && normal.limit == u32::MAX
                    && normal.default_bitness == DefaultBitness::Bits32
            })
    });
    if flat32_entry {
        let entry = image.header().entry.ok_or("missing flat32 entry")?;
        let declared = binding
            .object_binding(entry.object_number)
            .ok_or("entry binding missing")?
            .normal
            .ok_or("entry normal selector missing")?;
        if session.debug_cpu().control.cr0 & 0x8000_0001 != 1
            || !live_flat_context_matches(
                declared,
                session.registers().segment_cache(SegmentRegister::Cs),
            )
        {
            return Err("flat32 binding differs from the live protected CS context".into());
        }
    }
    let graph = if flat32_entry {
        StaticCfg::analyze_bound(&binding, &[], CfgLimits::default())
    } else {
        StaticCfg::analyze(&image, &[], CfgLimits::default())
    }
    .map_err(|e| e.to_string())?;
    if wait_policy == WaitPolicy::Cold && profile.is_none() {
        return Err("COLD wait selection requires profile guidance".into());
    }
    let waits = if wait_policy == WaitPolicy::Off {
        None
    } else {
        Some(WaitCandidates::discover(&graph, WaitLimits::default()).map_err(|e| e.to_string())?)
    };
    let mut guidance_report = None;
    let (prepared, selected_waits) = if let Some(input) = profile {
        let source =
            DosFlatBindingPlan::new(&image, input.source_bindings, BindingLimits::default())
                .map_err(|e| e.to_string())?;
        let source = BoundGuidance::bind(
            input.profile,
            &graph,
            &source,
            input.expected_identity,
            GuidanceLimits::default(),
        )
        .map_err(|e| e.to_string())?;
        guidance_report = Some(source.report());
        let target = source
            .rebind_to(&binding, GuidanceLimits::default())
            .map_err(|e| e.to_string())?;
        let selected = select_waits(waits.as_ref(), Some(&target), wait_policy)?;
        let entries: Vec<_> = selected.iter().map(|&(entry, _)| entry).collect();
        let prepared = PreparedProgram::with_runtime_entries(
            &graph,
            &binding,
            Some(&target),
            &entries,
            LoweringLimits {
                direct_memory: true,
                isolate_mutable_sources: true,
                isolate_immediates: true,
                ..LoweringLimits::default()
            },
        )
        .map_err(|e| e.to_string())?;
        (prepared, selected)
    } else {
        let selected = select_waits(waits.as_ref(), None, wait_policy)?;
        let entries: Vec<_> = selected.iter().map(|&(entry, _)| entry).collect();
        let prepared = PreparedProgram::with_runtime_entries(
            &graph,
            &binding,
            None,
            &entries,
            LoweringLimits {
                direct_memory: true,
                isolate_mutable_sources: true,
                isolate_immediates: true,
                ..LoweringLimits::default()
            },
        )
        .map_err(|e| e.to_string())?;
        (prepared, selected)
    };
    let mut objects = Vec::new();
    objects
        .try_reserve_exact(image.objects().len())
        .map_err(|_| "object metadata allocation")?;
    for object in image
        .objects()
        .iter()
        .filter(|object| !object.bytes().is_empty())
    {
        let loaded = binding
            .object_binding(object.object_number())
            .ok_or("object binding missing")?;
        objects.push(ObjectBinding {
            space: object.object_number(),
            linear_base: loaded.linear_base,
            size: object.metadata().virtual_size,
            default32: object.default_bitness() == DefaultBitness::Bits32,
        });
    }
    let mut regions = Vec::new();
    regions
        .try_reserve_exact(prepared.program().regions.len())
        .map_err(|_| "region metadata allocation")?;
    let mut total_code = 0usize;
    for (region, sources) in prepared.program().regions.iter().zip(prepared.sources()) {
        if region.id != sources.id {
            return Err("lowered source IDs differ".into());
        }
        let required_steps =
            u32::try_from(region.completed_instructions()).map_err(|_| "region count overflow")?;
        let mut parts = Vec::new();
        if required_steps != 0 {
            let mut start = None;
            let mut end = 0u32;
            for &address in sources.instructions() {
                let index = graph
                    .nodes()
                    .binary_search_by_key(&address, |node| node.address)
                    .map_err(|_| "source instruction missing")?;
                let node = &graph.nodes()[index];
                if node.bitness != DefaultBitness::Bits32 {
                    return Err("PC acceptance adapter currently admits default-code32 only (8/16-bit operands are supported)".into());
                }
                if address.object_number != region.entry.space
                    || start.is_some() && address.offset != end
                {
                    return Err("region source is not contiguous".into());
                }
                start.get_or_insert(address.offset);
                end = address
                    .offset
                    .checked_add(u32::from(node.byte_len))
                    .ok_or("source extent overflow")?;
            }
            let start = start.ok_or("compiled region has no source")?;
            if start != region.entry.offset {
                return Err("region source entry differs".into());
            }
            let object = image
                .object(region.entry.space)
                .ok_or("source object missing")?;
            let original = object
                .bytes()
                .get(start as usize..end as usize)
                .ok_or("source extent outside object")?;
            total_code = total_code
                .checked_add(original.len())
                .ok_or("source byte count overflow")?;
            if total_code > 16 * 1024 * 1024 {
                return Err("runtime code metadata exceeds16MiB".into());
            }
            let loaded = binding
                .object_binding(region.entry.space)
                .ok_or("source binding missing")?;
            let patches = binding
                .object_patches(region.entry.space)
                .ok_or("source patches missing")?;
            parts
                .try_reserve_exact(original.len().div_ceil(255))
                .map_err(|_| "code part allocation")?;
            for (index, chunk) in original.chunks(255).enumerate() {
                let offset = start
                    .checked_add(u32::try_from(index * 255).map_err(|_| "chunk offset overflow")?)
                    .ok_or("chunk offset overflow")?;
                let linear = loaded
                    .linear_base
                    .checked_add(offset)
                    .ok_or("source linear overflow")?;
                let mut expected = Vec::new();
                expected
                    .try_reserve_exact(chunk.len())
                    .map_err(|_| "code bytes allocation")?;
                expected.extend_from_slice(chunk);
                let first = patches.partition_point(|patch| {
                    u64::from(patch.source.offset) + patch.bytes().len() as u64 <= u64::from(offset)
                });
                for patch in &patches[first..] {
                    if u64::from(patch.source.offset) >= u64::from(offset) + chunk.len() as u64 {
                        break;
                    }
                    for (index, &value) in patch.bytes().iter().enumerate() {
                        let at = u64::from(patch.source.offset) + index as u64;
                        if at >= u64::from(offset) && at - u64::from(offset) < expected.len() as u64
                        {
                            expected[(at - u64::from(offset)) as usize] = value;
                        }
                    }
                }
                parts.push(CodePart { linear, expected });
            }
        }
        regions.push(RegionMetadata {
            id: region.id.0,
            entry: region.entry,
            required_steps,
            wait_proof_steps: selected_waits
                .binary_search_by_key(
                    &doslet_aot::ObjectAddress {
                        object_number: region.entry.space,
                        offset: region.entry.offset,
                    },
                    |&(entry, _)| entry,
                )
                .ok()
                .map_or(0, |index| selected_waits[index].1),
            parts,
        });
    }
    // Aliases name existing compiled entries in the live flat linear address
    // space. They neither decode additional starts nor admit changed code.
    // Omit zero-work stubs: an unknown linear entry already falls back safely,
    // and object-end stubs can share a linear address with the next object.
    let mut aliases = Vec::new();
    if graph.binding().is_some() {
        aliases
            .try_reserve_exact(prepared.program().regions.len())
            .map_err(|_| "entry alias allocation")?;
        for region in &prepared.program().regions {
            if region.completed_instructions() == 0 {
                continue;
            }
            let loaded = binding
                .object_binding(region.entry.space)
                .ok_or("entry alias binding missing")?;
            let linear = loaded
                .linear_base
                .checked_add(region.entry.offset)
                .ok_or("entry alias linear overflow")?;
            aliases.push(EntryAlias {
                address: Address {
                    space: 0,
                    offset: linear,
                },
                target: region.id,
            });
        }
    }
    let compiled = prepared
        .program()
        .compile_with_aliases(
            &CompileOptions {
                direct_memory: true,
                runtime_admission: false,
                inline_admission: true,
                memory_write_helper,
                ..CompileOptions::default()
            },
            &aliases,
        )
        .map_err(|e| e.to_string())?;
    let metadata = Metadata {
        objects,
        regions,
        context: CodeContext {
            cs: session.registers().segment_cache(SegmentRegister::Cs),
        },
    };
    let count = |tier| {
        prepared
            .guidance()
            .iter()
            .filter(|row| row.tier == tier)
            .count()
    };
    let mut wait_evidence = [0; 3];
    if let Some(plan) = &waits {
        for candidate in plan.candidates() {
            let index = match candidate.evidence {
                WaitEvidence::ExternalMemory => 0,
                WaitEvidence::LocalProgress => 1,
                WaitEvidence::Uncertain => 2,
            };
            wait_evidence[index] += 1;
        }
    }
    Ok(PreparedGame {
        wasm: compiled.into_wasm(),
        metadata,
        report: prepared.report(),
        guidance_report,
        rebased_guidance: profile.is_some(),
        hot_instructions: count(Tier::Hot),
        cold_instructions: count(Tier::Cold),
        irq_instructions: count(Tier::Irq),
        wait_candidates: selected_waits.len(),
        wait_evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding_exe() -> Vec<u8> {
        // One synthetic code16 object with one file-backed page and no fixups.
        let mut bytes = vec![0; 272];
        bytes[..2].copy_from_slice(b"LE");
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
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
            (204, 5),
            (208, 1),
            (212, 1),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[220..224].copy_from_slice(&[0, 0, 1, 0]);
        bytes[256] = 0xc3;
        bytes
    }

    #[test]
    fn binding_json_requires_explicit_null_for_an_address_only_object() {
        let exe = binding_exe();
        let declaration = |selector: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "objects": [{"object_number": 1, "linear_base": 4096, "selector": selector}]
            }))
            .unwrap()
        };
        let bindings = bindings_from_json(&exe, &declaration(serde_json::Value::Null)).unwrap();
        assert_eq!(bindings[0].normal, None);
        let le = LeImage::discover(&exe, LeLimits::default()).unwrap();
        let image = SymbolicImage::from_le(&le, ImageLimits::default()).unwrap();
        assert!(DosFlatBindingPlan::new(&image, &bindings, BindingLimits::default()).is_ok());
        assert_eq!(
            bindings_from_json(
                &exe,
                br#"{"objects":[{"object_number":1,"linear_base":4096}]}"#,
            )
            .unwrap_err(),
            "missing selector"
        );
        // Numeric zero is a supplied invalid selector, never an absent view.
        let zero = bindings_from_json(&exe, &declaration(serde_json::json!(0))).unwrap();
        assert_eq!(zero[0].normal.unwrap().value, 0);
        assert!(matches!(
            DosFlatBindingPlan::new(&image, &zero, BindingLimits::default()),
            Err(doslet_aot::binding::BindingError::Selector {
                problem: doslet_aot::binding::SelectorProblem::Null,
                ..
            })
        ));
        for invalid in [
            serde_json::json!(false),
            serde_json::json!("null"),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(65536),
        ] {
            assert!(bindings_from_json(&exe, &declaration(invalid)).is_err());
        }
    }

    #[test]
    fn binding_json_names_the_dpmi_mode_of_its_layout() {
        let exe = binding_exe();
        let objects = r#""objects":[{"object_number":1,"linear_base":4096,"selector":null}]"#;
        let json = |extra: &str| format!("{{{objects}{extra}}}").into_bytes();
        assert_eq!(bindings_dpmi(&json("")).unwrap(), "off");
        for mode in ["off", "reflect", "direct"] {
            let bytes = json(&format!(r#","dpmi":"{mode}""#));
            assert_eq!(bindings_dpmi(&bytes).unwrap(), mode);
            assert!(bindings_from_json(&exe, &bytes).is_ok());
        }
        assert!(bindings_from_json(&exe, &json(r#","dpmi":"on""#)).is_err());
        assert!(bindings_from_json(&exe, &json(r#","dpmi":true"#)).is_err());
        assert!(bindings_from_json(&exe, &json(r#","other":1"#)).is_err());
    }

    #[test]
    fn flat_binding_requires_the_same_live_code_context() {
        let declared = DosFlatSelector {
            value: 0x180,
            base: 0,
            limit: u32::MAX,
            default_bitness: DefaultBitness::Bits32,
        };
        let live = SegmentCache {
            selector: declared.value,
            base: 0,
            limit: u32::MAX,
            access: 0x9b,
            flags: 0xc,
            default32: true,
            usable: true,
        };
        assert!(live_flat_context_matches(declared, live));
        for changed in [
            SegmentCache {
                selector: 0x188,
                ..live
            },
            SegmentCache {
                base: 0x10000,
                ..live
            },
            SegmentCache {
                limit: 0xffff,
                ..live
            },
            SegmentCache {
                default32: false,
                ..live
            },
            SegmentCache {
                usable: false,
                ..live
            },
            SegmentCache {
                access: 0x93,
                ..live
            },
        ] {
            assert!(!live_flat_context_matches(declared, changed));
        }
        assert!(!live_flat_context_matches(
            DosFlatSelector {
                base: 0x10000,
                ..declared
            },
            live
        ));
    }
}

#[cfg(test)]
#[path = "prepare/wait_tests.rs"]
mod wait_tests;
