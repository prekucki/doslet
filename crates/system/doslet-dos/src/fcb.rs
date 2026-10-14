use doslet_bus::{BusError, MemoryBus};
use doslet_x86::real_address;

fn valid(byte: u8) -> bool {
    byte > 31 && !b":.;,=+ \t/\"[]<>|".contains(&byte)
}

/// Bounded ASCII 8.3 FCB parsing. None reports an input longer than 255 bytes.
pub fn parse<B: MemoryBus>(
    bus: &mut B,
    source: (u16, u16),
    destination: (u16, u16),
    options: u8,
) -> Result<Option<(u8, u16)>, BusError> {
    let mut input = [0; 256];
    let mut terminated = false;
    for (index, byte) in input.iter_mut().enumerate() {
        *byte = bus.read_u8(real_address(source.0, source.1.wrapping_add(index as u16)))?;
        if *byte == 0 {
            terminated = true;
            break;
        }
    }
    if !terminated {
        return Ok(None);
    }
    let mut fcb = [b' '; 12];
    for (index, byte) in fcb.iter_mut().enumerate() {
        *byte = bus.read_u8(real_address(
            destination.0,
            destination.1.wrapping_add(index as u16),
        ))?;
    }
    if options & 2 == 0 {
        fcb[0] = 0;
    }
    let mut position = 0;
    while matches!(input[position], b' ' | b'\t') {
        position += 1;
    }
    if options & 1 != 0 && b":;,=+".contains(&input[position]) {
        position += 1;
    }
    while matches!(input[position], b' ' | b'\t') {
        position += 1;
    }
    let mut status = 0;
    if input.get(position + 1) == Some(&b':') {
        let drive = input[position].to_ascii_uppercase();
        fcb[0] = drive.wrapping_sub(b'A').wrapping_add(1);
        if drive != b'C' {
            status = 0xFF;
        }
        position += 2;
    }
    for (start, width, preserve) in [(1, 8, 4), (9, 3, 8)] {
        let present = valid(input[position]);
        if present || options & preserve == 0 {
            fcb[start..start + width].fill(b' ');
        }
        if present {
            let mut length = 0;
            let mut wildcard = false;
            while valid(input[position]) {
                let byte = input[position].to_ascii_uppercase();
                wildcard |= byte == b'*';
                if length < width {
                    let byte = if wildcard { b'?' } else { byte };
                    fcb[start + length] = byte;
                    if byte == b'?' && status == 0 {
                        status = 1;
                    }
                    length += 1;
                }
                position += 1;
            }
            if wildcard {
                fcb[start + length..start + width].fill(b'?');
            }
        }
        if start == 1 && input[position] == b'.' {
            position += 1;
        } else if start == 1 {
            if options & 8 == 0 {
                fcb[9..].fill(b' ');
            }
            break;
        }
    }
    for (index, byte) in fcb.iter().enumerate() {
        bus.write_u8(
            real_address(destination.0, destination.1.wrapping_add(index as u16)),
            *byte,
        )?;
    }
    // DOS clears the current block and record size when parsing an FCB.
    for index in 12..16 {
        bus.write_u8(
            real_address(destination.0, destination.1.wrapping_add(index)),
            0,
        )?;
    }
    Ok(Some((status, position as u16)))
}
