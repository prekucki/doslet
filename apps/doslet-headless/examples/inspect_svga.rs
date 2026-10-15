//! Read-only driver inventory and bounded, instruction-by-instruction observation.
//! Diagnostic host work is outside Session::advance; this is not a benchmark.
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus};
use doslet_x86::{Access, Gpr16, Gpr32, SegmentRegister};
use std::{collections::BTreeMap, error::Error, fs, path::Path};

const MAGIC: [u8; 8] = [0x78, 0x56, 0x34, 0x12, 0x21, 0x43, 0x65, 0x87];
const SLOT_COUNT: usize = 12;
const HEADER_LEN: usize = 0x54;

struct Driver {
    name: String,
    bytes: Vec<u8>,
    entries: [u32; SLOT_COUNT],
}
struct Mapping {
    driver: usize,
    base: u32,
    steps: u64,
    calls: [u64; SLOT_COUNT],
}
struct ReturnSite {
    cs: u16,
    ip: u32,
    sp: u32,
    slot: usize,
    driver: usize,
    step: u64,
}

fn word(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
fn entries(bytes: &[u8]) -> Option<[u32; SLOT_COUNT]> {
    if bytes.get(..8)? != MAGIC || word(bytes, 0x14)? != 640 {
        return None;
    }
    let mut result = [0; SLOT_COUNT];
    for (index, entry) in result.iter_mut().enumerate() {
        *entry = word(bytes, 0x1c + index * 4)?;
        if *entry != 0 && (*entry < HEADER_LEN as u32 || *entry as usize >= bytes.len()) {
            return None;
        }
    }
    Some(result)
}
fn physical(session: &Session, address: u32) -> usize {
    (if session.a20_enabled() {
        address
    } else {
        address & !0x10_0000
    }) as usize
}
fn read(session: &Session, address: u32, length: usize) -> Option<&[u8]> {
    let first = physical(session, address);
    // Do not manufacture contiguous views across an A20 wrap or mapped devices.
    let last = address.checked_add(u32::try_from(length.checked_sub(1)?).ok()?)?;
    if physical(session, last) != first.checked_add(length - 1)? {
        return None;
    }
    session.ram_view(first..first.checked_add(length)?)
}
fn path_at_dx(session: &Session) -> Option<String> {
    let r = session.registers();
    let mut name = Vec::with_capacity(128);
    for index in 0..128 {
        let offset = r.get(Gpr16::Dx).wrapping_add(index) as u32;
        let address = r
            .address(SegmentRegister::Ds, offset, 1, Access::Read)
            .ok()?;
        let byte = *read(session, address, 1)?.first()?;
        if byte == 0 {
            return String::from_utf8(name).ok();
        }
        name.push(byte);
    }
    None
}
fn identify(session: &Session, base: u32, drivers: &[Driver]) -> Option<usize> {
    let header = read(session, base, HEADER_LEN)?;
    if header[..8] != MAGIC || word(header, 0x14)? != 640 {
        return None;
    }
    // Header selectors and cached bank are writable; identify by the complete
    // original code body and the unmodified relative entry table, not magic alone.
    drivers.iter().position(|driver| {
        read(session, base, driver.bytes.len()).is_some_and(|bytes| {
            bytes[0x1c..0x4c] == driver.bytes[0x1c..0x4c]
                && bytes[HEADER_LEN..] == driver.bytes[HEADER_LEN..]
        })
    })
}
fn snapshot(session: &Session) -> String {
    let r = session.registers();
    let registers = [
        Gpr32::Eax,
        Gpr32::Ebx,
        Gpr32::Ecx,
        Gpr32::Edx,
        Gpr32::Esi,
        Gpr32::Edi,
        Gpr32::Esp,
    ];
    let values = registers
        .map(|reg| format!("{reg:?}={:08X}", r.get32(reg)))
        .join(" ");
    let stack_offset = if r.stack32() {
        r.get32(Gpr32::Esp)
    } else {
        r.get(Gpr16::Sp) as u32
    };
    let stack = r
        .address(SegmentRegister::Ss, stack_offset, 32, Access::Read)
        .ok()
        .and_then(|address| read(session, address, 32))
        .map(|bytes| {
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|part| format!("{:08X}", u32::from_le_bytes(*part)))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "unavailable".into());
    format!(
        "{values} CS={:04X} DS={:04X} ES={:04X} SS={:04X} code32={} flags={:08X} stack=[{stack}]",
        r.cs,
        r.ds,
        r.es,
        r.ss,
        r.code32(),
        session.eflags()
    )
}
fn hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |value, byte| {
        (value ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| "PRINCE".into());
    let steps: u64 = args.next().as_deref().unwrap_or("300000000").parse()?;
    let (baseline, accelerated) = match args.next().as_deref() {
        None => (false, false),
        Some("baseline") => (true, false),
        Some("accelerated") => (true, true),
        _ => return Err("third argument must be baseline or accelerated".into()),
    };
    if args.next().is_some() || steps > 1_000_000_000 {
        return Err(
            "usage: inspect_svga GAME_DIRECTORY [STEPS <= 1000000000] [baseline|accelerated]; 0 inventories only".into(),
        );
    }
    let root = Path::new(&root);
    let mut paths = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    paths.sort_by_key(|entry| entry.file_name());
    let mut drivers = Vec::new();
    for entry in paths {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("386"))
            && entry.file_type()?.is_file()
        {
            let bytes = fs::read(&path)?;
            let entries =
                entries(&bytes).ok_or("unrecognized driver header or entry outside file")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            println!(
                "DRIVER {name} length={} hash={:016X} entries={entries:04X?}",
                bytes.len(),
                hash(bytes.iter().copied())
            );
            drivers.push(Driver {
                name,
                bytes,
                entries,
            });
        }
    }
    if steps == 0 {
        return Ok(());
    }
    if drivers.is_empty() {
        return Err("no .386 drivers found".into());
    }
    let mut session = native_input::load_game(&root.join("PRINCE.EXE"))?;
    session.set_vga_wait_acceleration(accelerated);
    session.start();
    let mut mappings: Vec<Mapping> = Vec::with_capacity(32);
    let mut handles = BTreeMap::<u16, String>::new();
    let mut previous_cs = None;
    let mut selected = None;
    let mut midi = [0; 1024];
    let mut pcm = [[0; 2]; 1024];
    let mut pcm_frames = 0;
    let mut pcm_hash = 0xcbf29ce484222325u64;
    let mut midi_bytes = 0;
    let mut midi_hash = 0xcbf29ce484222325u64;
    let mut poll_counts = BTreeMap::<u32, u64>::new();
    let mut vbe_calls = [0u64; 256];
    let mut driver_steps = 0;
    let mut returns = Vec::<ReturnSite>::with_capacity(24);
    while session.steps() < steps && session.status() == SessionStatus::Running {
        let r = session.registers();
        let cs = r.segment_cache(SegmentRegister::Cs);
        let pc = cs.base.wrapping_add(r.ip);
        let mut index = 0;
        while index < returns.len() {
            let site = &returns[index];
            if r.cs == site.cs
                && r.ip == site.ip
                && (site.sp..=site.sp.wrapping_add(32)).contains(&r.get32(Gpr32::Esp))
            {
                println!(
                    "RETURN {} slot={:02X} consumed_steps={} callee_cleanup={} {}",
                    drivers[site.driver].name,
                    0x1c + site.slot * 4,
                    session.steps() - site.step,
                    r.get32(Gpr32::Esp) - site.sp,
                    snapshot(&session)
                );
                returns.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if !baseline && previous_cs != Some((r.cs, cs.base)) {
            previous_cs = Some((r.cs, cs.base));
            selected = mappings.iter().position(|mapping| mapping.base == cs.base);
            if selected.is_none()
                && let Some(driver) = identify(&session, cs.base, &drivers)
            {
                if mappings.len() == 32 {
                    return Err("diagnostic driver mapping capacity exhausted".into());
                }
                println!(
                    "EXECUTED {} base={:08X} step={} {}",
                    drivers[driver].name,
                    cs.base,
                    session.steps(),
                    snapshot(&session)
                );
                mappings.push(Mapping {
                    driver,
                    base: cs.base,
                    steps: 0,
                    calls: [0; SLOT_COUNT],
                });
                selected = Some(mappings.len() - 1);
            }
        }
        if let Some(index) = selected {
            let mapping = &mut mappings[index];
            if r.ip < drivers[mapping.driver].bytes.len() as u32 {
                mapping.steps += 1;
                driver_steps += 1;
                for (slot, entry) in drivers[mapping.driver].entries.iter().enumerate() {
                    if *entry != 0 && r.ip == *entry {
                        mapping.calls[slot] += 1;
                        if mapping.calls[slot] <= 2 {
                            println!(
                                "ENTRY {} slot={:02X} offset={entry:04X} step={} {}",
                                drivers[mapping.driver].name,
                                0x1c + slot * 4,
                                session.steps(),
                                snapshot(&session)
                            );
                            if r.code32()
                                && r.stack32()
                                && let Ok(address) = r.address(
                                    SegmentRegister::Ss,
                                    r.get32(Gpr32::Esp),
                                    8,
                                    Access::Read,
                                )
                                && let Some(stack) = read(&session, address, 8)
                            {
                                if returns.len() == 24 {
                                    return Err("return-site capacity exhausted".into());
                                }
                                returns.push(ReturnSite {
                                    cs: word(stack, 4).unwrap() as u16,
                                    ip: word(stack, 0).unwrap(),
                                    sp: r.get32(Gpr32::Esp).wrapping_add(8),
                                    slot,
                                    driver: mapping.driver,
                                    step: session.steps(),
                                });
                            }
                        }
                    }
                }
            }
        }
        if !baseline && r.get(Gpr16::Dx) == 0x3da && read(&session, pc, 1) == Some(&[0xec][..]) {
            if poll_counts.len() == 64 && !poll_counts.contains_key(&pc) {
                return Err("poll site capacity exhausted".into());
            }
            let count = poll_counts.entry(pc).or_default();
            if *count == 0 {
                println!("POLL_CODE pc={pc:08X} bytes={:02X?}", read(&session, pc, 8));
            }
            *count += 1;
        }
        let shim = !baseline && session.control_registers().cr0 & 1 == 0 && r.cs == 0xf000;
        if shim && r.ip == 0x10 && r.get(Gpr16::Ax) >> 8 == 0x4f {
            let function = r.get_byte(0) as usize;
            vbe_calls[function] += 1;
            if vbe_calls[function] <= 2 {
                println!(
                    "VBE ax={:04X} step={} {}",
                    r.get(Gpr16::Ax),
                    session.steps(),
                    snapshot(&session)
                );
            }
        }
        let dos = shim && r.ip == 0x21;
        let open = if dos && r.get_byte(4) == 0x3d {
            path_at_dx(&session)
        } else {
            None
        };
        let read_driver = if dos && r.get_byte(4) == 0x3f {
            handles.get(&r.get(Gpr16::Bx)).and_then(|name| {
                if name.to_ascii_uppercase().ends_with(".386") {
                    r.address(
                        SegmentRegister::Ds,
                        r.get(Gpr16::Dx) as u32,
                        1,
                        Access::Write,
                    )
                    .ok()
                    .map(|address| (name.clone(), address))
                } else {
                    None
                }
            })
        } else {
            None
        };
        let close = if dos && r.get_byte(4) == 0x3e {
            Some(r.get(Gpr16::Bx))
        } else {
            None
        };
        let budget = if baseline {
            (steps - session.steps()).min(10_000) as u32
        } else {
            1
        };
        if session.advance(budget) == 0 {
            break;
        }
        if let Some(name) = open {
            if session.eflags() & 1 == 0 {
                let handle = session.registers().get(Gpr16::Ax);
                println!("OPEN step={} handle={handle} name={name}", session.steps());
                handles.insert(handle, name);
            } else {
                println!(
                    "OPEN_FAILED name={name} ax={:04X}",
                    session.registers().get(Gpr16::Ax)
                );
            }
        }
        if let Some((name, address)) = read_driver {
            println!(
                "READ_DRIVER {name} address={address:08X} returned={} step={}",
                session.registers().get(Gpr16::Ax),
                session.steps()
            );
        }
        if let Some(handle) = close
            && session.eflags() & 1 == 0
        {
            handles.remove(&handle);
        }
        if baseline || session.steps() % 10_000 == 0 {
            loop {
                let length = session.drain_pcm(&mut pcm);
                pcm_frames += length;
                for byte in pcm[..length]
                    .iter()
                    .flat_map(|frame| frame.iter())
                    .flat_map(|sample| sample.to_le_bytes())
                {
                    pcm_hash = (pcm_hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
                if length == 0 {
                    break;
                }
            }
            let length = session.drain_midi(&mut midi);
            midi_bytes += length;
            for byte in &midi[..length] {
                midi_hash = (midi_hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    let length = session.drain_midi(&mut midi);
    midi_bytes += length;
    for byte in &midi[..length] {
        midi_hash = (midi_hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    println!(
        "RESULT status={:?} steps={} guest_nanos={} driver_steps={driver_steps} midi_bytes={midi_bytes} midi_hash={midi_hash:016X} drained_pcm_frames={pcm_frames} pcm_hash={pcm_hash:016X}",
        session.status(),
        session.steps(),
        session.emulated_nanos()
    );
    println!(
        "FINAL_STATE ip={:08X} {}",
        session.registers().ip,
        snapshot(&session)
    );
    for mapping in mappings {
        println!(
            "MAPPING {} base={:08X} steps={} calls={:?}",
            drivers[mapping.driver].name, mapping.base, mapping.steps, mapping.calls
        );
        if let Some(header) = read(&session, mapping.base, HEADER_LEN) {
            println!(
                "LIVE_HEADER bank={} video_selectors={:08X} screen_offset={:08X} data_selector={:08X}",
                header[0x0a],
                word(header, 0x0c).unwrap(),
                word(header, 0x10).unwrap(),
                word(header, 0x18).unwrap()
            );
        }
    }
    for (pc, count) in poll_counts {
        println!("VGA_POLL pc={pc:08X} reads={count}");
    }
    for (function, count) in vbe_calls.iter().enumerate() {
        if *count != 0 {
            println!("VBE_COUNT function={function:02X} calls={count}");
        }
    }
    if let Some(frame) = session.frame() {
        println!(
            "FRAME {}x{} revision={} pixels_hash={:016X} rgba_hash={:016X}",
            frame.width,
            frame.height,
            frame.revision,
            hash(frame.pixels.iter().copied()),
            hash(
                frame
                    .pixels
                    .iter()
                    .flat_map(|pixel| frame.palette[*pixel as usize])
            )
        );
    }
    if let Some(fault) = session.fault() {
        return Err(fault.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_header_rejects_truncation_wrong_stride_and_outside_entries() {
        let mut bytes = [0; 128];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[0x14..0x18].copy_from_slice(&640u32.to_le_bytes());
        bytes[0x1c..0x20].copy_from_slice(&84u32.to_le_bytes());
        assert_eq!(entries(&bytes).unwrap()[0], 84);
        assert!(entries(&bytes[..70]).is_none());
        bytes[0x14] = 0;
        assert!(entries(&bytes).is_none());
        bytes[0x14..0x18].copy_from_slice(&640u32.to_le_bytes());
        bytes[0x1c..0x20].copy_from_slice(&128u32.to_le_bytes());
        assert!(entries(&bytes).is_none());
    }
}
