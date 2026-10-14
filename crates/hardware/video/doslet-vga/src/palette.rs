pub(crate) fn default_palette() -> [[u8; 4]; 256] {
    let mut palette = [[0, 0, 0, 255]; 256];
    let ega = [
        [0, 0, 0],
        [0, 0, 170],
        [0, 170, 0],
        [0, 170, 170],
        [170, 0, 0],
        [170, 0, 170],
        [170, 85, 0],
        [170, 170, 170],
        [85, 85, 85],
        [85, 85, 255],
        [85, 255, 85],
        [85, 255, 255],
        [255, 85, 85],
        [255, 85, 255],
        [255, 255, 85],
        [255, 255, 255],
    ];
    for (entry, [r, g, b]) in palette.iter_mut().zip(ega) {
        *entry = [r, g, b, 255];
    }
    // Bootstrap color cube, not a claim of the original VGA BIOS's complete palette.
    for (index, entry) in palette.iter_mut().enumerate().skip(16) {
        let value = index - 16;
        *entry = [
            ((value / 36) % 6 * 51) as u8,
            ((value / 6) % 6 * 51) as u8,
            (value % 6 * 51) as u8,
            255,
        ];
    }
    palette
}
