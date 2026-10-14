//! Host keys become BIOS scan-code/ASCII pairs; the machine stays in its owner.
use iced::keyboard::{Event, Key, Modifiers, key::Named};

pub fn translate(event: &Event) -> (u8, Option<(u8, u8)>) {
    let modifiers = match event {
        Event::KeyPressed { modifiers, .. } | Event::KeyReleased { modifiers, .. } => *modifiers,
        Event::ModifiersChanged(modifiers) => *modifiers,
    };
    let flags = modifier_flags(modifiers);
    let Event::KeyPressed {
        key, modified_key, ..
    } = event
    else {
        return (flags, None);
    };
    let pair = match key {
        Key::Named(named) => match named {
            Named::Enter => Some((0x1C, 13)),
            Named::Escape => Some((1, 27)),
            Named::Backspace => Some((0x0E, 8)),
            Named::Tab => Some((0x0F, if modifiers.shift() { 0 } else { 9 })),
            Named::Space => Some((0x39, 32)),
            Named::ArrowUp => Some((0x48, 0)),
            Named::ArrowDown => Some((0x50, 0)),
            Named::ArrowLeft => Some((0x4B, 0)),
            Named::ArrowRight => Some((0x4D, 0)),
            Named::Home => Some((0x47, 0)),
            Named::End => Some((0x4F, 0)),
            Named::PageUp => Some((0x49, 0)),
            Named::PageDown => Some((0x51, 0)),
            Named::Insert => Some((0x52, 0)),
            Named::Delete => Some((0x53, 0)),
            Named::F1 => Some((0x3B, 0)),
            Named::F2 => Some((0x3C, 0)),
            Named::F3 => Some((0x3D, 0)),
            Named::F4 => Some((0x3E, 0)),
            Named::F5 => Some((0x3F, 0)),
            Named::F6 => Some((0x40, 0)),
            Named::F7 => Some((0x41, 0)),
            Named::F8 => Some((0x42, 0)),
            Named::F9 => Some((0x43, 0)),
            Named::F10 => Some((0x44, 0)),
            _ => None,
        },
        Key::Character(base) => {
            let ascii = match modified_key {
                Key::Character(value) => value.as_bytes().first().copied(),
                _ => None,
            };
            if base.len() != 1 || !base.is_ascii() {
                None
            } else {
                ascii.filter(u8::is_ascii).map(|ascii| {
                    let scan = scan(base.as_bytes()[0].to_ascii_lowercase());
                    let ascii = if modifiers.alt() {
                        0
                    } else if modifiers.control() && ascii.is_ascii_alphabetic() {
                        ascii.to_ascii_uppercase() - b'@'
                    } else {
                        ascii
                    };
                    (scan, ascii)
                })
            }
        }
        _ => None,
    };
    (flags, pair)
}
fn modifier_flags(modifiers: Modifiers) -> u8 {
    (u8::from(modifiers.shift()) * 3)
        | (u8::from(modifiers.control()) * 4)
        | (u8::from(modifiers.alt()) * 8)
}
fn scan(byte: u8) -> u8 {
    const ROWS: [&[u8]; 3] = [b"qwertyuiop[]", b"asdfghjkl;'", b"zxcvbnm,./"];
    for (row, start) in ROWS.into_iter().zip([0x10, 0x1E, 0x2C]) {
        if let Some(index) = row.iter().position(|b| *b == byte) {
            return start + index as u8;
        }
    }
    match byte {
        b'1'..=b'9' => byte - b'1' + 2,
        b'0' => 0x0B,
        b'-' => 0x0C,
        b'=' => 0x0D,
        b'`' => 0x29,
        b'\\' => 0x2B,
        b' ' => 0x39,
        _ => 0,
    }
}
