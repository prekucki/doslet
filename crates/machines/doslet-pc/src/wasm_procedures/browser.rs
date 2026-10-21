//! One Rust owner for the host worker's RAM and generated procedure exports.
//! Both WebAssembly instances use the worker's unshared linear memory. Only
//! the small call frame is encoded; guest buffers remain at stable addresses.
use alloc::{boxed::Box, string::String, vec};
use doslet_x86_wasm::static_program::{
    Frame,
    memory::{self, Descriptor, RamDescriptor, SegmentBounds, StackAddress},
    procedure_memory::{
        COMPLETE, RleCall, SIDE_EXIT, X87_BOUNDARY_OFFSET, X87_FRAME_BYTES, X87_SELECTORS_OFFSET,
        rle::{RleAdmission, RleMemory, prepare_rle},
    },
};

const PC24_BOUNDARY_OFFSET: usize = X87_BOUNDARY_OFFSET as usize;
const PC24_SELECTORS_OFFSET: usize = X87_SELECTORS_OFFSET as usize;
const PC24_BOUNDARY_BYTES: usize = PC24_SELECTORS_OFFSET - PC24_BOUNDARY_OFFSET;
const PC24_FRAME_BYTES: usize = X87_FRAME_BYTES as usize;

/// Calls a previously resolved export by its index in sorted function-ID order.
/// The host must not reenter the machine while executing this synchronous call.
pub type BrowserCall = Box<dyn FnMut(u32, u32) -> Result<u32, String>>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    Complete,
    SideExit,
}

pub(crate) struct Layout {
    pub ram_bytes: usize,
    pub video_pointer: u32,
}

pub(crate) struct SharedProcedureMemory {
    ram: Box<[u8]>,
    generations: Box<[u64]>,
    video: Box<[u8]>,
    protected: Box<[u8]>,
    scratch: Box<[u8]>,
    functions: usize,
    callback: BrowserCall,
    descriptor: Option<Descriptor>,
    failed: bool,
    pc24_masks: Option<(u8, u8)>,
}

impl SharedProcedureMemory {
    pub fn new(ids: &[u32], ram_bytes: usize, callback: BrowserCall) -> Result<Self, String> {
        if !matches!(ram_bytes, 4_194_304 | 8_388_608 | 16_777_216 | 33_554_432)
            || ids.is_empty()
            || ids.len() > 4096
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("invalid browser procedure memory layout or exports".into());
        }
        Ok(Self {
            ram: vec![0; ram_bytes].into_boxed_slice(),
            generations: vec![0; ram_bytes / 4096].into_boxed_slice(),
            video: vec![0; 512 * 1024].into_boxed_slice(),
            protected: vec![0; ram_bytes / 4096].into_boxed_slice(),
            scratch: vec![0; PC24_FRAME_BYTES.max(RleCall::OFFSET as usize + RleCall::BYTES)]
                .into_boxed_slice(),
            functions: ids.len(),
            callback,
            descriptor: None,
            failed: false,
            pc24_masks: None,
        })
    }

    pub fn layout(&self) -> Layout {
        Layout {
            ram_bytes: self.ram.len(),
            video_pointer: self.video.as_ptr() as u32,
        }
    }
    pub fn prepare_pc24_boundary(
        &mut self,
        read_mask: u8,
        write_mask: u8,
        boundary: &[u8; PC24_BOUNDARY_BYTES],
        selectors: [u16; 6],
    ) {
        self.scratch[PC24_BOUNDARY_OFFSET..PC24_SELECTORS_OFFSET].copy_from_slice(boundary);
        for (selector, output) in selectors.into_iter().zip(
            self.scratch[PC24_SELECTORS_OFFSET..PC24_SELECTORS_OFFSET + 12]
                .as_chunks_mut::<2>()
                .0,
        ) {
            output.copy_from_slice(&selector.to_le_bytes());
        }
        self.pc24_masks = Some((read_mask, write_mask));
    }
    pub fn pending_pc24_boundary(&self) -> Option<(u8, u8, &[u8])> {
        let (read, write) = self.pc24_masks?;
        Some((
            read,
            write,
            &self.scratch[PC24_BOUNDARY_OFFSET..PC24_SELECTORS_OFFSET],
        ))
    }
    pub fn clear_pc24_boundary(&mut self) {
        self.pc24_masks = None;
    }
    pub fn ram(&self) -> &[u8] {
        &self.ram
    }
    pub fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.ram
    }
    pub fn generations(&self) -> &[u64] {
        &self.generations
    }
    pub fn generations_mut(&mut self) -> &mut [u64] {
        &mut self.generations
    }
    pub fn video(&self) -> &[u8] {
        &self.video
    }
    pub fn video_mut(&mut self) -> &mut [u8] {
        &mut self.video
    }
    pub fn parts_mut(&mut self) -> (&mut [u8], &mut [u64]) {
        (&mut self.ram, &mut self.generations)
    }
    pub fn parts_with_video_mut(&mut self) -> (&mut [u8], &mut [u64], &mut [u8]) {
        (&mut self.ram, &mut self.generations, &mut self.video)
    }
    fn ram_descriptor(&self) -> RamDescriptor {
        RamDescriptor {
            pointer: self.ram.as_ptr() as u32 + 0x500,
            linear_base: 0x500,
            length: self.ram.len() as u32 - 0x500,
            excluded_start: 0xa0000,
            excluded_length: 0x20000,
            generations_pointer: self.generations.as_ptr() as u32,
            generations_length: self.generations.len() as u32,
            code_pages_pointer: self.protected.as_ptr() as u32,
            code_pages_length: self.protected.len() as u32,
        }
    }
    pub fn descriptor(&self, windows: [[SegmentBounds; 6]; 2], stack: StackAddress) -> Descriptor {
        Descriptor {
            ram: self.ram_descriptor(),
            windows,
            stack_address: stack,
            ..Descriptor::default()
        }
    }
    pub fn protect_source(&mut self, linear: u32, len: u32) -> Result<(), String> {
        let end = linear
            .checked_add(len)
            .filter(|&end| len != 0 && end as usize <= self.ram.len())
            .ok_or("browser procedure source outside RAM")?;
        if linear < 0x500 || (linear < 0xc0000 && end > 0xa0000) {
            return Err("browser procedure source outside ordinary RAM".into());
        }
        self.protected[linear as usize / 4096..=(end as usize - 1) / 4096].fill(1);
        Ok(())
    }
    pub fn prepare_rle(&self, frame: &Frame, descriptor: &Descriptor) -> Option<RleAdmission> {
        if self.failed || descriptor.ram != self.ram_descriptor() {
            return None;
        }
        prepare_rle(
            RleMemory {
                ram: &self.ram,
                generations: &self.generations,
                protected_pages: &self.protected,
                ram_pointer: self.ram.as_ptr() as u32,
            },
            frame,
            &descriptor.windows,
            descriptor.stack_address,
        )
    }
    pub fn execute_rle_index(
        &mut self,
        index: usize,
        frame: &mut Frame,
        descriptor: &Descriptor,
        admission: RleAdmission,
    ) -> Result<(Exit, memory::Effects), String> {
        admission
            .call()
            .write(&mut self.scratch[RleCall::OFFSET as usize..])
            .expect("fixed browser RLE header");
        let charged = frame.charged_work.checked_add(u64::from(admission.work()));
        let (exit, mut effects) = self.execute_index(index, frame, descriptor)?;
        let expected_writes = 6
            + u64::from(admission.call().output_count)
            + u64::from(admission.call().count_writes)
            + u64::from(admission.call().value_writes);
        if exit != Exit::Complete
            || frame.exact_total() != charged
            || frame.address.offset != admission.return_target()
            || effects.ram_writes != expected_writes
        {
            self.failed = true;
            return Err("RLE kernel violated its admitted computation".into());
        }
        effects.ram_writes = admission.complete(&mut self.generations);
        Ok((exit, effects))
    }
    pub fn execute_index(
        &mut self,
        index: usize,
        frame: &mut Frame,
        descriptor: &Descriptor,
    ) -> Result<(Exit, memory::Effects), String> {
        if self.failed {
            return Err("browser procedures stopped after an execution failure".into());
        }
        if index >= self.functions {
            return Err("unknown browser procedure export index".into());
        }
        let device = descriptor.device;
        let video_valid = if device.kind == memory::DeviceReadKind::Disabled {
            device == memory::DeviceReadDescriptor::default()
                && descriptor.device_write == memory::DeviceWriteDescriptor::default()
        } else {
            device.guest_start == 0xa0000
                && device.guest_length == 65536
                && device.pointer == self.video.as_ptr() as u32
                && device.backing_length as usize == self.video.len()
                && u64::from(device.base_offset) + 65536 <= self.video.len() as u64
        };
        let unchanged = self.descriptor.as_ref() == Some(descriptor);
        if !unchanged
            && (descriptor.ram != self.ram_descriptor()
                || !video_valid
                || descriptor.region_guards_pointer != 0
                || descriptor.region_guards_length != 0)
        {
            return Err("browser procedure descriptor lost its RAM/VRAM layout".into());
        }
        frame
            .write(&mut self.scratch)
            .expect("fixed browser CPU frame");
        let bytes = &mut self.scratch[memory::OFFSET as usize..];
        if unchanged {
            memory::Effects::clear(bytes).expect("fixed browser descriptor");
            let at = memory::INLINE_GRANT as usize;
            bytes[at..at + 4].copy_from_slice(&descriptor.inline_grant.to_le_bytes());
        } else {
            descriptor.write(bytes).expect("fixed browser descriptor");
            self.descriptor = Some(*descriptor);
        }
        // No RAM, generation, VRAM or scratch slice borrow survives this call.
        let result = (self.callback)(index as u32, self.scratch.as_mut_ptr() as u32);
        let exit = match result {
            Ok(COMPLETE) => Exit::Complete,
            Ok(SIDE_EXIT) => Exit::SideExit,
            Ok(status) => {
                self.failed = true;
                return Err(alloc::format!(
                    "unexpected browser procedure status {status}"
                ));
            }
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        *frame = Frame::read(&self.scratch).expect("fixed browser CPU frame");
        let effects = memory::Effects::read(&self.scratch[memory::OFFSET as usize..])
            .expect("fixed browser descriptor");
        self.descriptor
            .as_mut()
            .expect("initialized descriptor")
            .device
            .latches = effects.latches;
        Ok((exit, effects))
    }
}
