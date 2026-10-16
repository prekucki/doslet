//! US keyboard positions plus the existing BIOS scan/ASCII convention.
#[derive(Clone, Copy, Debug)]
pub struct Stroke {
    pub usage: u16,
    pub scan: u8,
    pub ascii: u8,
    pub modifiers: u8,
}
pub fn key_stroke(key: &str, modifiers: &[String]) -> Result<Stroke, String> {
    let mut stroke = if key.len() == 1 && key.is_ascii() {
        character(key.as_bytes()[0])?
    } else {
        let (usage, scan, ascii) = match key.to_ascii_lowercase().as_str() {
            "enter" | "return" => (40, 0x1c, 13),
            "escape" | "esc" => (41, 1, 27),
            "backspace" => (42, 0x0e, 8),
            "tab" => (43, 0x0f, 9),
            "space" => (44, 0x39, 32),
            "up" | "arrowup" => (82, 0x48, 0),
            "down" | "arrowdown" => (81, 0x50, 0),
            "left" | "arrowleft" => (80, 0x4b, 0),
            "right" | "arrowright" => (79, 0x4d, 0),
            "home" => (74, 0x47, 0),
            "end" => (77, 0x4f, 0),
            "pageup" => (75, 0x49, 0),
            "pagedown" => (78, 0x51, 0),
            "insert" => (73, 0x52, 0),
            "delete" => (76, 0x53, 0),
            value if value.starts_with('f') => {
                let n = value[1..].parse::<u8>().map_err(|_| "Unknown key")?;
                if !(1..=10).contains(&n) {
                    return Err("Only F1..F10 are supported".into());
                }
                (u16::from(n) + 57, n + 0x3a, 0)
            }
            _ => return Err(format!("Unknown key: {key}")),
        };
        Stroke {
            usage,
            scan,
            ascii,
            modifiers: 0,
        }
    };
    for modifier in modifiers {
        stroke.modifiers |= match modifier.to_ascii_lowercase().as_str() {
            "shift" => 3,
            "ctrl" | "control" => 4,
            "alt" => 8,
            _ => return Err(format!("Unknown modifier: {modifier}")),
        };
    }
    if stroke.modifiers & 8 != 0 {
        stroke.ascii = 0;
    } else if stroke.modifiers & 4 != 0 && stroke.ascii.is_ascii_alphabetic() {
        stroke.ascii = stroke.ascii.to_ascii_uppercase() - b'@';
    } else if stroke.modifiers & 3 != 0 {
        if stroke.usage == 43 {
            stroke.ascii = 0;
        } else if stroke.ascii.is_ascii_lowercase() {
            stroke.ascii = stroke.ascii.to_ascii_uppercase();
        }
    }
    Ok(stroke)
}
pub fn text_strokes(text: &str) -> Result<Vec<Stroke>, String> {
    if text.is_empty() || text.len() > 64 || !text.is_ascii() {
        return Err("text must contain 1..64 ASCII characters".into());
    }
    text.bytes().map(character).collect()
}
fn character(byte: u8) -> Result<Stroke, String> {
    if matches!(byte, b'\n' | b'\r' | b'\t') {
        return key_stroke(if byte == b'\t' { "Tab" } else { "Enter" }, &[]);
    }
    if !(32..=126).contains(&byte) {
        return Err("Unsupported control character".into());
    }
    let mut base = byte.to_ascii_lowercase();
    let mut modifiers = if byte.is_ascii_uppercase() { 3 } else { 0 };
    const SHIFTED: &[u8] = b"!@#$%^&*()_+{}|:\"~<>?";
    const BASE: &[u8] = b"1234567890-=[]\\;'`,./";
    if let Some(index) = SHIFTED.iter().position(|b| *b == byte) {
        base = BASE[index];
        modifiers = 3;
    }
    let usage = match base {
        b'a'..=b'z' => u16::from(base - b'a') + 4,
        b'1'..=b'9' => u16::from(base - b'1') + 30,
        b'0' => 39,
        b' ' => 44,
        b'-' => 45,
        b'=' => 46,
        b'[' => 47,
        b']' => 48,
        b'\\' => 49,
        b';' => 51,
        b'\'' => 52,
        b'`' => 53,
        b',' => 54,
        b'.' => 55,
        b'/' => 56,
        _ => return Err("Unsupported ASCII character".into()),
    };
    let mut scan = 0;
    for (row, start) in [
        (&b"qwertyuiop[]"[..], 0x10),
        (&b"asdfghjkl;'"[..], 0x1e),
        (&b"zxcvbnm,./"[..], 0x2c),
    ] {
        if let Some(index) = row.iter().position(|b| *b == base) {
            scan = start + index as u8;
        }
    }
    if scan == 0 {
        scan = match base {
            b'1'..=b'9' => base - b'1' + 2,
            b'0' => 0x0b,
            b'-' => 0x0c,
            b'=' => 0x0d,
            b'`' => 0x29,
            b'\\' => 0x2b,
            b' ' => 0x39,
            _ => return Err("Unsupported scan code".into()),
        };
    }
    Ok(Stroke {
        usage,
        scan,
        ascii: byte,
        modifiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn printable_ascii_has_matching_us_positions_and_bios_scans() {
        for byte in 32..=126 {
            let stroke = character(byte).unwrap();
            assert!((4..=56).contains(&stroke.usage));
            assert_ne!(stroke.scan, 0);
            assert_eq!(stroke.ascii, byte);
        }
        for (key, usage, scan) in [("!", 30, 2), ("{", 47, 0x1a), ("\"", 52, 0x28)] {
            let stroke = key_stroke(key, &[]).unwrap();
            assert_eq!(
                (stroke.usage, stroke.scan, stroke.modifiers),
                (usage, scan, 3)
            );
        }
        assert_eq!(key_stroke("c", &["ctrl".into()]).unwrap().ascii, 3);
        assert!(text_strokes("bad\u{1b}").is_err());
        assert!(text_strokes("zażółć").is_err());
        assert!(text_strokes(&"a".repeat(65)).is_err());
    }
}
