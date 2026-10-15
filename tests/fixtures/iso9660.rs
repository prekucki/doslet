fn test_iso() -> Vec<u8> {
    fn record(sector: u32, size: u32, flags: u8, name: &[u8]) -> Vec<u8> {
        let length = (33 + name.len() + 1) & !1;
        let mut record = vec![0; length];
        record[0] = length as u8;
        record[2..6].copy_from_slice(&sector.to_le_bytes());
        record[6..10].copy_from_slice(&sector.to_be_bytes());
        record[10..14].copy_from_slice(&size.to_le_bytes());
        record[14..18].copy_from_slice(&size.to_be_bytes());
        record[25] = flags;
        record[28..32].copy_from_slice(&[1, 0, 0, 1]);
        record[32] = name.len() as u8;
        record[33..33 + name.len()].copy_from_slice(name);
        record
    }
    let mut bytes = vec![0; 25 * 2048];
    let pvd = &mut bytes[16 * 2048..17 * 2048];
    pvd[..7].copy_from_slice(b"\x01CD001\x01");
    pvd[80..84].copy_from_slice(&25u32.to_le_bytes());
    pvd[84..88].copy_from_slice(&25u32.to_be_bytes());
    pvd[128..132].copy_from_slice(&[0, 8, 8, 0]);
    let root = record(20, 2048, 2, &[0]);
    pvd[156..156 + root.len()].copy_from_slice(&root);
    bytes[17 * 2048..17 * 2048 + 7].copy_from_slice(b"\xFFCD001\x01");
    for (sector, child, child_sector, flags, size) in [
        (20, &b"HEROES"[..], 21, 2, 2048),
        (21, &b"SOUND"[..], 22, 2, 2048),
        (22, &b"HEROES01.62S;1"[..], 23, 0, 4),
    ] {
        let mut offset = sector * 2048;
        for entry in [record(sector as u32, 2048, 2, &[0]), record(20, 2048, 2, &[1]), record(child_sector, size, flags, child)] {
            bytes[offset..offset + entry.len()].copy_from_slice(&entry);
            offset += entry.len();
        }
    }
    bytes[23 * 2048..23 * 2048 + 4].copy_from_slice(b"data");
    bytes
}
