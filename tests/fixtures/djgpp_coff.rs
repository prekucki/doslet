// Synthetic, redistributable DJGPP v2-style executable: a 2048-byte MZ stub
// followed by an i386 COFF image with .text, .data and .bss, as `stubify`
// lays it out. No DJGPP or game bytes are included; the stub does not run.
extern crate alloc;
use alloc::{vec, vec::Vec};

/// Where the fixture links its sections, like DJGPP's default script.
pub const STUB_BYTES: usize = 2048;
pub const TEXT_VADDR: u32 = 0x10a8;
pub const HEADERS: u32 = 0xa8;

/// A symbol to emit: name, value, one-based section, type and storage class.
pub type FixtureSymbol<'a> = (&'a [u8], u32, i16, u16, u8);

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Virtual address of the data section for a text section of `text_len`.
pub const fn data_vaddr(text_len: usize) -> u32 {
    (TEXT_VADDR + text_len as u32 + 0x1ff) & !0x1ff
}

/// The complete executable. `entry` is an offset in `text`.
pub fn djgpp_exe(
    text: &[u8],
    data: &[u8],
    bss: u32,
    entry: u32,
    symbols: &[FixtureSymbol<'_>],
) -> Vec<u8> {
    let mut file = vec![0; STUB_BYTES];
    file[..2].copy_from_slice(b"MZ");
    put16(&mut file, 4, (STUB_BYTES / 512) as u16); // e_cp; e_cblp stays 0.
    put16(&mut file, 8, 2); // 32-byte header
    put16(&mut file, 12, 0xffff);
    // The real stub keeps this text in its header, so e_lfanew is ASCII.
    file[0x20..0x20 + 39].copy_from_slice(b"stub.h generated from stub.asm by djasm");
    let coff = file.len();
    let data_vaddr = data_vaddr(text.len());
    let text_pointer = HEADERS;
    let data_pointer = (text_pointer + text.len() as u32 + 0x1ff) & !0x1ff;
    let bss_vaddr = (data_vaddr + data.len() as u32 + 3) & !3;
    let symbol_pointer = data_pointer + data.len() as u32;
    file.resize(coff + symbol_pointer as usize, 0);
    // File header.
    put16(&mut file, coff, 0x014c);
    put16(&mut file, coff + 2, 3);
    put32(&mut file, coff + 4, 0x3130_3032);
    put32(
        &mut file,
        coff + 8,
        if symbols.is_empty() {
            0
        } else {
            symbol_pointer
        },
    );
    put32(&mut file, coff + 12, symbols.len() as u32 * 2);
    put16(&mut file, coff + 16, 28);
    put16(&mut file, coff + 18, 0x0103);
    // a.out header.
    let a = coff + 20;
    put16(&mut file, a, 0x010b);
    put32(&mut file, a + 4, text.len() as u32);
    put32(&mut file, a + 8, data.len() as u32);
    put32(&mut file, a + 12, bss);
    put32(&mut file, a + 16, TEXT_VADDR + entry);
    put32(&mut file, a + 20, TEXT_VADDR);
    put32(&mut file, a + 24, data_vaddr);
    // Section headers.
    for (index, (name, vaddr, size, pointer, flags)) in [
        (
            b".text\0\0\0",
            TEXT_VADDR,
            text.len() as u32,
            text_pointer,
            0x20,
        ),
        (
            b".data\0\0\0",
            data_vaddr,
            data.len() as u32,
            data_pointer,
            0x40,
        ),
        (b".bss\0\0\0\0", bss_vaddr, bss, 0, 0x80),
    ]
    .into_iter()
    .enumerate()
    {
        let s = coff + 48 + index * 40;
        file[s..s + 8].copy_from_slice(name);
        put32(&mut file, s + 8, vaddr);
        put32(&mut file, s + 12, vaddr);
        put32(&mut file, s + 16, size);
        put32(&mut file, s + 20, pointer);
        put32(&mut file, s + 36, flags);
    }
    file[coff + text_pointer as usize..][..text.len()].copy_from_slice(text);
    file[coff + data_pointer as usize..][..data.len()].copy_from_slice(data);
    // Each symbol has one zeroed auxiliary entry; long names go to strings.
    let mut strings = vec![0; 4];
    for &(name, value, section, kind, class) in symbols {
        let mut entry = [0; 18];
        if name.len() <= 8 {
            entry[..name.len()].copy_from_slice(name);
        } else {
            put32(&mut entry, 4, strings.len() as u32);
            strings.extend_from_slice(name);
            strings.push(0);
        }
        put32(&mut entry, 8, value);
        put16(&mut entry, 12, section as u16);
        put16(&mut entry, 14, kind);
        entry[16] = class;
        entry[17] = 1;
        file.extend_from_slice(&entry);
        file.extend_from_slice(&[0; 18]);
    }
    if !symbols.is_empty() {
        let size = strings.len() as u32;
        put32(&mut strings, 0, size);
        file.extend_from_slice(&strings);
    }
    file
}
