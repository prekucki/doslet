//! A single native/WASM owner for ordinary PC RAM and its code generations.

use super::Exit;
use super::rle::{RleAdmission, RleMemory, prepare_rle};
use doslet_x86_wasm::static_program::{
    Frame,
    memory::{self, Descriptor, RamDescriptor, SegmentBounds, StackAddress},
    procedure_memory::{COMPLETE, MAX_PROCEDURES, SIDE_EXIT},
};
use wasmtime::{Instance, Memory, MemoryType, Store, TypedFunc};

const WASM_PAGE_BYTES: usize = 65_536;
const CODE_PAGE_BYTES: usize = 4096;
const GENERATIONS_POINTER: usize = 1024;
const MAX_MODULE_BYTES: usize = 64 * 1024 * 1024;
const RAM_BASE: u32 = 0x500;
const VIDEO_BYTES: usize = 512 * 1024;
const VGA_START: u32 = 0xa_0000;
const VGA_END: u32 = 0xc_0000;

/// All offsets are into the one fixed Wasmtime backing. RAM index zero is
/// physical address zero; direct procedures see only the ordinary span at 500h.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedProcedureLayout {
    pub ram_pointer: u32,
    pub ram_bytes: usize,
    pub video_pointer: u32,
    pub video_bytes: usize,
    pub generations_pointer: u32,
    pub generations_length: u32,
    pub code_pages_pointer: u32,
    pub code_pages_length: u32,
    pub memory_bytes: usize,
}

impl SharedProcedureLayout {
    fn new(ram_bytes: usize) -> Result<Self, String> {
        if !matches!(ram_bytes, 4_194_304 | 8_388_608 | 16_777_216 | 33_554_432) {
            return Err("shared procedure RAM must be 4, 8, 16 or 32 MiB".into());
        }
        let pages = ram_bytes / CODE_PAGE_BYTES;
        let code_pages_pointer = GENERATIONS_POINTER + pages * size_of::<u64>();
        let metadata_end = code_pages_pointer + pages;
        let ram_pointer = metadata_end.next_multiple_of(WASM_PAGE_BYTES);
        let video_pointer = ram_pointer + ram_bytes;
        Ok(Self {
            ram_pointer: ram_pointer as u32,
            ram_bytes,
            video_pointer: video_pointer as u32,
            video_bytes: VIDEO_BYTES,
            generations_pointer: GENERATIONS_POINTER as u32,
            generations_length: pages as u32,
            code_pages_pointer: code_pages_pointer as u32,
            code_pages_length: pages as u32,
            memory_bytes: video_pointer + VIDEO_BYTES,
        })
    }

    fn ram_descriptor(self) -> RamDescriptor {
        RamDescriptor {
            pointer: self.ram_pointer + RAM_BASE,
            linear_base: RAM_BASE,
            length: self.ram_bytes as u32 - RAM_BASE,
            excluded_start: VGA_START,
            excluded_length: VGA_END - VGA_START,
            generations_pointer: self.generations_pointer,
            generations_length: self.generations_length,
            code_pages_pointer: self.code_pages_pointer,
            code_pages_length: self.code_pages_length,
        }
    }

    fn generations_range(self) -> core::ops::Range<usize> {
        self.generations_pointer as usize..self.code_pages_pointer as usize
    }

    fn ram_range(self) -> core::ops::Range<usize> {
        self.ram_pointer as usize..self.ram_pointer as usize + self.ram_bytes
    }
    fn video_range(self) -> core::ops::Range<usize> {
        self.video_pointer as usize..self.video_pointer as usize + self.video_bytes
    }
}

/// One mutable owner, moved with the machine to its worker. Native CPU/bus
/// accesses borrow the actual WASM RAM and generation table. No RAM image,
/// pointer, shared borrow or device callback survives entry into generated code.
///
/// The caller validates frozen sources and CPU/mapping capabilities, protects
/// all compiled source pages, and reserves a hardware-bounded work grant before
/// entry. This owner enforces backing layout, not those execution proofs.
pub struct SharedProcedureMemory {
    store: Store<()>,
    memory: Memory,
    functions: Vec<(u32, TypedFunc<u32, u32>)>,
    layout: SharedProcedureLayout,
    // Only generated effects/grant/latches may modify the descriptor during a
    // call. Keep the last validated inputs so stable mappings need no rewrite.
    descriptor: Option<Descriptor>,
    failed: bool,
    compile_seconds: f64,
    load_seconds: f64,
}

impl SharedProcedureMemory {
    /// Compile trusted compiler output with Cranelift Speed and bind every
    /// `memory_procedure_<id>` once, in one instance with fixed-size memory.
    /// IDs must be sorted and unique; input caps precede compilation/allocation.
    pub fn new(wasm: &[u8], function_ids: &[u32], ram_bytes: usize) -> Result<Self, String> {
        Self::with_cache(wasm, function_ids, ram_bytes, None)
    }

    /// As `new`, reusing native code from a host cache when it is compatible.
    pub fn with_cache(
        wasm: &[u8],
        function_ids: &[u32],
        ram_bytes: usize,
        cache: Option<&mut dyn crate::CompiledCache>,
    ) -> Result<Self, String> {
        if wasm.len() > MAX_MODULE_BYTES {
            return Err("shared procedure module exceeds 64 MiB".into());
        }
        if function_ids.is_empty() || function_ids.len() > MAX_PROCEDURES {
            return Err("shared procedure module requires 1 through 4096 function IDs".into());
        }
        if function_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("shared procedure function IDs must be sorted and unique".into());
        }
        if !cfg!(target_endian = "little") {
            return Err("shared procedure generations require a little-endian host".into());
        }
        let layout = SharedProcedureLayout::new(ram_bytes)?;
        let artifact = match cache {
            Some(cache) => crate::Artifact::compile_cached(wasm, cache)?,
            None => crate::Artifact::compile(wasm)?,
        };
        let mut store = Store::new(&artifact.engine, ());
        let pages = (layout.memory_bytes / WASM_PAGE_BYTES) as u32;
        let memory = Memory::new(&mut store, MemoryType::new(pages, Some(pages)))
            .map_err(|e| format!("shared procedure memory: {e:#}"))?;
        bytemuck::try_cast_slice::<u8, u64>(&memory.data(&store)[layout.generations_range()])
            .map_err(|e| format!("shared procedure generations alignment: {e}"))?;
        let instance = Instance::new(&mut store, &artifact.module, &[memory.into()])
            .map_err(|e| format!("shared procedure instantiation: {e:#}"))?;
        let mut functions = Vec::new();
        functions
            .try_reserve_exact(function_ids.len())
            .map_err(|_| "shared procedure function allocation")?;
        for &id in function_ids {
            let name = format!("memory_procedure_{id}");
            let function = instance
                .get_typed_func::<u32, u32>(&mut store, &name)
                .map_err(|e| format!("shared procedure export {name}: {e:#}"))?;
            functions.push((id, function));
        }
        Ok(Self {
            store,
            memory,
            functions,
            layout,
            descriptor: None,
            failed: false,
            compile_seconds: artifact.compile_seconds,
            load_seconds: artifact.load_seconds,
        })
    }

    #[inline]
    pub fn layout(&self) -> SharedProcedureLayout {
        self.layout
    }

    pub fn compile_seconds(&self) -> f64 {
        self.compile_seconds
    }

    pub fn load_seconds(&self) -> f64 {
        self.load_seconds
    }

    /// Resolve a compiler ID once during plan installation. The index belongs
    /// to this owner's immutable export table and is checked again on entry.
    pub fn function_index(&self, function_id: u32) -> Option<usize> {
        self.functions
            .binary_search_by_key(&function_id, |&(id, _)| id)
            .ok()
    }

    pub fn ram(&self) -> &[u8] {
        &self.memory.data(&self.store)[self.layout.ram_range()]
    }

    /// The bus must update `generations` for its writes, as with its original
    /// RAM owner. Borrowing the backing itself performs no invalidation.
    pub fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.memory.data_mut(&mut self.store)[self.layout.ram_range()]
    }

    pub fn generations(&self) -> &[u64] {
        bytemuck::try_cast_slice(&self.memory.data(&self.store)[self.layout.generations_range()])
            .expect("validated fixed generations backing")
    }

    /// Physical VRAM shares the same exclusive owner as RAM and generated code.
    pub fn video(&self) -> &[u8] {
        &self.memory.data(&self.store)[self.layout.video_range()]
    }

    pub fn video_mut(&mut self) -> &mut [u8] {
        &mut self.memory.data_mut(&mut self.store)[self.layout.video_range()]
    }

    pub fn generations_mut(&mut self) -> &mut [u64] {
        bytemuck::try_cast_slice_mut(
            &mut self.memory.data_mut(&mut self.store)[self.layout.generations_range()],
        )
        .expect("validated fixed generations backing")
    }

    /// Disjoint native bus views of the same RAM and generations used by WASM.
    pub fn parts_mut(&mut self) -> (&mut [u8], &mut [u64]) {
        let (ram, generations, _) = self.parts_with_video_mut();
        (ram, generations)
    }

    /// Borrow all physical memory once for a bounded native interpreter batch.
    pub fn parts_with_video_mut(&mut self) -> (&mut [u8], &mut [u64], &mut [u8]) {
        let layout = self.layout;
        let (metadata, backing) = self
            .memory
            .data_mut(&mut self.store)
            .split_at_mut(layout.ram_pointer as usize);
        let generations = bytemuck::try_cast_slice_mut(&mut metadata[layout.generations_range()])
            .expect("validated fixed generations backing");
        let (ram, video) = backing.split_at_mut(layout.ram_bytes);
        (ram, generations, &mut video[..layout.video_bytes])
    }

    /// Build the ordinary-RAM ABI using the caller's current segment windows
    /// and independent SS.B stack width. The VGA aperture remains excluded.
    pub fn descriptor(&self, windows: [[SegmentBounds; 6]; 2], stack: StackAddress) -> Descriptor {
        Descriptor {
            ram: self.layout.ram_descriptor(),
            windows,
            stack_address: stack,
            ..Descriptor::default()
        }
    }

    /// Establish one complete RLE computation over the actual shared backing.
    /// The caller keeps exclusive ownership until consuming this proof below.
    pub fn prepare_rle(&self, frame: &Frame, descriptor: &Descriptor) -> Option<RleAdmission> {
        if self.failed || descriptor.ram != self.layout.ram_descriptor() {
            return None;
        }
        let bytes = self.memory.data(&self.store);
        let generations = bytemuck::try_cast_slice(&bytes[self.layout.generations_range()]).ok()?;
        let start = self.layout.code_pages_pointer as usize;
        prepare_rle(
            RleMemory {
                ram: &bytes[self.layout.ram_range()],
                generations,
                protected_pages: &bytes[start..start + self.layout.code_pages_length as usize],
                ram_pointer: self.layout.ram_pointer,
            },
            frame,
            &descriptor.windows,
            descriptor.stack_address,
        )
    }

    /// Consume a proof made from this owner's unchanged RAM and mappings. The
    /// generated body performs one complete computation; page generations are
    /// reconciled from the original store counts before native execution resumes.
    pub fn execute_rle_index(
        &mut self,
        index: usize,
        frame: &mut Frame,
        descriptor: &Descriptor,
        admission: RleAdmission,
    ) -> Result<(Exit, memory::Effects), String> {
        use doslet_x86_wasm::static_program::procedure_memory::RleCall;
        admission
            .call
            .write(&mut self.memory.data_mut(&mut self.store)[RleCall::OFFSET as usize..])
            .expect("fixed RLE call header");
        let charged = frame.charged_work.checked_add(u64::from(admission.work()));
        let (exit, mut effects) = self.execute_index(index, frame, descriptor)?;
        let expected_writes = 6
            + u64::from(admission.call.output_count)
            + u64::from(admission.call.count_writes)
            + u64::from(admission.call.value_writes);
        if exit != Exit::Complete
            || frame.exact_total() != charged
            || frame.address.offset != admission.return_target()
            || effects.ram_writes != expected_writes
        {
            self.failed = true;
            return Err("RLE kernel violated its admitted computation".into());
        }
        // The kernel reports original stores; PcBus counts touched pages,
        // including DWORD writes which straddle a page boundary.
        effects.ram_writes = admission.complete(self.generations_mut());
        let at = (memory::OFFSET + memory::RAM_WRITES) as usize;
        self.memory.data_mut(&mut self.store)[at..at + 8]
            .copy_from_slice(&effects.ram_writes.to_le_bytes());
        Ok((exit, effects))
    }

    /// Protect every physical 4-KiB page touched by an admitted source. Bits
    /// accumulate for this owner's lifetime, covering every bound procedure.
    pub fn protect_source(&mut self, linear: u32, len: u32) -> Result<(), String> {
        let end = linear
            .checked_add(len)
            .filter(|&end| len != 0 && end as usize <= self.layout.ram_bytes)
            .ok_or("shared procedure source outside RAM")?;
        if linear < RAM_BASE || (linear < VGA_END && end > VGA_START) {
            return Err("shared procedure source outside ordinary RAM".into());
        }
        let first = linear as usize / CODE_PAGE_BYTES;
        let last = (end as usize - 1) / CODE_PAGE_BYTES;
        let start = self.layout.code_pages_pointer as usize;
        self.memory.data_mut(&mut self.store)[start + first..=start + last].fill(1);
        Ok(())
    }

    /// Transfer only the small CPU frame/descriptor. RAM, generations and
    /// source bits stay in place. A trap/invalid status poisons further entry,
    /// because a committed store must never be replayed by retrying the body.
    pub fn execute(
        &mut self,
        function_id: u32,
        frame: &mut Frame,
        descriptor: &Descriptor,
    ) -> Result<(Exit, memory::Effects), String> {
        let index = self
            .function_index(function_id)
            .ok_or("unknown shared memory procedure")?;
        self.execute_index(index, frame, descriptor)
    }

    /// The same checked call boundary using an export resolved at setup.
    /// Stable input mappings stay encoded in place; only the CPU frame,
    /// per-call effect counters and work grant are reset on repeated calls.
    pub fn execute_index(
        &mut self,
        index: usize,
        frame: &mut Frame,
        descriptor: &Descriptor,
    ) -> Result<(Exit, memory::Effects), String> {
        if self.failed {
            return Err("shared procedure memory stopped after an execution failure".into());
        }
        let unchanged = self.descriptor.as_ref() == Some(descriptor);
        let device = descriptor.device;
        let video_valid = if device.kind == memory::DeviceReadKind::Disabled {
            device == memory::DeviceReadDescriptor::default()
                && descriptor.device_write == memory::DeviceWriteDescriptor::default()
        } else {
            device.guest_start == VGA_START
                && device.guest_length == 65536
                && device.pointer == self.layout.video_pointer
                && device.backing_length as usize == self.layout.video_bytes
                && u64::from(device.base_offset) + 65536 <= self.layout.video_bytes as u64
        };
        if !unchanged
            && (descriptor.ram != self.layout.ram_descriptor()
                || !video_valid
                || descriptor.region_guards_pointer != 0
                || descriptor.region_guards_length != 0)
        {
            return Err("shared procedure descriptor must retain its RAM/VRAM layout".into());
        }
        let function = &self
            .functions
            .get(index)
            .ok_or("unknown shared memory procedure index")?
            .1;
        {
            let bytes = self.memory.data_mut(&mut self.store);
            frame.write(bytes).expect("fixed CPU frame");
            let bytes = &mut bytes[memory::OFFSET as usize..];
            if unchanged {
                memory::Effects::clear(bytes).expect("fixed memory descriptor");
                let grant = memory::INLINE_GRANT as usize;
                bytes[grant..grant + 4].copy_from_slice(&descriptor.inline_grant.to_le_bytes());
            } else {
                descriptor.write(bytes).expect("fixed memory descriptor");
                self.descriptor = Some(*descriptor);
            }
        }
        let exit = match function.call(&mut self.store, 0) {
            Ok(COMPLETE) => Exit::Complete,
            Ok(SIDE_EXIT) => Exit::SideExit,
            Ok(status) => {
                self.failed = true;
                return Err(format!(
                    "unexpected shared memory procedure status {status}"
                ));
            }
            Err(error) => {
                self.failed = true;
                return Err(format!("shared memory procedure execution: {error:#}"));
            }
        };
        let bytes = self.memory.data(&self.store);
        *frame = Frame::read(bytes).expect("fixed CPU frame");
        let effects = memory::Effects::read(&bytes[memory::OFFSET as usize..])
            .expect("fixed memory descriptor");
        // Generated reads update the latch mirror. Remember its actual output
        // so the next call with unchanged live VGA controls can reuse it.
        self.descriptor
            .as_mut()
            .expect("initialized descriptor")
            .device
            .latches = effects.latches;
        Ok((exit, effects))
    }
}

#[cfg(test)]
mod tests;
