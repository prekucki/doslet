/// Read-only view. It cannot outlive or overlap mutable access to the VGA device.
#[derive(Clone, Copy)]
pub struct IndexedFrame<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a [u8],
    pub palette: &'a [[u8; 4]; 256],
    pub revision: u64,
}
impl IndexedFrame<'_> {
    pub fn write_rgba(&self, output: &mut [u8]) -> Result<(), FrameSizeError> {
        if output.len() != self.pixels.len() * 4 {
            return Err(FrameSizeError);
        }
        for (pixel, rgba) in self.pixels.iter().zip(output.chunks_exact_mut(4)) {
            rgba.copy_from_slice(&self.palette[usize::from(*pixel)]);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSizeError;
