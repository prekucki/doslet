//! Entry proof for a complete run-length computation over exclusively owned RAM.
//! The input prefix, control state and original write generations are proved
//! before generated code can commit any effect. No guest buffer is copied.

use doslet_x86_wasm::static_program::{
    Frame, Register,
    memory::{Segment, SegmentBounds, StackAddress},
    procedure_memory::RleCall,
};

const PAGE: u32 = 4096;

#[derive(Clone, Copy, Debug, Default)]
struct Span {
    start: u32,
    len: u32,
}

impl Span {
    fn end(self) -> u64 {
        u64::from(self.start) + u64::from(self.len)
    }
    fn overlaps(self, other: Self) -> bool {
        self.len != 0
            && other.len != 0
            && u64::from(self.start) < other.end()
            && u64::from(other.start) < self.end()
    }
    fn bytes(self, ram: &[u8]) -> Option<&[u8]> {
        ram.get(self.start as usize..usize::try_from(self.end()).ok()?)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Writes {
    span: Span,
    repetitions: u32,
    bytewise: bool,
}

impl Writes {
    fn delta(self, page: u32) -> u64 {
        let first = u64::from(page) * u64::from(PAGE);
        let end = first + u64::from(PAGE);
        let overlap = self
            .span
            .end()
            .min(end)
            .saturating_sub(u64::from(self.span.start).max(first));
        if self.bytewise {
            overlap
        } else {
            u64::from(self.repetitions) * u64::from(overlap != 0)
        }
    }
}

/// A live-entry proof. Its owner must keep RAM, mappings and registers unchanged
/// until entering the corresponding trusted kernel. IRQ/device service occurs
/// only after that computation. The proof is consumed after one invocation.
pub struct RleAdmission {
    pub(super) call: RleCall,
    writes: [Writes; 9],
}

impl RleAdmission {
    pub fn work(&self) -> u32 {
        self.call.work
    }
    pub fn return_target(&self) -> u32 {
        self.call.return_target
    }

    fn each_page(&self, mut visit: impl FnMut(u32, u64) -> Option<()>) -> Option<()> {
        for (index, write) in self.writes.iter().enumerate() {
            if write.span.len == 0 || (!write.bytewise && write.repetitions == 0) {
                continue;
            }
            let first = write.span.start / PAGE;
            let last = u32::try_from((write.span.end() - 1) / u64::from(PAGE)).ok()?;
            for page in first..=last {
                if self.writes[..index].iter().any(|w| w.delta(page) != 0) {
                    continue;
                }
                let delta = self.writes.iter().map(|w| w.delta(page)).sum();
                visit(page, delta)?;
            }
        }
        Some(())
    }

    pub(super) fn complete(&self, generations: &mut [u64]) -> u64 {
        let mut writes = 0;
        self.each_page(|page, delta| {
            let generation = &mut generations[page as usize];
            *generation = generation
                .checked_add(delta)
                .expect("proved RLE generations");
            writes += delta;
            Some(())
        })
        .expect("proved RLE pages");
        writes
    }
}

/// Read-only inputs for the same native and interpreted admission proof.
pub struct RleMemory<'a> {
    pub ram: &'a [u8],
    pub generations: &'a [u64],
    pub protected_pages: &'a [u8],
    /// Zero is useful for an interpreted reference, which never uses backing
    /// offsets. The shared owner supplies its actual fixed WASM RAM pointer.
    pub ram_pointer: u32,
}

fn span(memory: &RleMemory<'_>, window: SegmentBounds, offset: u32, len: u32) -> Option<Span> {
    if len == 0 {
        return Some(Span::default());
    }
    let last = offset.checked_add(len - 1)?;
    if !window.valid || offset < window.first || last > window.last {
        return None;
    }
    let start = window.base.checked_add(offset)?;
    let result = Span { start, len };
    if start < 0x500 || (start < 0xc0000 && result.end() > 0xa0000) {
        return None;
    }
    result.bytes(memory.ram)?;
    Some(result)
}

fn word(memory: &RleMemory<'_>, window: SegmentBounds, offset: u32) -> Option<u32> {
    let bytes = span(memory, window, offset, 4)?.bytes(memory.ram)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// Recognized procedure semantics supply the proof template; live values prove
/// this invocation terminates and that bulk stores preserve every alias and
/// page-generation dependency. A failed proof has no architectural effects.
pub fn prepare_rle(
    memory: RleMemory<'_>,
    frame: &Frame,
    windows: &[[SegmentBounds; 6]; 2],
    stack: StackAddress,
) -> Option<RleAdmission> {
    if stack != StackAddress::Bits32 {
        return None;
    }
    let n = frame.gpr[Register::Ecx as usize];
    // Empty/negative calls remain ordinary interpretation. Positive output is
    // finite through the proved input prefix, independent of a step budget.
    if n == 0 || n > i32::MAX as u32 {
        return None;
    }
    let sp = frame.gpr[Register::Esp as usize];
    let ds_read = windows[0][Segment::Ds as usize];
    let ds_write = windows[1][Segment::Ds as usize];
    let ss_read = windows[0][Segment::Ss as usize];
    let ss_write = windows[1][Segment::Ss as usize];
    let args = span(&memory, ss_read, sp, 12)?;
    let return_target = word(&memory, ss_read, sp)?;
    let count_offset = word(&memory, ss_read, sp.checked_add(4)?)?;
    let value_offset = word(&memory, ss_read, sp.checked_add(8)?)?;
    let local_offset = sp.checked_sub(24)?;
    let locals = span(&memory, ss_write, local_offset, 24)?;
    // SS read/write windows must resolve to the same physical stack bytes.
    if u64::from(locals.start) + 24 != u64::from(args.start) {
        return None;
    }
    let stack_span = Span {
        start: locals.start,
        len: 36,
    };
    let count_span = span(&memory, ds_read, count_offset, 4)?;
    // The recognized assembly keeps the value pointer in EBP: its default
    // segment is SS, independently of the DS source/destination/count spans.
    let value_span = span(&memory, ss_read, value_offset, 4)?;
    let initial_run = word(&memory, ds_read, count_offset)?;
    let initial_value = word(&memory, ss_read, value_offset)?;
    let initial_output = if (initial_run as i32) > 0 {
        initial_run.min(n)
    } else {
        0
    };
    let mut produced = initial_output;
    let mut consumed = 0u32;
    let mut count_writes = initial_output;
    let mut value_writes = 0u32;
    let mut work = 19u64
        + 13 * u64::from(initial_output)
        + if (initial_run as i32) > 0 && initial_run > initial_output {
            4
        } else {
            2
        };
    let input = frame.gpr[Register::Eax as usize];
    while produced < n {
        // Every token consumes one or two bytes. Checked nonwrapping ordinary
        // RAM, not scheduling fuel, bounds this read-only prefix traversal.
        let at = input.checked_add(consumed)?;
        let byte = span(&memory, ds_read, at, 1)?.bytes(memory.ram)?[0];
        consumed = consumed.checked_add(1)?;
        if byte & 0xc0 == 0xc0 {
            span(&memory, ds_read, input.checked_add(consumed)?, 1)?;
            consumed = consumed.checked_add(1)?;
            let run = u32::from(byte & 63);
            let pixels = run.min(n - produced);
            produced += pixels;
            count_writes = count_writes.checked_add(1)?.checked_add(pixels)?;
            value_writes = value_writes.checked_add(1)?;
            work += 22 + 14 * u64::from(pixels) + if run > pixels { 2 } else { 0 };
        } else {
            produced += 1;
            work += 20;
        }
        if work > u64::from(u32::MAX) {
            return None;
        }
    }
    let source = span(&memory, ds_read, input, consumed)?;
    let destination = span(&memory, ds_write, frame.gpr[Register::Ebx as usize], n)?;
    if count_writes != 0 && span(&memory, ds_write, count_offset, 4)?.start != count_span.start {
        return None;
    }
    if value_writes != 0 && span(&memory, ss_write, value_offset, 4)?.start != value_span.start {
        return None;
    }
    let regions = [source, destination, count_span, value_span, stack_span];
    for (i, region) in regions.iter().enumerate() {
        if regions[..i]
            .iter()
            .any(|previous| region.overlaps(*previous))
        {
            return None;
        }
    }
    let pointer = |linear: u32| memory.ram_pointer.checked_add(linear);
    let call = RleCall {
        source_pointer: pointer(source.start)?,
        destination_pointer: pointer(destination.start)?,
        count_pointer: pointer(count_span.start)?,
        value_pointer: pointer(value_span.start)?,
        stack_pointer: pointer(locals.start)?,
        return_target,
        work: u32::try_from(work).ok()?,
        output_count: n,
        initial_run,
        initial_value,
        source_length: consumed,
        count_offset,
        count_writes,
        value_writes,
    };
    let mut writes = [Writes::default(); 9];
    writes[0] = Writes {
        span: destination,
        bytewise: true,
        repetitions: 1,
    };
    writes[1] = Writes {
        span: count_span,
        repetitions: count_writes,
        bytewise: false,
    };
    writes[2] = Writes {
        span: value_span,
        repetitions: value_writes,
        bytewise: false,
    };
    for (i, write) in writes[3..].iter_mut().enumerate() {
        *write = Writes {
            span: Span {
                start: locals.start + i as u32 * 4,
                len: 4,
            },
            repetitions: 1,
            bytewise: false,
        };
    }
    let admission = RleAdmission { call, writes };
    admission.each_page(|page, delta| {
        if *memory.protected_pages.get(page as usize)? != 0 {
            return None;
        }
        memory.generations.get(page as usize)?.checked_add(delta)?;
        Some(())
    })?;
    Some(admission)
}
