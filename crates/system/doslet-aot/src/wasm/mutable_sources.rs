//! Partitioning hints for reachable absolute MOV stores that may modify code.
//!
//! These are deliberately incomplete write candidates, not a proof that other
//! code is immutable. Treating a flat displacement as a possible linear address
//! can add unnecessary boundaries when the live data segment has another base.
//! It never permits execution: every resulting region still needs its complete
//! existing source-byte and runtime guards.

use super::{Budget, DosFlatBindingPlan, InstructionNode, LoweringError, StaticCfg, decode_node};
use alloc::vec::Vec;
use iced_x86::{Instruction, Mnemonic, OpKind, Register};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LinearInterval {
    start: u64,
    end: u64,
}

impl LinearInterval {
    fn new(start: u64, byte_len: u64) -> Option<Self> {
        (byte_len != 0).then_some(Self {
            start,
            end: start.checked_add(byte_len)?,
        })
    }
}

/// Sorted, merged possible linear store spans. No guest/object bytes are owned.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CodeWrites {
    intervals: Vec<LinearInterval>,
}

impl CodeWrites {
    /// Whether any candidate overlaps this already-discovered instruction.
    /// This is only a region-boundary hint; it does not relax source guards.
    pub(super) fn intersects(
        &self,
        node: &InstructionNode,
        binding: &DosFlatBindingPlan<'_>,
    ) -> bool {
        let Some(object) = binding.object_binding(node.address.object_number) else {
            return false;
        };
        let Some(start) = object.linear_base.checked_add(node.address.offset) else {
            return false;
        };
        let Some(source) = LinearInterval::new(u64::from(start), u64::from(node.byte_len)) else {
            return false;
        };
        let first = self
            .intervals
            .partition_point(|interval| interval.end <= source.start);
        self.intervals
            .get(first)
            .is_some_and(|interval| interval.start < source.end)
    }
}

/// Inspect only instructions reached by the exact bound CFG. Relocation
/// targets, pointer values and unvisited byte sequences never create work.
/// Unbound graphs provide no write hints because their addresses are symbolic.
pub(super) fn discover(
    graph: &StaticCfg<'_>,
    binding: &DosFlatBindingPlan<'_>,
    budget: &mut Budget,
) -> Result<CodeWrites, LoweringError> {
    if !core::ptr::eq(graph.image(), binding.image()) {
        return Err(LoweringError::DifferentImage);
    }
    if !graph.matches_binding(binding) {
        return Err(LoweringError::DifferentBinding);
    }
    if graph.binding().is_none() {
        return Ok(CodeWrites::default());
    }

    // Decode twice to reserve only candidate storage, rather than a vector as
    // large as the entire CFG. Each window is the parent's bounded stack copy.
    let mut count = 0;
    for node in graph.nodes() {
        if absolute_mov_write(&decode_node(node, graph, binding)?).is_some() {
            count += 1;
        }
    }
    let mut intervals = Vec::new();
    budget.reserve(&mut intervals, count)?;
    for node in graph.nodes() {
        if let Some(interval) = absolute_mov_write(&decode_node(node, graph, binding)?) {
            intervals.push(interval);
        }
    }
    intervals.sort_unstable();
    let mut merged = 0;
    for index in 0..intervals.len() {
        let interval = intervals[index];
        if merged != 0 && interval.start <= intervals[merged - 1].end {
            intervals[merged - 1].end = intervals[merged - 1].end.max(interval.end);
        } else {
            intervals[merged] = interval;
            merged += 1;
        }
    }
    intervals.truncate(merged);
    Ok(CodeWrites { intervals })
}

fn absolute_mov_write(instruction: &Instruction) -> Option<LinearInterval> {
    if instruction.mnemonic() != Mnemonic::Mov
        || instruction.op0_kind() != OpKind::Memory
        || instruction.memory_base() != Register::None
        || instruction.memory_index() != Register::None
    {
        return None;
    }
    let width = instruction.memory_size().size();
    if !matches!(width, 1 | 2 | 4) {
        return None;
    }
    let displacement = u32::try_from(instruction.memory_displacement64()).ok()?;
    // Keep the exclusive end widened, including stores at the final u32 byte.
    // No wrapping low-address write or dynamic segment base is guessed here.
    LinearInterval::new(u64::from(displacement), width as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DefaultBitness, FileBackedSpan, ObjectAddress, RelocationTarget, Site, SymbolicImage,
        SymbolicObject,
        binding::{BindingLimits, DosFlatObjectBinding, DosFlatSelector},
        cfg::CfgLimits,
    };
    use alloc::vec;
    use core::mem::size_of;
    use doslet_executable::{LeFixupSourceType, LeHeader, LeLocation, LeObject, LeObjectOffset};

    fn at(object_number: u32, offset: u32) -> ObjectAddress {
        ObjectAddress {
            object_number,
            offset,
        }
    }

    fn image(objects: &[&[u8]]) -> SymbolicImage {
        SymbolicImage {
            header: LeHeader {
                location: LeLocation {
                    container_offset: 0,
                    header_offset: 0,
                },
                cpu_type: 2,
                os_type: 0,
                module_version: 0,
                module_flags: 0,
                page_count: 0,
                page_size: 4096,
                last_page_bytes: 0,
                object_count: objects.len() as u32,
                entry: Some(LeObjectOffset {
                    object_number: 1,
                    offset: 0,
                }),
                stack: None,
                auto_data_object: None,
                data_pages_offset: 0,
            },
            objects: objects
                .iter()
                .enumerate()
                .map(|(index, bytes)| SymbolicObject {
                    object_number: index as u32 + 1,
                    metadata: LeObject {
                        virtual_size: bytes.len() as u32,
                        preferred_base: 0x7654_0000,
                        flags: 0x2007,
                        first_page_number: 0,
                        page_count: 0,
                    },
                    bytes: bytes.to_vec(),
                    file_backed_spans: vec![FileBackedSpan {
                        object_offset: 0,
                        file_offset: 0,
                        byte_len: bytes.len() as u32,
                    }],
                })
                .collect(),
            sites: Vec::new(),
            provenance: Vec::new(),
        }
    }

    fn binding<'a>(image: &'a SymbolicImage, bases: &[u32]) -> DosFlatBindingPlan<'a> {
        let declarations: Vec<_> = bases
            .iter()
            .enumerate()
            .map(|(index, &linear_base)| DosFlatObjectBinding {
                object_number: index as u32 + 1,
                linear_base,
                normal: Some(DosFlatSelector {
                    value: (index as u16 + 1) * 8,
                    base: 0,
                    limit: u32::MAX,
                    default_bitness: DefaultBitness::Bits32,
                }),
                alias: None,
            })
            .collect();
        DosFlatBindingPlan::new(image, &declarations, BindingLimits::default()).unwrap()
    }

    fn graph<'a>(binding: &'a DosFlatBindingPlan<'a>) -> StaticCfg<'a> {
        StaticCfg::analyze_bound(binding, &[], CfgLimits::default()).unwrap()
    }

    fn node<'graph>(
        graph: &'graph StaticCfg<'_>,
        address: ObjectAddress,
    ) -> &'graph InstructionNode {
        &graph.nodes()[graph
            .nodes()
            .binary_search_by_key(&address, |node| node.address)
            .unwrap()]
    }

    fn writes(graph: &StaticCfg<'_>, binding: &DosFlatBindingPlan<'_>) -> CodeWrites {
        discover(graph, binding, &mut Budget(usize::MAX)).unwrap()
    }

    #[test]
    fn reachable_relocated_write_hits_only_the_instruction_with_the_immediate() {
        let code = [
            0xc7, 0x05, 0, 0, 0, 0, 0, 0, 4, 0,    // mov dword [relocated], 0x40000
            0x40, // inc eax
            0x81, 0xc2, 0x78, 0x56, 0x34, 0x12, // add edx, 0x12345678
            0x43, 0xc3, // inc ebx; ret
        ];
        let mut image = image(&[&code]);
        image.sites.push(Site {
            source: at(1, 2),
            kind: LeFixupSourceType::Offset32,
            target: RelocationTarget {
                object_number: 1,
                offset: Some(13),
            },
            is_alias: false,
            provenance: 0..0,
        });
        let binding = binding(&image, &[0x1000]);
        let graph = graph(&binding);
        let writes = writes(&graph, &binding);
        assert_eq!(
            writes.intervals,
            [LinearInterval {
                start: 0x100d,
                end: 0x1011,
            }]
        );
        let hits: Vec<_> = graph
            .nodes()
            .iter()
            .filter(|node| writes.intersects(node, &binding))
            .map(|node| node.address)
            .collect();
        assert_eq!(hits, [at(1, 11)]);
        assert_eq!(image.object(1).unwrap().bytes(), code);
    }

    #[test]
    fn ordinary_data_write_does_not_overlap_source() {
        let image = image(&[&[0xa3, 0, 0x20, 0, 0, 0x40, 0xc3], &[0; 4]]);
        let binding = binding(&image, &[0x1000, 0x2000]);
        let graph = graph(&binding);
        let writes = writes(&graph, &binding);
        assert_eq!(writes.intervals.len(), 1);
        assert!(
            graph
                .nodes()
                .iter()
                .all(|node| !writes.intersects(node, &binding))
        );
        assert!(
            graph
                .nodes()
                .iter()
                .all(|node| node.address.object_number == 1)
        );
    }

    #[test]
    fn relocated_cross_object_writer_matches_the_bound_target_source() {
        let mut image = image(&[
            &[
                0xa3, 0, 0, 0, 0, // mov [relocated target], eax
                0xe9, 0, 0, 0, 0, // jmp relocated target object
            ],
            &[0x40, 0x81, 0xc1, 0x78, 0x56, 0x34, 0x12, 0xc3],
        ]);
        for (source, kind, offset) in [
            (at(1, 1), LeFixupSourceType::Offset32, 3),
            (at(1, 6), LeFixupSourceType::SelfRelative32, 0),
        ] {
            image.sites.push(Site {
                source,
                kind,
                target: RelocationTarget {
                    object_number: 2,
                    offset: Some(offset),
                },
                is_alias: false,
                provenance: 0..0,
            });
        }
        // Reverse linear order to exercise identity-to-layout translation.
        let binding = binding(&image, &[0x9000, 0x1000]);
        let graph = graph(&binding);
        let writes = writes(&graph, &binding);
        let hits: Vec<_> = graph
            .nodes()
            .iter()
            .filter(|node| writes.intersects(node, &binding))
            .map(|node| node.address)
            .collect();
        assert_eq!(hits, [at(2, 1)]);
        assert_eq!(image.object(1).unwrap().bytes()[1..5], [0; 4]);
    }

    #[test]
    fn unreachable_writer_fixup_and_pointer_do_not_add_candidates_or_roots() {
        let mut image = image(&[
            &[
                0xc3, // entry returns before the writer
                0xc7, 0x05, 0, 0, 0, 0, 0, 0, 0, 0, // unreachable relocated MOV
                0, 0, 0, 0, // ordinary relocated pointer
            ],
            &[0xa3, 0, 0x10, 0, 0, 0xc3],
        ]);
        for source in [at(1, 3), at(1, 11)] {
            image.sites.push(Site {
                source,
                kind: LeFixupSourceType::Offset32,
                target: RelocationTarget {
                    object_number: 2,
                    offset: Some(0),
                },
                is_alias: false,
                provenance: 0..0,
            });
        }
        let binding = binding(&image, &[0x1000, 0x2000]);
        let graph = graph(&binding);
        assert_eq!(graph.nodes().len(), 1);
        assert_eq!(graph.nodes()[0].address, at(1, 0));
        assert!(writes(&graph, &binding).intervals.is_empty());
        assert_eq!(image.object(1).unwrap().bytes()[3..7], [0; 4]);
    }

    #[test]
    fn read_only_and_dynamic_address_instructions_do_not_become_writes() {
        let image = image(&[&[
            0x83, 0x3d, 0, 0x10, 0, 0, 0, // cmp dword [0x1000], 0
            0xa1, 0, 0x10, 0, 0, // mov eax, [0x1000]
            0x89, 0x83, 0, 0x10, 0, 0, // mov [ebx+0x1000], eax
            0x89, 0x04, 0x8d, 0, 0x10, 0, 0, // mov [ecx*4+0x1000], eax
            0x87, 0x05, 0, 0x10, 0, 0, // xchg [0x1000], eax is not admitted
            0xc3,
        ]]);
        let binding = binding(&image, &[0x1000]);
        let graph = graph(&binding);
        assert!(writes(&graph, &binding).intervals.is_empty());
    }

    #[test]
    fn widths_overlaps_and_adjacency_merge_deterministically() {
        let image = image(&[&[
            0xa3, 0, 0x20, 0, 0, // dword [0x2000,0x2004)
            0xa2, 5, 0x20, 0, 0, // byte  [0x2005,0x2006)
            0x66, 0xa3, 3, 0x20, 0, 0, // word  [0x2003,0x2005)
            0xa2, 0xf0, 0x1f, 0, 0, // earlier nonadjacent byte
            0xa3, 0, 0x20, 0, 0, // duplicate dword
            0xc3,
        ]]);
        let binding = binding(&image, &[0x1000]);
        let graph = graph(&binding);
        let first = writes(&graph, &binding);
        assert_eq!(first, writes(&graph, &binding));
        assert_eq!(
            first.intervals,
            [
                LinearInterval {
                    start: 0x1ff0,
                    end: 0x1ff1,
                },
                LinearInterval {
                    start: 0x2000,
                    end: 0x2006,
                },
            ]
        );
    }

    #[test]
    fn overlapping_instruction_streams_are_checked_independently() {
        let image = image(&[&[
            0xa2, 0x0a, 0x10, 0, 0, // write a byte inside the MOV immediate below
            0x75, 3, // jnz into the MOV immediate
            0xb8, 0x90, 0x90, 0x40, 0x90, // mov eax, immediate
            0xc3,
        ]]);
        let binding = binding(&image, &[0x1000]);
        let graph = graph(&binding);
        let writes = writes(&graph, &binding);
        assert!(writes.intersects(node(&graph, at(1, 7)), &binding));
        assert!(writes.intersects(node(&graph, at(1, 10)), &binding));
        assert!(!writes.intersects(node(&graph, at(1, 11)), &binding));
        assert!(!writes.intersects(node(&graph, at(1, 12)), &binding));
    }

    #[test]
    fn widened_store_extent_covers_the_final_address_without_wrapping() {
        let image = image(&[&[0xa3, 0xff, 0xff, 0xff, 0xff, 0xc3], &[0xc3], &[0xc3]]);
        let binding = binding(&image, &[0x1000, u32::MAX, 0]);
        let graph = graph(&binding);
        let writes = writes(&graph, &binding);
        assert_eq!(
            writes.intervals,
            [LinearInterval {
                start: u64::from(u32::MAX),
                end: u64::from(u32::MAX) + 4,
            }]
        );
        let final_node = InstructionNode {
            address: at(2, 0),
            ..*node(&graph, at(1, 5))
        };
        assert!(writes.intersects(&final_node, &binding));
        assert!(!writes.intersects(
            &InstructionNode {
                address: at(3, 0),
                ..final_node
            },
            &binding
        ));
        assert_eq!(LinearInterval::new(u64::MAX, 1), None);
        assert_eq!(LinearInterval::new(0, 0), None);
    }

    #[test]
    fn storage_limit_is_charged_before_reserving_all_candidate_spans() {
        let image = image(&[&[0xa2, 0, 0x20, 0, 0, 0xa2, 0, 0x20, 0, 0, 0xc3]]);
        let binding = binding(&image, &[0x1000]);
        let graph = graph(&binding);
        let required = 2 * size_of::<LinearInterval>();
        assert_eq!(
            discover(&graph, &binding, &mut Budget(required - 1)).unwrap_err(),
            LoweringError::OwnedByteLimit
        );
        let mut budget = Budget(required);
        assert_eq!(
            discover(&graph, &binding, &mut budget)
                .unwrap()
                .intervals
                .len(),
            1
        );
        assert_eq!(budget.0, 0);
    }

    #[test]
    fn exact_bound_plan_is_required_and_unbound_graph_uses_no_storage() {
        let image = image(&[&[0xa3, 0, 0x10, 0, 0, 0xc3]]);
        let first = binding(&image, &[0x1000]);
        let equal = binding(&image, &[0x1000]);
        let graph = graph(&first);
        assert_eq!(
            discover(&graph, &equal, &mut Budget(0)).unwrap_err(),
            LoweringError::DifferentBinding
        );
        let other_image = self::image(&[&[0xc3]]);
        let other = binding(&other_image, &[0x1000]);
        assert_eq!(
            discover(&graph, &other, &mut Budget(0)).unwrap_err(),
            LoweringError::DifferentImage
        );
        let unbound = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
        assert!(
            discover(&unbound, &first, &mut Budget(0))
                .unwrap()
                .intervals
                .is_empty()
        );
    }
}
