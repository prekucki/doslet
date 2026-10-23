//! Host keys become BIOS scan-code/ASCII pairs; the machine stays in its owner.
use iced::keyboard::{Event, Key, Modifiers, key::Named};

/// Opt-in native input diagnostics; no formatting or environment reads per
/// event after the disabled flag has been cached.
#[cfg(not(target_arch = "wasm32"))]
pub fn trace(
    event: &Event,
    captured: bool,
    enabled: bool,
    mode: doslet_config::KeyboardMode,
    v86: bool,
) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("DOSLET_TRACE_KEYS").is_some()) {
        eprintln!(
            "[INFO doslet] keyboard: captured={captured} enabled={enabled} mode={mode:?} v86={v86} routed={:?} event={event:?}",
            physical(event)
        );
    }
}

/// Translate the host's physical location into USB HID usages, including releases.
pub fn physical(event: &Event) -> Option<(u16, bool)> {
    use iced::keyboard::key::{Code, Physical};
    let (physical, pressed) = match event {
        Event::KeyPressed { repeat: true, .. } => return None,
        Event::KeyPressed { physical_key, .. } => (physical_key, true),
        Event::KeyReleased { physical_key, .. } => (physical_key, false),
        Event::ModifiersChanged(_) => return None,
    };
    let Physical::Code(code) = physical else {
        return None;
    };
    let usage = match code {
        Code::KeyA => 4,
        Code::KeyB => 5,
        Code::KeyC => 6,
        Code::KeyD => 7,
        Code::KeyE => 8,
        Code::KeyF => 9,
        Code::KeyG => 10,
        Code::KeyH => 11,
        Code::KeyI => 12,
        Code::KeyJ => 13,
        Code::KeyK => 14,
        Code::KeyL => 15,
        Code::KeyM => 16,
        Code::KeyN => 17,
        Code::KeyO => 18,
        Code::KeyP => 19,
        Code::KeyQ => 20,
        Code::KeyR => 21,
        Code::KeyS => 22,
        Code::KeyT => 23,
        Code::KeyU => 24,
        Code::KeyV => 25,
        Code::KeyW => 26,
        Code::KeyX => 27,
        Code::KeyY => 28,
        Code::KeyZ => 29,
        Code::Digit1 => 30,
        Code::Digit2 => 31,
        Code::Digit3 => 32,
        Code::Digit4 => 33,
        Code::Digit5 => 34,
        Code::Digit6 => 35,
        Code::Digit7 => 36,
        Code::Digit8 => 37,
        Code::Digit9 => 38,
        Code::Digit0 => 39,
        Code::Enter => 40,
        Code::Escape => 41,
        Code::Backspace => 42,
        Code::Tab => 43,
        Code::Space => 44,
        Code::Minus => 45,
        Code::Equal => 46,
        Code::BracketLeft => 47,
        Code::BracketRight => 48,
        Code::Backslash => 49,
        Code::Semicolon => 51,
        Code::Quote => 52,
        Code::Backquote => 53,
        Code::Comma => 54,
        Code::Period => 55,
        Code::Slash => 56,
        Code::CapsLock => 57,
        Code::F1 => 58,
        Code::F2 => 59,
        Code::F3 => 60,
        Code::F4 => 61,
        Code::F5 => 62,
        Code::F6 => 63,
        Code::F7 => 64,
        Code::F8 => 65,
        Code::F9 => 66,
        Code::F10 => 67,
        Code::F11 => 68,
        Code::F12 => 69,
        Code::PrintScreen => 70,
        Code::ScrollLock => 71,
        Code::Pause => 72,
        Code::Insert => 73,
        Code::Home => 74,
        Code::PageUp => 75,
        Code::Delete => 76,
        Code::End => 77,
        Code::PageDown => 78,
        Code::ArrowRight => 79,
        Code::ArrowLeft => 80,
        Code::ArrowDown => 81,
        Code::ArrowUp => 82,
        Code::NumLock => 83,
        Code::NumpadDivide => 84,
        Code::NumpadMultiply => 85,
        Code::NumpadSubtract => 86,
        Code::NumpadAdd => 87,
        Code::NumpadEnter => 88,
        Code::Numpad1 => 89,
        Code::Numpad2 => 90,
        Code::Numpad3 => 91,
        Code::Numpad4 => 92,
        Code::Numpad5 => 93,
        Code::Numpad6 => 94,
        Code::Numpad7 => 95,
        Code::Numpad8 => 96,
        Code::Numpad9 => 97,
        Code::Numpad0 => 98,
        Code::NumpadDecimal => 99,
        Code::ControlLeft => 224,
        Code::ShiftLeft => 225,
        Code::AltLeft => 226,
        Code::SuperLeft => 227,
        Code::ControlRight => 228,
        Code::ShiftRight => 229,
        Code::AltRight => 230,
        Code::SuperRight => 231,
        _ => return None,
    };
    Some((usage, pressed))
}

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
                // Option on macOS can produce a non-ASCII character or a dead
                // key. DOS Alt shortcuts use the base key and no ASCII byte.
                _ if modifiers.alt() => Some(0),
                Key::Character(value) if value.len() == 1 => value.as_bytes().first().copied(),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alt_shortcuts_ignore_option_characters_and_dead_keys() {
        use iced::keyboard::{
            Location,
            key::{Code, Physical},
        };
        for modified_key in [Key::Character("ø".into()), Key::Unidentified] {
            let press = Event::KeyPressed {
                key: Key::Character("o".into()),
                modified_key,
                physical_key: Physical::Code(Code::KeyO),
                location: Location::Standard,
                modifiers: Modifiers::ALT,
                text: None,
                repeat: false,
            };
            assert_eq!(translate(&press), (8, Some((0x18, 0))));
            assert_eq!(physical(&press), Some((18, true)));
        }
    }

    #[test]
    fn hardware_input_uses_position_and_keeps_release_edges() {
        use iced::keyboard::{
            Location,
            key::{Code, Physical},
        };
        let press = Event::KeyPressed {
            key: Key::Character("z".into()),
            modified_key: Key::Character("Z".into()),
            physical_key: Physical::Code(Code::KeyY),
            location: Location::Standard,
            modifiers: Modifiers::SHIFT,
            text: Some("Z".into()),
            repeat: false,
        };
        assert_eq!(physical(&press), Some((28, true)));
        let release = Event::KeyReleased {
            key: Key::Character("z".into()),
            modified_key: Key::Character("Z".into()),
            physical_key: Physical::Code(Code::KeyY),
            location: Location::Standard,
            modifiers: Modifiers::SHIFT,
        };
        assert_eq!(physical(&release), Some((28, false)));
        let mut repeated = press;
        if let Event::KeyPressed { repeat, .. } = &mut repeated {
            *repeat = true;
        }
        assert_eq!(physical(&repeated), None);
        assert_eq!(translate(&repeated).1, Some((0x2c, b'Z')));
    }
}
