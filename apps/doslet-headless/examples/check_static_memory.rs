//! Synthetic shared-WASM memory acceptance; no game assets or browser required.
//! Build the Rust probe for wasm32 in release, then run this example in release
//! with `--features wasm-static-test`. An optional argument selects the probe.
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;
#[allow(dead_code)]
#[path = "../../common/wasm_game.rs"]
mod wasm_game;

use doslet_static_game_probe::{Probe, metadata, static_session::*};
use doslet_wasm_aot::{shared_runtime::SharedArtifacts, static_program::Exit};
use doslet_x86::SegmentRegister;
use doslet_x86_wasm::static_program::{
    Accounting, Address, CompileOptions, Operation, Program, Region, RegionId, Register, Source,
    Terminator, Width,
    memory::{self, MemoryAddress, Segment, StackAddress},
};
use serde_json::Value;
use std::{
    error::Error,
    fs,
    io::{Cursor, Read, Write},
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const CODE: u32 = 0x10300;
const SEEDS: [u32; 8] = [
    0xd4c3b2a1, 0x81706f5e, 0x44332211, 0xc897a6b5, 0x7b6a5948, 0x1e0dfceb, 0x5a493827, 0x9e8d7c6b,
];

#[derive(Clone, Copy, Debug)]
enum Video {
    Chain4,
    Plane2,
    Compare,
}

struct Body {
    bytes: Vec<u8>,
    regions: Vec<Region>,
    metadata: Vec<RegionMetadata>,
}
impl Body {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            regions: Vec::new(),
            metadata: Vec::new(),
        }
    }
    /// Each IR operation is paired with the actual guest instruction it replaces.
    fn instruction(&mut self, bytes: &[u8], operation: Operation) {
        let next = RegionId(self.regions.len() as u32 + 1);
        self.region(bytes, vec![operation], Terminator::Fallthrough(next));
    }
    fn region(&mut self, bytes: &[u8], operations: Vec<Operation>, terminator: Terminator) {
        let id = self.regions.len() as u32;
        let entry = Address {
            space: 1,
            offset: self.bytes.len() as u32,
        };
        let region = Region {
            id: RegionId(id),
            entry,
            operations,
            terminator,
            accounting: Accounting::Counted,
        };
        let required_steps = region.completed_instructions() as u32;
        self.regions.push(region);
        self.metadata.push(RegionMetadata {
            id,
            entry,
            required_steps,
            wait_proof_steps: 0,
            parts: if bytes.is_empty() {
                vec![]
            } else {
                vec![CodePart {
                    linear: CODE + entry.offset,
                    expected: bytes.to_vec(),
                }]
            },
        });
        self.bytes.extend_from_slice(bytes);
    }
    fn immediate(&mut self, opcode: u8, target: Register, value: u32) {
        let mut bytes = vec![opcode];
        bytes.extend_from_slice(&value.to_le_bytes());
        self.instruction(
            &bytes,
            Operation::Move {
                target,
                source: Source::Immediate(value),
            },
        );
    }
}

fn address(base: Option<Register>, displacement: u32) -> MemoryAddress {
    MemoryAddress {
        base,
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment: Segment::Ds,
    }
}

fn body(video: Video) -> Body {
    use Register::*;
    let mut body = Body::new();
    let video_address = if matches!(video, Video::Chain4) {
        0xa0011
    } else {
        0xa0005
    };
    body.immediate(0xb8, Eax, video_address);
    body.instruction(
        &[0x8a, 0x18],
        Operation::LoadMemory {
            target: Bl,
            address: address(Some(Eax), 0),
        },
    );
    body.instruction(
        &[0x66, 0x8b, 0x08],
        Operation::LoadMemory {
            target: Cx,
            address: address(Some(Eax), 0),
        },
    );
    // Destination aliases the address register: preflight must retain the old EAX.
    body.instruction(
        &[0x8b, 0x00],
        Operation::LoadMemory {
            target: Eax,
            address: address(Some(Eax), 0),
        },
    );
    body.instruction(
        &[0xa3, 0xff, 0x1f, 0x02, 0x00],
        Operation::StoreMemory {
            address: address(None, 0x21fff),
            width: Width::Dword,
            source: Source::Register(Eax),
        },
    );
    body.instruction(
        &[0x8b, 0x15, 0xff, 0x1f, 0x02, 0x00],
        Operation::LoadMemory {
            target: Edx,
            address: address(None, 0x21fff),
        },
    );
    body.immediate(0xbb, Ebx, 0x23000);
    // High-byte source aliases the address register, followed by a high-byte load.
    body.instruction(
        &[0x88, 0x3b],
        Operation::StoreMemory {
            address: address(Some(Ebx), 0),
            width: Width::Byte,
            source: Source::Register(Bh),
        },
    );
    body.instruction(
        &[0x8a, 0x23],
        Operation::LoadMemory {
            target: Ah,
            address: address(Some(Ebx), 0),
        },
    );
    body.instruction(
        &[0x66, 0x89, 0x43, 0x01],
        Operation::StoreMemory {
            address: address(Some(Ebx), 1),
            width: Width::Word,
            source: Source::Register(Ax),
        },
    );
    body.immediate(0xb9, Ecx, 0x1234eeee);
    body.instruction(
        &[0x88, 0x4b, 0x04],
        Operation::StoreMemory {
            address: address(Some(Ebx), 4),
            width: Width::Byte,
            source: Source::Register(Cl),
        },
    );
    body.instruction(
        &[0x66, 0x8b, 0x0b],
        Operation::LoadMemory {
            target: Cx,
            address: address(Some(Ebx), 0),
        },
    );
    body.instruction(
        &[0x66, 0x89, 0x4b, 0x07],
        Operation::StoreMemory {
            address: address(Some(Ebx), 7),
            width: Width::Word,
            source: Source::Register(Cx),
        },
    );
    body.immediate(0xbf, Edi, 0x24000);
    body.instruction(
        &[0x89, 0x3f],
        Operation::StoreMemory {
            address: address(Some(Edi), 0),
            width: Width::Dword,
            source: Source::Register(Edi),
        },
    );
    body.instruction(
        &[0x66, 0x8b, 0x3f],
        Operation::LoadMemory {
            target: Di,
            address: address(Some(Edi), 0),
        },
    );
    body.instruction(
        &[0x66, 0x89, 0x15, 0xff, 0x2f, 0x02, 0x00],
        Operation::StoreMemory {
            address: address(None, 0x22fff),
            width: Width::Word,
            source: Source::Register(Dx),
        },
    );
    body.instruction(
        &[0x8a, 0x1d, 0xff, 0x2f, 0x02, 0x00],
        Operation::LoadMemory {
            target: Bl,
            address: address(None, 0x22fff),
        },
    );
    body.instruction(
        &[0x88, 0x3d, 0x10, 0x40, 0x02, 0x00],
        Operation::StoreMemory {
            address: address(None, 0x24010),
            width: Width::Byte,
            source: Source::Register(Bh),
        },
    );
    body.instruction(
        &[0x89, 0x15, 0x12, 0x40, 0x02, 0x00],
        Operation::StoreMemory {
            address: address(None, 0x24012),
            width: Width::Dword,
            source: Source::Register(Edx),
        },
    );
    for (prefix, target, width, signed) in [
        ([0x0f, 0xb6, 0x35], Esi, Width::Byte, false),
        ([0x0f, 0xbf, 0x3d], Edi, Width::Word, true),
    ] {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&video_address.to_le_bytes());
        body.instruction(
            &bytes,
            Operation::ExtendMemory {
                target,
                width,
                address: address(None, video_address),
                signed,
            },
        );
    }
    body.instruction(
        &[0x0f, 0xb7, 0x0d, 0xff, 0x1f, 0x02, 0],
        Operation::ExtendMemory {
            target: Ecx,
            width: Width::Word,
            address: address(None, 0x21fff),
            signed: false,
        },
    );
    for (bytes, operation) in [
        (
            &[0x66, 0x98][..],
            Operation::Extend {
                target: Ax,
                source: Al,
                signed: true,
            },
        ),
        (
            &[0x98][..],
            Operation::Extend {
                target: Eax,
                source: Ax,
                signed: true,
            },
        ),
        (
            &[0x0f, 0xb6, 0xd4][..],
            Operation::Extend {
                target: Edx,
                source: Ah,
                signed: false,
            },
        ),
    ] {
        body.instruction(bytes, operation);
    }
    body.instruction(
        &[0x8d, 0x7c, 0x40, 0xf0],
        Operation::Lea {
            target: Edi,
            address: MemoryAddress {
                base: Some(Eax),
                index: Some(Eax),
                scale: 2,
                displacement: 0xffff_fff0,
                address16: false,
                segment: Segment::Ds,
            },
        },
    );
    body.instruction(
        &[0x6b, 0xc9, 0xfd],
        Operation::SignedMultiply {
            target: Ecx,
            left: Source::Register(Ecx),
            right: Source::Immediate(0xffff_fffd),
        },
    );
    body.instruction(
        &[0x0f, 0xaf, 0x15, 0xff, 0x1f, 0x02, 0],
        Operation::SignedMultiplyMemory {
            target: Edx,
            address: address(None, 0x21fff),
            source: Source::Register(Edx),
        },
    );
    let mut multiply_video = vec![0x66, 0x6b, 0x35];
    multiply_video.extend_from_slice(&video_address.to_le_bytes());
    multiply_video.push(0xfc);
    body.instruction(
        &multiply_video,
        Operation::SignedMultiplyMemory {
            target: Si,
            address: address(None, video_address),
            source: Source::Immediate(0xfffc),
        },
    );
    body.instruction(
        &[0xf6, 0xe4],
        Operation::WideMultiply {
            signed: false,
            width: Width::Byte,
            source: Source::Register(Ah),
        },
    );
    body.instruction(
        &[0x66, 0xf7, 0xea],
        Operation::WideMultiply {
            signed: true,
            width: Width::Word,
            source: Source::Register(Dx),
        },
    );
    body.instruction(
        &[0xf7, 0x25, 0xff, 0x1f, 0x02, 0],
        Operation::WideMultiplyMemory {
            signed: false,
            width: Width::Dword,
            address: address(None, 0x21fff),
        },
    );
    for (bytes, operation) in [
        (
            &[0x50][..],
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Eax),
            },
        ),
        (&[0x5b][..], Operation::Pop { target: Ebx }),
        (
            &[0x66, 0x50][..],
            Operation::Push {
                width: Width::Word,
                source: Source::Register(Ax),
            },
        ),
        (&[0x66, 0x59][..], Operation::Pop { target: Cx }),
        (
            &[0x6a, 0xff][..],
            Operation::Push {
                width: Width::Dword,
                source: Source::Immediate(u32::MAX),
            },
        ),
        (&[0x5a][..], Operation::Pop { target: Edx }),
        (
            &[0x54][..],
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Esp),
            },
        ),
        (&[0x5c][..], Operation::Pop { target: Esp }),
        (
            &[0x66, 0x54][..],
            Operation::Push {
                width: Width::Word,
                source: Source::Register(Sp),
            },
        ),
        (&[0x66, 0x5c][..], Operation::Pop { target: Sp }),
    ] {
        body.instruction(bytes, operation);
    }
    // CALL next; POP ESI exercises a real near CALL as one guarded PUSH plus
    // an internal fallthrough edge, including its actual linear return EIP.
    let return_eip = CODE + body.bytes.len() as u32 + 5;
    body.instruction(
        &[0xe8, 0, 0, 0, 0],
        Operation::Push {
            width: Width::Dword,
            source: Source::Immediate(return_eip),
        },
    );
    body.instruction(&[0x5e], Operation::Pop { target: Esi });
    // A complete CALL / RET round trip returns through the static linear alias
    // of the caller's JMP, with no callback or runtime-created dispatcher entry.
    let call_id = body.regions.len() as u32;
    let return_eip = CODE + body.bytes.len() as u32 + 5;
    body.region(
        &[0xe8, 5, 0, 0, 0],
        vec![Operation::Push {
            width: Width::Dword,
            source: Source::Immediate(return_eip),
        }],
        Terminator::Fallthrough(RegionId(call_id + 2)),
    );
    body.region(
        &[0xe9, 1, 0, 0, 0],
        vec![],
        Terminator::Jump(RegionId(call_id + 3)),
    );
    body.region(
        &[0xc3],
        vec![],
        Terminator::Return {
            width: Width::Dword,
            pop_bytes: 0,
            target_space: 0,
        },
    );
    body.region(
        &[],
        vec![],
        Terminator::Fallback(Address {
            space: 1,
            offset: body.bytes.len() as u32,
        }),
    );
    body
}

fn fixture(
    video: Video,
    stack_address: StackAddress,
    write_mode: Option<u8>,
    body: &Body,
) -> Result<Vec<u8>> {
    let mut com = vec![0x90; 0x440];
    let mut bootstrap = vec![
        0xb8, 0x13, 0x00, 0xcd, 0x10, // BIOS VGA mode 13h
        0xfa, 0xb0, 0x02, 0xe6, 0x92, // CLI; enable A20 through the guest port
        0xb8, 0x00, 0xa0, 0x8e, 0xc0, // ES=A000
    ];
    for (index, seed) in SEEDS.into_iter().enumerate() {
        bootstrap.extend_from_slice(&[0x26, 0x66, 0xc7, 0x06]);
        bootstrap.extend_from_slice(&(0x10u16 + index as u16 * 4).to_le_bytes());
        bootstrap.extend_from_slice(&seed.to_le_bytes());
    }
    if !matches!(video, Video::Chain4) {
        // Disable chain4, then choose plane 2, using ordinary guest OUTs.
        bootstrap.extend_from_slice(&[0xba, 0xc4, 0x03, 0xb8, 0x04, 0x06, 0xef]);
        bootstrap.extend_from_slice(&[0xba, 0xce, 0x03, 0xb8, 0x04, 0x02, 0xef]);
    }
    if matches!(video, Video::Compare) {
        for (index, value) in [(2, 3), (5, 0x48), (7, 5)] {
            bootstrap.extend_from_slice(&[0xb8, index, value, 0xef]);
        }
    }
    if let Some(mode) = write_mode {
        // Set/reset, logical XOR, partial bit/plane masks and write modes 0..3
        // exercise the ordinary VGA bus path, including retained read latches.
        bootstrap.extend_from_slice(&[0xba, 0xc4, 0x03, 0xb8, 2, 5, 0xef]);
        bootstrap.extend_from_slice(&[0xba, 0xce, 0x03]);
        for (index, value) in [
            (0, 9),
            (1, 5),
            (3, 0x1b),
            (
                5,
                if matches!(video, Video::Compare) {
                    0x48 | mode
                } else {
                    0x40 | mode
                },
            ),
            (8, 0xa5),
        ] {
            bootstrap.extend_from_slice(&[0xb8, index, value, 0xef]);
        }
    }
    bootstrap.extend_from_slice(&[
        0x0f, 0x01, 0x16, 0x20, 0x05, // LGDT [0520]
        0x0f, 0x20, 0xc0, 0x66, 0x83, 0xc8, 0x01, 0x0f, 0x22, 0xc0, 0x66, 0xea, 0x80, 0x02, 0x01,
        0x00, 0x08, 0x00, // JMP 0008:00010280
    ]);
    assert!(bootstrap.len() < 0x180);
    com[..bootstrap.len()].copy_from_slice(&bootstrap);
    let mut protected = vec![
        0x66, 0xb8, 0x10, 0x00, 0x8e, 0xd8, 0x8e, 0xc0, // flat DS/ES
    ];
    if stack_address == StackAddress::Bits16 {
        protected.extend_from_slice(&[0x66, 0xb8, 0x18, 0x00]);
    }
    protected.extend_from_slice(&[0x8e, 0xd0, 0xbc, 0x00, 0xf0, 0x03, 0x00]); // SS; private ESP
    let displacement = CODE - (0x10280 + protected.len() as u32 + 5);
    protected.push(0xe9);
    protected.extend_from_slice(&displacement.to_le_bytes()); // clear SS shadow
    com[0x180..0x180 + protected.len()].copy_from_slice(&protected);
    com[0x200..0x200 + body.bytes.len()].copy_from_slice(&body.bytes);
    com[0x200 + body.bytes.len()] = 0xf4; // outside measured instruction budget
    com[0x400..0x408].fill(0);
    com[0x408..0x410].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x9b, 0xcf, 0]);
    com[0x410..0x418].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x93, 0xcf, 0]);
    com[0x418..0x420].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x93, 0, 0]);
    com[0x420..0x426].copy_from_slice(&[0x1f, 0, 0x00, 0x05, 0x01, 0]);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("MEMORY.COM", zip::write::SimpleFileOptions::default())?;
    writer.write_all(&com)?;
    let zip = writer.finish()?.into_inner();
    let config = b"version=2\n[machine]\nmemory_mib=4\n[game]\nexecutable='MEMORY.COM'\n[[drives]]\nletter='C'\nkind='zip'\nsource='fixture.zip'\npolicy='temporary'\n";
    let spec = machine_profile::parse_machine_profile(std::str::from_utf8(config)?)?;
    let archive = archive::GameArchive::new(Arc::new(zip.clone()))?;
    let mut session = archive.load_configured("MEMORY.COM", &spec)?;
    wasm_game::setup(&mut session);
    for _ in 0..256 {
        if session.debug_pc() == CODE {
            break;
        }
        assert_eq!(session.advance(1), 1, "bootstrap: {:?}", session.fault());
    }
    assert_eq!(
        session.debug_pc(),
        CODE,
        "bounded bootstrap did not reach measured code"
    );
    assert!(session.registers().code32());
    assert_eq!(
        session.registers().stack32(),
        stack_address == StackAddress::Bits32
    );
    let flat = session.registers().segment_cache(SegmentRegister::Ds);
    assert_eq!((flat.base, flat.limit, flat.default32), (0, u32::MAX, true));
    let view = session
        .debug_direct_memory()
        .ok_or("guest did not enable identity RAM mapping")?;
    let video_view = view.video.ok_or("guest VGA mode has no direct read view")?;
    assert_eq!(&video_view.bytes[0x10..0x14], &SEEDS[0].to_le_bytes());
    assert_eq!(
        video_view
            .planar
            .ok_or("expected VGA plane backing")?
            .chain4,
        matches!(video, Video::Chain4)
    );
    let metadata = Metadata {
        objects: vec![ObjectBinding {
            space: 1,
            linear_base: CODE,
            size: body.bytes.len() as u32 + 1,
            default32: true,
        }],
        regions: body.metadata.clone(),
        context: CodeContext {
            cs: session.registers().segment_cache(SegmentRegister::Cs),
        },
    };
    let state = session.save_state()?;
    let metadata = metadata::encode(&metadata)?;
    let mut bundle = Vec::new();
    for part in [&zip[..], &config[..], &state, &metadata] {
        bundle.extend_from_slice(&u32::try_from(part.len())?.to_le_bytes());
    }
    for part in [&zip[..], &config[..], &state, &metadata] {
        bundle.extend_from_slice(part);
    }
    Ok(bundle)
}

fn output(
    runner: &mut doslet_wasm_aot::shared_runtime::SharedRunner,
    handle: u32,
    kind: u32,
) -> Result<Vec<u8>> {
    let packed = runner.call_runtime::<(u32, u32), u64>("static_probe_output", (handle, kind))?;
    Ok(runner
        .memory_view(packed as u32, (packed >> 32) as u32)?
        .to_vec())
}

fn verify(
    video: Video,
    stack_address: StackAddress,
    write_mode: Option<u8>,
    runtime: &[u8],
) -> Result<()> {
    let mut body = body(video);
    if write_mode.is_some() {
        // Replace only the zero-work terminal stub; existing CALL/RET targets
        // now enter this suffix. All preceding direct accesses remain intact.
        body.regions.pop();
        body.metadata.pop();
        for (prefix, width, source, address) in [
            (
                &[0xa3][..],
                Width::Dword,
                Source::Register(Register::Eax),
                0xa0041u32,
            ),
            (
                &[0x66, 0x89, 0x15][..],
                Width::Word,
                Source::Register(Register::Dx),
                0xa0047,
            ),
            (
                &[0x88, 0x25][..],
                Width::Byte,
                Source::Register(Register::Ah),
                0xa004b,
            ),
            (
                &[0xa3][..],
                Width::Dword,
                Source::Register(Register::Eax),
                0x480,
            ),
        ] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&address.to_le_bytes());
            body.instruction(
                &bytes,
                Operation::StoreMemory {
                    address: self::address(None, address),
                    width,
                    source,
                },
            );
        }
        // A same-value store to a protected compiled source page must use the
        // bus helper and update its generation before the exact budget exit.
        let mut bytes = vec![0xc6, 0x05];
        bytes.extend_from_slice(&CODE.to_le_bytes());
        bytes.push(body.bytes[0]);
        body.instruction(
            &bytes,
            Operation::StoreMemory {
                address: address(None, CODE),
                width: Width::Byte,
                source: Source::Immediate(u32::from(body.bytes[0])),
            },
        );
        body.region(
            &[],
            vec![],
            Terminator::Fallback(Address {
                space: 1,
                offset: body.bytes.len() as u32,
            }),
        );
    }
    let count = body
        .regions
        .iter()
        .map(Region::completed_instructions)
        .sum::<usize>() as u32;
    let bundle = fixture(video, stack_address, write_mode, &body)?;
    let options = CompileOptions {
        direct_memory: true,
        inline_admission: true,
        memory_write_helper: write_mode.is_some(),
        // The complete acyclic fixture returns at its exact soft budget first.
        service_interval_regions: count + 1,
        ..CompileOptions::default()
    };
    let aliases: Vec<_> = body
        .regions
        .iter()
        .filter(|region| region.completed_instructions() != 0)
        .map(|region| doslet_x86_wasm::static_program::EntryAlias {
            address: Address {
                space: 0,
                offset: CODE + region.entry.offset,
            },
            target: region.id,
        })
        .collect();
    let program = Program {
        regions: body.regions,
    }
    .compile_with_aliases(&options, &aliases)?;
    let artifacts = SharedArtifacts::compile(runtime, program.wasm())?;
    let mut runner = artifacts.instantiate()?;
    let len = u32::try_from(bundle.len())?;
    let input = runner.call_runtime::<u32, u32>("static_probe_input", len)?;
    runner.write_bytes(input, &bundle)?;
    let handle = runner.call_runtime::<(u32, u32), u32>("static_probe_new", (input, len))?;
    let frame = runner.call_runtime::<u32, u32>("static_probe_frame", handle)?;
    runner.call_runtime::<(u32, u64), ()>("static_probe_begin", (handle, u64::from(count)))?;
    let descriptor = runner.memory_view(frame + memory::OFFSET, memory::BYTES as u32)?;
    let word = |offset: u32| {
        u32::from_le_bytes(
            descriptor[offset as usize..offset as usize + 4]
                .try_into()
                .unwrap(),
        )
    };
    assert_eq!(
        word(memory::DEVICE_KIND),
        memory::DeviceReadKind::FourLanes as u32
    );
    assert_eq!(
        word(memory::DEVICE_CHAIN4),
        u32::from(matches!(video, Video::Chain4))
    );
    assert_eq!(
        word(memory::DEVICE_COMPARE_ENABLED),
        u32::from(matches!(video, Video::Compare))
    );
    let generations = word(memory::GENERATIONS_POINTER);
    assert_eq!(word(memory::STACK_ADDRESS), stack_address as u32);
    let stack_page = if stack_address == StackAddress::Bits32 {
        0x3e
    } else {
        0xe
    };
    let stack_generation = u64::from_le_bytes(
        runner
            .memory_view(generations + stack_page * 8, 8)?
            .try_into()
            .unwrap(),
    );
    let initial_generations: Vec<_> = (0x21..=0x24)
        .map(|page| {
            Ok(u64::from_le_bytes(
                runner
                    .memory_view(generations + page * 8, 8)?
                    .try_into()
                    .unwrap(),
            ))
        })
        .collect::<Result<_>>()?;
    assert_eq!(runner.run(frame)?, Exit::Budget);
    let completed = runner.read_frame(frame)?;
    assert_eq!(completed.charged_work, u64::from(count));
    assert_eq!(completed.remaining_budget, 0);
    assert_eq!(
        completed.address,
        Address {
            space: 1,
            offset: body.bytes.len() as u32
        }
    );
    let effects =
        memory::Effects::read(runner.memory_view(frame + memory::OFFSET, memory::BYTES as u32)?)?;
    assert_eq!(
        effects.device_reads,
        if write_mode.is_some() { 0 } else { 12 },
        "VGA reads use each operation's source width"
    );
    assert_eq!(
        effects.ram_writes,
        if write_mode.is_some() { 0 } else { 18 },
        "direct writes are committed by a helper"
    );
    assert_eq!(
        u64::from_le_bytes(
            runner
                .memory_view(generations + stack_page * 8, 8)?
                .try_into()
                .unwrap()
        ),
        stack_generation + 7,
        "five PUSH instructions and two CALLs update the stack page"
    );
    assert_eq!(
        effects.inline_reserved,
        if write_mode.is_some() { 0 } else { count },
        "helpers commit reservations without charging an interpreter step"
    );
    let last_group = if matches!(video, Video::Chain4) {
        SEEDS[0]
    } else {
        SEEDS[2]
    };
    assert_eq!(
        effects.latches,
        last_group.to_le_bytes(),
        "last VGA read updates all four latches"
    );
    for (index, increment) in [1, 2, 5, 3].into_iter().enumerate() {
        let page = 0x21 + index as u32;
        let actual = u64::from_le_bytes(
            runner
                .memory_view(generations + page * 8, 8)?
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            actual,
            initial_generations[index] + increment,
            "page {page:x} generation"
        );
    }
    runner.call_runtime::<u32, ()>("static_probe_finish", handle)?;
    let stats: Value = serde_json::from_slice(&output(&mut runner, handle, 3)?)?;
    assert_eq!(stats["translated_steps"], count);
    assert_eq!(stats["interpreted_steps"], 0);
    assert_eq!(
        stats["callbacks"], 0,
        "no per-access or structural service callbacks"
    );
    assert_eq!(stats["guard_refusals"], 0);
    assert_eq!(
        stats["memory_write_helpers"],
        if write_mode.is_some() { 5 } else { 0 }
    );
    assert_eq!(stats["fault"], Value::Null);
    let actual = output(&mut runner, handle, 0)?;
    // Interpreter in the same actual Rust WASM runtime, at identical boundaries.
    runner.call_runtime::<(u32, u32), ()>("static_probe_reset", (handle, 0))?;
    assert_eq!(
        runner.call_runtime::<(u32, u32), u32>("static_probe_reference", (handle, count))?,
        count
    );
    let expected = output(&mut runner, handle, 0)?;
    assert!(
        actual == expected,
        "{video:?}: complete state differs from WASM interpreter; first difference {:?}",
        actual.iter().zip(&expected).position(|(a, b)| a != b)
    );
    // The native interpreter is an additional independent target comparison.
    let mut native = Probe::new(&bundle)?;
    assert_eq!(native.run_reference(count)?, count);
    native.prepare_output(0)?;
    assert!(
        actual == native.output(),
        "{video:?}: complete state differs from native interpreter"
    );
    let mut stored = [0; 4];
    assert!(native.session().debug_read_memory(0x21fff, &mut stored));
    let expected_video = match video {
        Video::Chain4 => [0xb2, 0xc3, 0xd4, 0x5e],
        Video::Plane2 => [0x70, 0x33, 0x97, 0x6a],
        Video::Compare => [0x0e, 0x00, 0x20, 0x00],
    };
    assert_eq!(
        stored, expected_video,
        "known guest VGA data must reach RAM"
    );
    runner.call_runtime::<u32, ()>("static_probe_free", handle)?;
    println!(
        "{video:?}/{stack_address:?}/write={write_mode:?}: {count} translated instructions; zero callbacks; full WASM/native interpreter state exact"
    );
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        "target/wasm32-unknown-unknown/release/doslet_static_game_probe.wasm".into()
    });
    if args.next().is_some() {
        return Err("usage: check_static_memory [probe.wasm]".into());
    }
    let mut runtime = Vec::new();
    fs::File::open(path)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut runtime)?;
    if runtime.len() > 64 * 1024 * 1024 {
        return Err("probe exceeds 64 MiB input bound".into());
    }
    for video in [Video::Chain4, Video::Plane2, Video::Compare] {
        for stack_address in [StackAddress::Bits16, StackAddress::Bits32] {
            verify(video, stack_address, None, &runtime)?;
            for mode in 0..=3 {
                verify(video, stack_address, Some(mode), &runtime)?;
            }
        }
    }
    Ok(())
}
