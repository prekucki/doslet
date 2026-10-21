//! Explicit PC24 procedure-only lowering. Ordinary static programs keep their
//! interpreter boundary at every x87 instruction.
use super::*;
use doslet_x86_wasm::static_program::{X87Instruction, X87Operation as X};

pub(super) fn lower(
    instruction: &Instruction,
    offsets: &iced_x86::ConstantOffsets,
    ip: u32,
    node: &InstructionNode,
    graph: &StaticCfg<'_>,
    binding: &DosFlatBindingPlan<'_>,
    virtual_registers: &mutable_sources::VirtualRegisters,
) -> Option<Operation> {
    if node.bitness != DefaultBitness::Bits32
        || instruction.has_lock_prefix()
        || instruction.has_rep_prefix()
        || instruction.has_repne_prefix()
    {
        return None;
    }
    let object = graph.image().object(node.address.object_number)?;
    let start = node.address.offset as usize;
    let bytes = object
        .bytes()
        .get(start..start + usize::from(node.byte_len))?;
    let at = bytes
        .iter()
        .position(|byte| !matches!(byte, 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67))?;
    let opcode = *bytes.get(at)?;
    let modrm = *bytes.get(at + 1)?;
    if !(0xd8..=0xdf).contains(&opcode) {
        return None;
    }
    // Encoding bytes supply FOP; relocated displacement data comes from the
    // already bound decoder. Refuse exotic relocations of opcode/prefix bytes.
    let prefix_end = node.address.offset.checked_add(at as u32 + 2)?;
    let patches = binding.object_patches(node.address.object_number)?;
    let first = patches.partition_point(|patch| {
        u64::from(patch.source.offset) + patch.bytes().len() as u64
            <= u64::from(node.address.offset)
    });
    if patches[first..]
        .iter()
        .take_while(|patch| patch.source.offset < prefix_end)
        .any(|patch| {
            patch.source.offset < prefix_end
                && u64::from(patch.source.offset) + patch.bytes().len() as u64
                    > u64::from(node.address.offset)
        })
    {
        return None;
    }
    let group = (modrm >> 3) & 7;
    let index = modrm & 7;
    let arithmetic = matches!(group, 0 | 1 | 4 | 5 | 6 | 7);
    let operation = if modrm < 0xc0 {
        let slot = (offsets.has_displacement() && offsets.displacement_size() == 4)
            .then(|| ip.checked_add(offsets.displacement_offset() as u32))
            .flatten()
            .filter(|&slot| slot >= 0x500 && virtual_registers.intersects_slot(slot));
        let address = memory_address_with_vreg(instruction, slot)?;
        match (opcode, group) {
            (0xd9, 0) => X::LoadReal32 { address },
            (0xdb, 0) => X::LoadInteger32 { address },
            (0xdb, 2 | 3) => X::StoreInteger32 {
                address,
                pop: group == 3,
            },
            (0xd8, 2 | 3) => X::CompareReal32 {
                address,
                pop: group == 3,
            },
            (0xd8, _) if arithmetic => X::ArithmeticReal32 {
                address,
                operation: group,
            },
            _ => return None,
        }
    } else {
        match (opcode, group) {
            (0xde, 3) if modrm == 0xd9 => X::CompareRegister { index: 1, pops: 2 },
            (0xdf, 4) if modrm == 0xe0 => X::StoreStatusAx,
            (0xd8, 2 | 3) => X::CompareRegister {
                index,
                pops: u8::from(group == 3),
            },
            (0xdd, 2 | 3) => X::StoreRegister {
                index,
                pop: group == 3,
            },
            (0xd9, 0) => X::Duplicate { index },
            (0xd9, 1) => X::Exchange { index },
            (0xd8, _) if arithmetic => X::ArithmeticRegister {
                index,
                operation: group,
                target_top: true,
                pop: false,
            },
            (0xdc | 0xde, _) if arithmetic => X::ArithmeticRegister {
                index,
                operation: if group >= 4 { group ^ 1 } else { group },
                target_top: false,
                pop: opcode == 0xde,
            },
            _ => return None,
        }
    };
    Some(Operation::X87(X87Instruction {
        opcode: (u16::from(opcode & 7) << 8) | u16::from(modrm),
        operation,
    }))
}
