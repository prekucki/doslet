//! Exclusive Session owner for the admitted, counted static-register WASM ABI.
//!
//! Metadata describes one already-bound, post-relocation executable. It is not a
//! translated-code cache: neither admission nor fallback changes the static graph.
//! The caller runs only the matching compiler-produced module, synchronously,
//! between `begin_run` and `finish_run`. A generated-code trap must abandon this
//! owner; partially executed regions cannot be recovered or rolled back.
use doslet_runtime::{RuntimeError, Session, SessionStatus, session::debug::CodeVersion};
use doslet_x86::{Access, SegmentCache, SegmentRegister, flags};
use doslet_x86_wasm::static_program::{Address, Frame, MAX_REGION_INSTRUCTIONS, MAX_REGIONS, abi};

const MAX_OBJECTS: usize = 65_535;
const MAX_CODE_PARTS: usize = 1_048_576;
const MAX_CODE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EMPTY_SERVICES: u8 = 8;
const ARITHMETIC_FLAGS: u32 =
    flags::CARRY | flags::PARITY | flags::AUXILIARY | flags::ZERO | flags::SIGN | flags::OVERFLOW;

/// Space zero is reserved for an unbound, bus-linear interpreter address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectBinding {
    pub space: u32,
    pub linear_base: u32,
    pub size: u32,
    pub default32: bool,
}

/// Exact compiled source bytes, after executable relocation; length is 1..=255.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodePart {
    pub linear: u32,
    pub expected: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionMetadata {
    /// Dense IDs in vector order, identical to the system lowerer's IDs.
    pub id: u32,
    pub entry: Address,
    /// Includes a completed JMP/Jcc; zero-work fallback stubs have no parts.
    pub required_steps: u32,
    /// Optional wait-candidate hint, never a recurrence proof. Such entries are
    /// zero-work runtime stubs; the ordinary interpreter proves every skip.
    pub wait_proof_steps: u32,
    /// Consecutive source chunks from entry, including a completed terminator.
    pub parts: Vec<CodePart>,
}

/// Actual selector and hidden descriptor used to compile register-only code.
/// This slice admits no instructions whose behavior depends on CPU privilege.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeContext {
    pub cs: SegmentCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub objects: Vec<ObjectBinding>,
    pub regions: Vec<RegionMetadata>,
    pub context: CodeContext,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub translated_steps: u64,
    pub interpreted_steps: u64,
    pub callbacks: u64,
    pub memory_write_helpers: u64,
    pub guard_refusals: u64,
    pub wait_calls: u64,
    pub skipped_wait_steps: u64,
    pub fallback_diagnostics: FallbackDiagnostics,
}

/// Optional callback counts, independent of completed instruction counts. The
/// seven causes are exclusive, in the order declared here. Source checks cover
/// every immutable CodePart; residual refusals can include stale inline guards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FallbackDiagnostics {
    pub fallback_callbacks: u64,
    pub service_callbacks: u64,
    pub unknown_entry: u64,
    pub unsupported_stub: u64,
    pub context_refused: u64,
    pub hard_grant: u64,
    pub hard_grant_required_steps_sum: u64,
    pub hard_grant_available_steps_sum: u64,
    pub hard_grant_required_steps_max: u32,
    pub hard_grant_available_steps_max: u32,
    pub source_changed: u64,
    pub stale_proof: u64,
    pub memory_or_guard: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FallbackCause {
    UnknownEntry,
    UnsupportedStub,
    ContextRefused,
    HardGrant { required: u32, available: u32 },
    SourceChanged,
    StaleProof,
    MemoryOrGuard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Continue,
    Exit,
}
impl Control {
    pub fn abi_value(self) -> u32 {
        match self {
            Self::Continue => abi::CONTINUE,
            Self::Exit => abi::EXIT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Metadata(&'static str),
    Protocol(&'static str),
    Runtime(RuntimeError),
    NoProgress,
    Allocation,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Metadata(message) => write!(f, "static metadata: {message}"),
            Self::Protocol(message) => write!(f, "static runtime protocol: {message}"),
            Self::Runtime(error) => error.fmt(f),
            Self::NoProgress => f.write_str("static runtime made no guest progress"),
            Self::Allocation => f.write_str("static runtime setup allocation failed"),
        }
    }
}
impl core::error::Error for Error {}

pub struct StaticSession {
    session: Session,
    metadata: Metadata,
    proofs: Vec<Vec<Option<CodeVersion>>>,
    wait_entries: Vec<(Address, u32)>,
    direct_entry_index: Vec<(Address, usize)>,
    counters: Counters,
    diagnose_fallbacks: bool,
    active: bool,
    poisoned: bool,
    exiting: bool,
    grant: u32,
    pending: u32,
    accounted_work: u64,
    boundary_budget: u64,
    empty_services: u8,
}

impl StaticSession {
    fn direct_guard(
        &mut self,
        index: usize,
    ) -> doslet_x86_wasm::static_program::memory::RegionGuard {
        use doslet_x86_wasm::static_program::memory::RegionGuard;
        let region = &self.metadata.regions[index];
        if region.required_steps == 0 {
            return RegionGuard::default();
        }
        for (part, proof) in region.parts.iter().zip(&mut self.proofs[index]) {
            if proof
                .as_ref()
                .is_some_and(|version| self.session.debug_code_valid(version))
            {
                continue;
            }
            if self
                .session
                .debug_code_bytes(part.linear, part.expected.len() as u8)
                != Some(part.expected.as_slice())
            {
                return RegionGuard::default();
            }
            *proof = self
                .session
                .debug_code_version(part.linear, part.expected.len() as u8);
        }
        if self.proofs[index].iter().any(Option::is_none) {
            return RegionGuard::default();
        }
        let Some(first) = self.proofs[index]
            .first()
            .and_then(|proof| *proof)
            .map(CodeVersion::parts)
        else {
            return RegionGuard::default();
        };
        let Some(last) = self.proofs[index]
            .last()
            .and_then(|proof| *proof)
            .map(CodeVersion::parts)
        else {
            return RegionGuard::default();
        };
        if first.space != last.space || last.pages[1].saturating_sub(first.pages[0]) > 1 {
            return RegionGuard::default();
        }
        RegionGuard {
            valid: true,
            pages: [first.pages[0], last.pages[1]],
            generations: [first.generations[0], last.generations[1]],
        }
    }

    /// Setup-only table, owned by the application for the complete module run.
    pub fn direct_guards(&mut self) -> Result<Vec<u8>, Error> {
        use doslet_x86_wasm::static_program::memory::REGION_GUARD_BYTES;
        self.require_idle()?;
        let length = self
            .metadata
            .regions
            .len()
            .checked_mul(REGION_GUARD_BYTES)
            .ok_or(Error::Allocation)?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(length)
            .map_err(|_| Error::Allocation)?;
        result.resize(length, 0);
        for index in 0..self.metadata.regions.len() {
            self.direct_guard(index)
                .write(&mut result[index * REGION_GUARD_BYTES..])
                .expect("sized guard table");
        }
        Ok(result)
    }

    /// Refresh only the next current entry. Other stale entries fail their
    /// inline guard and take ordinary fallback before effects. No translation
    /// or dynamic code-cache construction occurs here. Returns whether the
    /// current entry has a valid generated region and needs a memory snapshot.
    pub fn refresh_direct_guard(
        &mut self,
        address: Address,
        guards: &mut [u8],
    ) -> Result<bool, Error> {
        use doslet_x86_wasm::static_program::memory::REGION_GUARD_BYTES;
        self.require_active()?;
        let Ok(position) = self
            .direct_entry_index
            .binary_search_by_key(&address, |&(entry, _)| entry)
        else {
            return Ok(false);
        };
        let index = self.direct_entry_index[position].1;
        let guard = self.direct_guard(index);
        guard
            .write(
                guards
                    .get_mut(index * REGION_GUARD_BYTES..(index + 1) * REGION_GUARD_BYTES)
                    .ok_or(Error::Protocol("guard table extent"))?,
            )
            .map_err(|_| Error::Protocol("guard table size"))?;
        Ok(guard.valid)
    }

    /// Setup-only protection for all immutable compiled source pages. A direct
    /// store to these pages takes ordinary interpreter fallback instead.
    pub fn direct_code_pages(&self) -> Result<Vec<u8>, Error> {
        let pages = self.session.hardware_config().memory_bytes() / 4096;
        let mut result = Vec::new();
        result
            .try_reserve_exact(pages)
            .map_err(|_| Error::Allocation)?;
        result.resize(pages, 0);
        for region in &self.metadata.regions {
            for part in &region.parts {
                let first = part.linear as usize / 4096;
                let last = (part.linear as u64 + part.expected.len() as u64 - 1) / 4096;
                if first < pages {
                    result[first..=(last as usize).min(pages - 1)].fill(1);
                }
            }
        }
        Ok(result)
    }

    /// Only the wasm32 application may export offsets into its actual memory.
    /// All ephemeral Rust borrows end before generated execution resumes.
    #[cfg(target_arch = "wasm32")]
    pub fn prepare_direct_memory(
        &mut self,
        code_pages: &[u8],
        guards: &[u8],
    ) -> Result<doslet_x86_wasm::static_program::memory::Descriptor, Error> {
        use doslet_x86_wasm::static_program::memory::{
            Descriptor, DeviceReadDescriptor, DeviceReadKind, RamDescriptor, SegmentBounds,
        };
        self.require_active()?;
        // A bus-only helper can refresh the view while counted work remains
        // pending. It neither synchronizes live GPRs nor advances device time.
        let mut descriptor = Descriptor::default();
        descriptor.stack_address = if self.session.registers().stack32() {
            doslet_x86_wasm::static_program::memory::StackAddress::Bits32
        } else {
            doslet_x86_wasm::static_program::memory::StackAddress::Bits16
        };
        descriptor.inline_grant = if self.eligible_context() {
            self.grant
        } else {
            0
        };
        descriptor.region_guards_pointer = guards.as_ptr() as u32;
        descriptor.region_guards_length =
            (guards.len() / doslet_x86_wasm::static_program::memory::REGION_GUARD_BYTES) as u32;
        for (access, windows) in descriptor.windows.iter_mut().enumerate() {
            for (segment, target) in windows.iter_mut().enumerate() {
                if let Some(window) = self.session.registers().window(
                    SegmentRegister::ALL[segment],
                    if access == 0 {
                        Access::Read
                    } else {
                        Access::Write
                    },
                ) {
                    let (base, first, last) = window.parts();
                    *target = SegmentBounds {
                        valid: true,
                        base,
                        first,
                        last,
                    };
                }
            }
        }
        if let Some(view) = self.session.debug_direct_memory() {
            descriptor.ram = RamDescriptor {
                pointer: view.ram.bytes.as_mut_ptr() as u32,
                linear_base: view.ram.linear_base,
                length: view.ram.bytes.len() as u32,
                excluded_start: view.ram.excluded_start,
                excluded_length: view.ram.excluded_len,
                generations_pointer: view.ram.page_generations.as_mut_ptr() as u32,
                generations_length: view.ram.page_generations.len() as u32,
                code_pages_pointer: code_pages.as_ptr() as u32,
                code_pages_length: code_pages.len() as u32,
            };
            if let Some(video) = view.video {
                let mut device = DeviceReadDescriptor {
                    guest_start: 0xa0000,
                    guest_length: 65536,
                    pointer: video.bytes.as_ptr() as u32,
                    backing_length: video.bytes.len() as u32,
                    kind: DeviceReadKind::Linear,
                    base_offset: video.bank_offset,
                    latches: video.latches,
                    ..DeviceReadDescriptor::default()
                };
                if let Some(planar) = video.planar {
                    device.kind = DeviceReadKind::FourLanes;
                    device.base_offset = 0;
                    device.chain4 = planar.chain4;
                    device.read_plane = planar.read_plane;
                    if let Some((compare, care)) = planar.compare {
                        device.compare_enabled = true;
                        device.compare = compare;
                        device.care = care;
                    }
                }
                descriptor.device = device;
            }
        }
        Ok(descriptor)
    }

    pub fn complete_direct_memory(
        &mut self,
        effects: doslet_x86_wasm::static_program::memory::Effects,
    ) -> Result<(), Error> {
        self.require_active()?;
        if effects.inline_reserved != 0 {
            if effects.inline_reserved > self.grant
                || effects.inline_remaining != self.grant - effects.inline_reserved
            {
                return self.fail(Error::Protocol(
                    "inline region reservations exceed hard grant",
                ));
            }
            let Some(pending) = self.pending.checked_add(effects.inline_reserved) else {
                return self.fail(Error::Protocol(
                    "inline region reservation counter overflow",
                ));
            };
            self.pending = pending;
            self.grant = effects.inline_remaining;
        }
        if effects.device_reads > u64::from(self.pending) * 4
            || effects.ram_writes > u64::from(self.pending) * 2
        {
            return self.fail(Error::Protocol(
                "direct memory effects exceed reserved work",
            ));
        }
        self.session.debug_complete_direct_memory(
            effects.device_reads,
            effects.latches,
            effects.ram_writes,
        );
        Ok(())
    }

    /// Side-effect-free test performed before committing a helper's current
    /// reservation. A refusal must leave both descriptor and owner untouched.
    pub fn memory_write_allowed(&mut self, address: u32, bytes: u32) -> Result<bool, Error> {
        self.require_active()?;
        Ok(self
            .session
            .debug_register_engine_memory_span(address, bytes))
    }

    /// Complete only the memory effect of an already-reserved MOV. The generated
    /// region retains live registers and charges the instruction after return.
    pub fn memory_write(&mut self, address: u32, bytes: u32, value: u32) -> Result<(), Error> {
        self.require_active()?;
        if self.pending == 0 {
            return self.fail(Error::Protocol("memory helper has no reserved work"));
        }
        match self
            .session
            .debug_register_engine_write_memory(address, bytes, value)
        {
            Ok(true) => {
                self.counters.memory_write_helpers =
                    self.counters.memory_write_helpers.saturating_add(1);
                Ok(())
            }
            Ok(false) => self.fail(Error::Protocol("reserved memory helper lost its span")),
            Err(error) => self.fail(Error::Runtime(error)),
        }
    }

    /// Validate bounded binding metadata and reserve guard storage at setup.
    /// Changed/unmapped source bytes leave that region on ordinary fallback;
    /// they do not invalidate independent static regions or the owner.
    /// Execution/idle policy selection remains the application's responsibility.
    pub fn new(session: Session, metadata: Metadata) -> Result<Self, Error> {
        validate_metadata(&session, &metadata)?;
        let mut proofs = Vec::new();
        proofs
            .try_reserve_exact(metadata.regions.len())
            .map_err(|_| Error::Allocation)?;
        for region in &metadata.regions {
            let mut parts = Vec::new();
            parts
                .try_reserve_exact(region.parts.len())
                .map_err(|_| Error::Allocation)?;
            for part in &region.parts {
                let matches = session.debug_code_bytes(part.linear, part.expected.len() as u8)
                    == Some(part.expected.as_slice());
                parts.push(if matches {
                    session.debug_code_version(part.linear, part.expected.len() as u8)
                } else {
                    None
                });
            }
            proofs.push(parts);
        }
        let mut wait_entries = Vec::new();
        wait_entries
            .try_reserve_exact(
                metadata
                    .regions
                    .iter()
                    .filter(|r| r.wait_proof_steps != 0)
                    .count(),
            )
            .map_err(|_| Error::Allocation)?;
        wait_entries.extend(
            metadata
                .regions
                .iter()
                .filter(|r| r.wait_proof_steps != 0)
                .map(|r| (r.entry, r.wait_proof_steps)),
        );
        wait_entries.sort_unstable_by_key(|&(entry, _)| entry);
        let mut direct_entry_index = Vec::new();
        direct_entry_index
            .try_reserve_exact(metadata.regions.len())
            .map_err(|_| Error::Allocation)?;
        direct_entry_index.extend(
            metadata
                .regions
                .iter()
                .enumerate()
                .map(|(index, region)| (region.entry, index)),
        );
        direct_entry_index.sort_unstable_by_key(|&(entry, _)| entry);
        Ok(Self {
            session,
            metadata,
            proofs,
            wait_entries,
            direct_entry_index,
            counters: Counters::default(),
            diagnose_fallbacks: false,
            active: false,
            poisoned: false,
            exiting: false,
            grant: 0,
            pending: 0,
            accounted_work: 0,
            boundary_budget: 0,
            empty_services: 0,
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn fallback_diagnostics_enabled(&self) -> bool {
        self.diagnose_fallbacks
    }

    /// Select outside generated execution. Changing policy clears diagnostics;
    /// disabled execution performs no diagnostic source reads or classification.
    pub fn set_fallback_diagnostics(&mut self, enabled: bool) -> Result<(), Error> {
        self.require_idle()?;
        if self.diagnose_fallbacks != enabled {
            self.counters.fallback_diagnostics = FallbackDiagnostics::default();
            self.diagnose_fallbacks = enabled;
        }
        Ok(())
    }

    /// Mutations are available only after a successfully reconciled run.
    pub fn session_mut(&mut self) -> Result<&mut Session, Error> {
        self.require_idle()?;
        Ok(&mut self.session)
    }
    pub fn into_session(self) -> Result<Session, Error> {
        self.require_idle()?;
        Ok(self.session)
    }

    pub fn begin_run(&mut self, budget: u64) -> Result<Frame, Error> {
        self.require_idle()?;
        self.session.debug_prepare_register_engine();
        self.active = true;
        self.exiting = false;
        self.pending = 0;
        self.accounted_work = 0;
        self.boundary_budget = budget;
        self.empty_services = 0;
        self.refresh_grant();
        let mut frame = Frame {
            remaining_budget: budget,
            ..Frame::default()
        };
        self.load_frame(&mut frame);
        Ok(frame)
    }

    /// ABI admission import. Frame and live CPU GPR/IP may both be stale here.
    /// Only immutable code/context proofs and the reserved hard horizon matter.
    pub fn admit(&mut self, region_id: u32, required_steps: u32) -> Result<bool, Error> {
        self.require_active()?;
        let Some(region) = self.metadata.regions.get(region_id as usize) else {
            return self.fail(Error::Protocol("unknown admission region"));
        };
        if required_steps == 0 || required_steps != region.required_steps {
            return self.fail(Error::Protocol("admission count differs from metadata"));
        }
        if !self.eligible_context() || required_steps > self.grant {
            return Ok(self.refuse());
        }
        if self
            .accounted_work
            .checked_add(u64::from(self.pending) + u64::from(required_steps))
            .is_none()
        {
            return Ok(self.refuse());
        }
        for (part, proof) in region
            .parts
            .iter()
            .zip(&mut self.proofs[region_id as usize])
        {
            if proof
                .as_ref()
                .is_some_and(|version| self.session.debug_code_valid(version))
            {
                continue;
            }
            if self
                .session
                .debug_code_bytes(part.linear, part.expected.len() as u8)
                != Some(part.expected.as_slice())
            {
                return Ok(self.refuse());
            }
            *proof = self
                .session
                .debug_code_version(part.linear, part.expected.len() as u8);
        }
        // Reservation alone changes neither architectural state nor device time.
        self.grant -= required_steps;
        self.pending += required_steps;
        Ok(true)
    }

    pub fn callback(&mut self, frame: &mut Frame, reason: u32) -> Result<Control, Error> {
        self.require_active()?;
        if reason != abi::REASON_SERVICE && reason != abi::REASON_FALLBACK {
            return self.fail(Error::Protocol("unknown callback reason"));
        }
        self.counters.callbacks = self.counters.callbacks.saturating_add(1);
        let translated = self.reconcile(frame)?;
        if self.diagnose_fallbacks {
            // Preserve the grant seen by generated code: refreshing it first
            // would conceal a refusal at the end of the preceding reservation.
            self.record_callback_diagnostic(frame.address, reason);
        }
        if !self.can_continue(frame) {
            return Ok(self.exit());
        }
        let previous_grant = self.grant;
        self.session.debug_prepare_register_engine();
        self.refresh_grant();
        let retry = reason == abi::REASON_FALLBACK
            && self.refresh_stale_entry(frame.address, previous_grant);
        // A service at a device/IRQ boundary must take the ordinary runtime path
        // even when the current static address still has a translated region.
        let interpret = reason == abi::REASON_FALLBACK && !retry || self.grant == 0;
        let mut completed = translated != 0;
        if interpret {
            if frame.charged_work == u64::MAX {
                return self.fail(Error::Protocol("completed-work counter exhausted"));
            }
            let before = self.session.steps();
            let hint = (reason == abi::REASON_FALLBACK)
                .then(|| {
                    self.wait_entries
                        .binary_search_by_key(&frame.address, |&(entry, _)| entry)
                        .ok()
                })
                .flatten();
            let skipped_before = self.session.idle_loop_stats().skipped_steps;
            let allowed = if hint.is_some() {
                frame
                    .remaining_budget
                    .min(65_536)
                    .min(u64::MAX - frame.charged_work) as u32
            } else {
                1
            };
            let actual = if let Some(index) = hint {
                self.counters.wait_calls = self.counters.wait_calls.saturating_add(1);
                self.session
                    .debug_advance_wait_candidate(allowed, self.wait_entries[index].1)
            } else {
                self.session.advance(allowed)
            };
            if actual > allowed
                || self.session.steps().checked_sub(before) != Some(u64::from(actual))
            {
                return self.fail(Error::Protocol("ordinary fallback accounting differs"));
            }
            let skipped = self
                .session
                .idle_loop_stats()
                .skipped_steps
                .saturating_sub(skipped_before);
            if skipped > u64::from(actual) {
                return self.fail(Error::Protocol("idle accounting exceeds completed work"));
            }
            self.counters.skipped_wait_steps =
                self.counters.skipped_wait_steps.saturating_add(skipped);
            self.counters.interpreted_steps = self
                .counters
                .interpreted_steps
                .saturating_add(u64::from(actual) - skipped);
            frame.charged_work += u64::from(actual);
            frame.remaining_budget = frame.remaining_budget.saturating_sub(u64::from(actual));
            self.accounted_work = frame.charged_work;
            self.boundary_budget = frame.remaining_budget;
            self.load_frame(frame);
            completed |= actual != 0;
            if !self.can_continue(frame) {
                return Ok(self.exit());
            }
            if actual == 0 {
                return self.fail(Error::NoProgress);
            }
            self.refresh_grant();
        }
        // Renewing an immutable proof permits only one unchanged-entry retry:
        // another refusal now has current proofs and takes ordinary execution.
        if completed || retry {
            self.empty_services = 0;
        } else {
            self.empty_services += 1;
            if self.empty_services >= MAX_EMPTY_SERVICES {
                return self.fail(Error::NoProgress);
            }
        }
        Ok(Control::Continue)
    }

    /// Reconcile every generated-code return, including direct soft-budget exits.
    /// Safe to call again after success, and after a callback requested Exit.
    pub fn finish_run(&mut self, frame: &mut Frame) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Protocol("owner is poisoned"));
        }
        if !self.active {
            return Ok(());
        }
        self.reconcile(frame)?;
        self.active = false;
        self.exiting = false;
        self.grant = 0;
        Ok(())
    }

    fn require_idle(&self) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Protocol("owner is poisoned"));
        }
        if self.active {
            return Err(Error::Protocol("run is still active"));
        }
        Ok(())
    }
    fn require_active(&mut self) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Protocol("owner is poisoned"));
        }
        if !self.active || self.exiting {
            return self.fail(Error::Protocol("no active continuing run"));
        }
        Ok(())
    }
    fn fail<T>(&mut self, error: Error) -> Result<T, Error> {
        self.poisoned = true;
        Err(error)
    }
    fn refuse(&mut self) -> bool {
        self.counters.guard_refusals = self.counters.guard_refusals.saturating_add(1);
        false
    }
    fn exit(&mut self) -> Control {
        self.exiting = true;
        self.grant = 0;
        Control::Exit
    }
    fn can_continue(&self, frame: &Frame) -> bool {
        frame.remaining_budget != 0
            && self.session.status() == SessionStatus::Running
            && !self.session.storage_waiting()
            && !self.session.timeslice_yielded()
    }
    fn eligible_context(&self) -> bool {
        let cpu = self.session.debug_cpu();
        cpu.control.cr0 & 0x8000_0001 == 1
            && !cpu.is_halted()
            && !cpu.has_interrupt_shadow()
            && cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) == 0
            && cpu.registers.code32()
            && cpu.registers.cs == self.metadata.context.cs.selector
            && cpu.registers.segment_cache(SegmentRegister::Cs) == self.metadata.context.cs
    }

    /// Preserve a compiled entry after an unrelated source-page write. Check
    /// generations first; ordinary memory refusals need no source-byte scans.
    /// The caller refreshes the shared guard table before generated execution.
    fn refresh_stale_entry(&mut self, address: Address, previous_grant: u32) -> bool {
        let Ok(position) = self
            .direct_entry_index
            .binary_search_by_key(&address, |&(entry, _)| entry)
        else {
            return false;
        };
        let index = self.direct_entry_index[position].1;
        let required = self.metadata.regions[index].required_steps;
        if required == 0
            || required > previous_grant.min(self.grant)
            || !self.eligible_context()
            || !self.proofs[index].iter().any(|proof| {
                proof
                    .as_ref()
                    .is_none_or(|version| !self.session.debug_code_valid(version))
            })
        {
            return false;
        }
        self.direct_guard(index).valid
    }

    /// Read-only explanation of this exact dispatcher entry, before ordinary
    /// execution or guard refresh. This never renews proofs or touches guards.
    fn fallback_cause(&self, address: Address) -> FallbackCause {
        let Ok(position) = self
            .direct_entry_index
            .binary_search_by_key(&address, |&(entry, _)| entry)
        else {
            return FallbackCause::UnknownEntry;
        };
        let index = self.direct_entry_index[position].1;
        let region = &self.metadata.regions[index];
        if region.required_steps == 0 {
            return FallbackCause::UnsupportedStub;
        }
        if !self.eligible_context() {
            return FallbackCause::ContextRefused;
        }
        if region.required_steps > self.grant {
            return FallbackCause::HardGrant {
                required: region.required_steps,
                available: self.grant,
            };
        }
        if region.parts.iter().any(|part| {
            self.session
                .debug_code_bytes(part.linear, part.expected.len() as u8)
                != Some(part.expected.as_slice())
        }) {
            return FallbackCause::SourceChanged;
        }
        if self.proofs[index].iter().any(|proof| {
            proof
                .as_ref()
                .is_none_or(|version| !self.session.debug_code_valid(version))
        }) {
            return FallbackCause::StaleProof;
        }
        FallbackCause::MemoryOrGuard
    }

    fn record_callback_diagnostic(&mut self, address: Address, reason: u32) {
        if reason == abi::REASON_SERVICE {
            let count = &mut self.counters.fallback_diagnostics.service_callbacks;
            *count = count.saturating_add(1);
            return;
        }
        let cause = self.fallback_cause(address);
        let counts = &mut self.counters.fallback_diagnostics;
        counts.fallback_callbacks = counts.fallback_callbacks.saturating_add(1);
        let count = match cause {
            FallbackCause::UnknownEntry => &mut counts.unknown_entry,
            FallbackCause::UnsupportedStub => &mut counts.unsupported_stub,
            FallbackCause::ContextRefused => &mut counts.context_refused,
            FallbackCause::HardGrant {
                required,
                available,
            } => {
                counts.hard_grant_required_steps_sum = counts
                    .hard_grant_required_steps_sum
                    .saturating_add(u64::from(required));
                counts.hard_grant_available_steps_sum = counts
                    .hard_grant_available_steps_sum
                    .saturating_add(u64::from(available));
                counts.hard_grant_required_steps_max =
                    counts.hard_grant_required_steps_max.max(required);
                counts.hard_grant_available_steps_max =
                    counts.hard_grant_available_steps_max.max(available);
                &mut counts.hard_grant
            }
            FallbackCause::SourceChanged => &mut counts.source_changed,
            FallbackCause::StaleProof => &mut counts.stale_proof,
            FallbackCause::MemoryOrGuard => &mut counts.memory_or_guard,
        };
        *count = count.saturating_add(1);
    }
    fn refresh_grant(&mut self) {
        // The host budget is intentionally separate: a whole region may exceed
        // its last positive soft-budget remainder, but never the hardware grant.
        self.grant = if self.session.status() == SessionStatus::Running
            && !self.session.storage_waiting()
            && !self.session.timeslice_yielded()
        {
            self.session.debug_register_engine_budget(u32::MAX)
        } else {
            0
        };
    }
    fn load_frame(&self, frame: &mut Frame) {
        frame.gpr =
            doslet_runtime::session::debug::GENERAL.map(|reg| self.session.registers().get32(reg));
        frame.eflags = self.session.eflags();
        frame.address = self.symbolic_address(self.session.debug_pc());
    }
    fn symbolic_address(&self, linear: u32) -> Address {
        for object in &self.metadata.objects {
            if let Some(offset) = linear.checked_sub(object.linear_base)
                && offset < object.size
            {
                return Address {
                    space: object.space,
                    offset,
                };
            }
        }
        Address {
            space: 0,
            offset: linear,
        }
    }
    fn reconcile(&mut self, frame: &Frame) -> Result<u32, Error> {
        if frame.exact_total().is_none() {
            return self.fail(Error::Protocol("counted execution lost exact accounting"));
        }
        if frame.charged_work.checked_sub(self.accounted_work) != Some(u64::from(self.pending)) {
            return self.fail(Error::Protocol(
                "completed work differs from reserved regions",
            ));
        }
        if frame.remaining_budget != self.boundary_budget.saturating_sub(u64::from(self.pending)) {
            return self.fail(Error::Protocol("soft budget differs from completed work"));
        }
        let Some(linear) = resolve_address(&self.metadata.objects, frame.address, true) else {
            return self.fail(Error::Protocol("unbound frame address"));
        };
        let cpu = self.session.debug_cpu();
        if (frame.eflags ^ cpu.flags) & !(ARITHMETIC_FLAGS | 2) != 0 {
            return self.fail(Error::Protocol("translated frame changed control flags"));
        }
        if self.pending != 0
            && (!self.eligible_context()
                || self.session.debug_register_engine_budget(self.pending) < self.pending)
        {
            return self.fail(Error::Protocol(
                "reserved context or hardware horizon changed",
            ));
        }
        let count = self.pending;
        let cpu = self.session.debug_cpu_mut();
        let base = cpu.registers.segment_cache(SegmentRegister::Cs).base;
        for (reg, value) in doslet_runtime::session::debug::GENERAL
            .into_iter()
            .zip(frame.gpr)
        {
            cpu.registers.set32(reg, value);
        }
        cpu.flags = frame.eflags | 2;
        cpu.registers.ip = linear.wrapping_sub(base);
        // Clear the reservation before a fallible clock advance: errors poison
        // the owner and cannot accidentally charge the same work a second time.
        self.pending = 0;
        self.accounted_work = frame.charged_work;
        self.boundary_budget = frame.remaining_budget;
        if count != 0 {
            if let Err(error) = self.session.debug_account_register_engine(count) {
                return self.fail(Error::Runtime(error));
            }
            self.counters.translated_steps = self
                .counters
                .translated_steps
                .saturating_add(u64::from(count));
        }
        Ok(count)
    }
}

fn resolve_address(objects: &[ObjectBinding], address: Address, allow_end: bool) -> Option<u32> {
    if address.space == 0 {
        return Some(address.offset);
    }
    let object = objects
        .iter()
        .find(|object| object.space == address.space)?;
    if address.offset > object.size || (!allow_end && address.offset == object.size) {
        return None;
    }
    object.linear_base.checked_add(address.offset)
}

fn validate_metadata(session: &Session, metadata: &Metadata) -> Result<(), Error> {
    let invalid = |message| Error::Metadata(message);
    if metadata.objects.is_empty()
        || metadata.objects.len() > MAX_OBJECTS
        || metadata.regions.len() > MAX_REGIONS
    {
        return Err(invalid("object/region count is out of bounds"));
    }
    let context = metadata.context;
    let cpu = session.debug_cpu();
    if !context.cs.default32
        || !context.cs.usable
        || context.cs.access & 0x98 != 0x98
        || cpu.control.cr0 & 0x8000_0001 != 1
        || cpu.registers.cs != context.cs.selector
        || cpu.registers.segment_cache(SegmentRegister::Cs) != context.cs
    {
        return Err(invalid(
            "binding requires the actual 32-bit protected code context without paging",
        ));
    }
    let mut object_order = Vec::new();
    object_order
        .try_reserve_exact(metadata.objects.len())
        .map_err(|_| Error::Allocation)?;
    for object in &metadata.objects {
        if object.space == 0
            || object.size == 0
            || u64::from(object.linear_base) + u64::from(object.size) > 1u64 << 32
        {
            return Err(invalid("invalid object identity or extent"));
        }
        object_order.push(object);
    }
    object_order.sort_unstable_by_key(|object| object.space);
    if object_order
        .windows(2)
        .any(|pair| pair[0].space == pair[1].space)
    {
        return Err(invalid("duplicate object identity"));
    }
    object_order.sort_unstable_by_key(|object| object.linear_base);
    if object_order.windows(2).any(|pair| {
        u64::from(pair[0].linear_base) + u64::from(pair[0].size) > u64::from(pair[1].linear_base)
    }) {
        return Err(invalid("object extents overlap"));
    }
    object_order.sort_unstable_by_key(|object| object.space);
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(metadata.regions.len())
        .map_err(|_| Error::Allocation)?;
    let mut total_parts = 0usize;
    let mut total_bytes = 0usize;
    for (index, region) in metadata.regions.iter().enumerate() {
        if region.id as usize != index || region.required_steps as usize > MAX_REGION_INSTRUCTIONS {
            return Err(invalid("region IDs must be dense and counts bounded"));
        }
        if region.wait_proof_steps > 4096
            || region.wait_proof_steps != 0 && region.required_steps != 0
        {
            return Err(invalid(
                "wait hints require zero-work stubs and bounded proofs",
            ));
        }
        let object = object_order
            .binary_search_by_key(&region.entry.space, |object| object.space)
            .ok()
            .map(|index| object_order[index])
            .ok_or_else(|| invalid("region entry is outside its object"))?;
        if region.entry.offset >= object.size {
            return Err(invalid("region entry is outside its object"));
        }
        let entry = object
            .linear_base
            .checked_add(region.entry.offset)
            .ok_or_else(|| invalid("region entry address overflow"))?;
        if region.required_steps != 0 && !object.default32 {
            return Err(invalid("translated regions require a 32-bit code object"));
        }
        if (region.required_steps == 0) != region.parts.is_empty() {
            return Err(invalid(
                "nonempty regions need source bytes; fallback stubs have none",
            ));
        }
        if !region.parts.is_empty() && region.parts[0].linear != entry {
            return Err(invalid("first code part must start at the region entry"));
        }
        total_parts = total_parts
            .checked_add(region.parts.len())
            .ok_or_else(|| invalid("too many code parts"))?;
        if total_parts > MAX_CODE_PARTS {
            return Err(invalid("too many code parts"));
        }
        let mut next_linear = u64::from(entry);
        let mut region_bytes = 0usize;
        for part in &region.parts {
            if part.expected.is_empty() || part.expected.len() > u8::MAX as usize {
                return Err(invalid("code-part length must be 1..=255"));
            }
            if u64::from(part.linear) != next_linear {
                return Err(invalid(
                    "code parts must be contiguous from the region entry",
                ));
            }
            next_linear += part.expected.len() as u64;
            region_bytes += part.expected.len();
            let offset = part
                .linear
                .checked_sub(object.linear_base)
                .ok_or_else(|| invalid("code part is outside its object"))?;
            if u64::from(offset) + part.expected.len() as u64 > u64::from(object.size)
                || cpu.registers.address(
                    SegmentRegister::Cs,
                    part.linear.wrapping_sub(context.cs.base),
                    part.expected.len() as u8,
                    Access::Execute,
                ) != Ok(part.linear)
            {
                return Err(invalid(
                    "code part is outside its object or executable CS range",
                ));
            }
            total_bytes = total_bytes
                .checked_add(part.expected.len())
                .ok_or_else(|| invalid("too many code bytes"))?;
            if total_bytes > MAX_CODE_BYTES {
                return Err(invalid("too many code bytes"));
            }
        }
        if region_bytes < region.required_steps as usize {
            return Err(invalid(
                "source byte count cannot cover the completed instructions",
            ));
        }
        entries.push(region.entry);
    }
    entries.sort_unstable();
    if entries.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(invalid("duplicate region entry"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_x86::Gpr32;

    const CODE: u32 = 0x10130;

    /// Enter actual protected mode through guest instructions and a GDT. The
    /// fixture does not install descriptors through private CPU/runtime fields.
    fn protected_session(body: &[u8]) -> Session {
        assert!(body.len() <= 0x50);
        let mut program = vec![0x90; 0xB0];
        program[..24].copy_from_slice(&[
            0xFA, // CLI
            0x0F, 0x01, 0x16, 0xA0, 0x01, // LGDT [01A0]
            0x0F, 0x20, 0xC0, // MOV EAX,CR0
            0x66, 0x83, 0xC8, 0x01, // OR EAX,1
            0x0F, 0x22, 0xC0, // MOV CR0,EAX
            0x66, 0xEA, 0x30, 0x01, 0x01, 0, 8, 0, // JMP 0008:00010130
        ]);
        program[0x30..0x30 + body.len()].copy_from_slice(body);
        program[0x80..0x88].fill(0);
        for offset in [0x88, 0x90] {
            program[offset..offset + 8].copy_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0x9B, 0xCF, 0]);
        }
        program[0x98..0xA0].copy_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0x93, 0xCF, 0]);
        program[0xA0..0xA6].copy_from_slice(&[0x1F, 0, 0x80, 0x01, 0x01, 0]);
        let mut session = Session::new(&program).unwrap();
        session.set_dynamic_core(false);
        session.set_decode_cache(false);
        session.set_idle_loops(false);
        session.set_vga_wait_acceleration(false);
        session.set_synchronization_analysis(false);
        session.set_adaptive_idle(false);
        session.set_idle_observation(false);
        session.start();
        assert_eq!(session.advance(6), 6, "{:?}", session.fault());
        assert_eq!(session.debug_pc(), CODE);
        assert!(session.debug_cpu().protected_mode());
        assert!(session.registers().code32());
        session
    }

    fn metadata(session: &Session, bytes: &[u8], steps: u32) -> Metadata {
        Metadata {
            objects: vec![ObjectBinding {
                space: 1,
                linear_base: CODE,
                size: 0x50,
                default32: true,
            }],
            regions: vec![RegionMetadata {
                id: 0,
                entry: Address {
                    space: 1,
                    offset: 0,
                },
                required_steps: steps,
                wait_proof_steps: 0,
                parts: if steps == 0 {
                    vec![]
                } else {
                    vec![CodePart {
                        linear: CODE,
                        expected: bytes.to_vec(),
                    }]
                },
            }],
            context: CodeContext {
                cs: session.registers().segment_cache(SegmentRegister::Cs),
            },
        }
    }

    fn owner(bytes: &[u8], steps: u32) -> StaticSession {
        let session = protected_session(bytes);
        let metadata = metadata(&session, bytes, steps);
        StaticSession::new(session, metadata).unwrap()
    }

    fn reference_session(session: &Session) -> Session {
        // Portable state includes acceleration policy, while a diagnostic
        // WorkingState restore intentionally starts with independent defaults.
        Session::load_state(&session.save_state().unwrap()).unwrap()
    }

    fn completed_frame(owner: &StaticSession, reference: &Session, frame: &mut Frame, count: u32) {
        frame.gpr =
            doslet_runtime::session::debug::GENERAL.map(|reg| reference.registers().get32(reg));
        frame.eflags = reference.eflags();
        frame.address = owner.symbolic_address(reference.debug_pc());
        frame.charged_work += u64::from(count);
        frame.remaining_budget = frame.remaining_budget.saturating_sub(u64::from(count));
    }

    fn assert_saved_equal(a: &Session, b: &Session) {
        let a = a.save_state().unwrap();
        let b = b.save_state().unwrap();
        let first = a.iter().zip(&b).position(|(a, b)| a != b);
        assert!(
            a == b,
            "wire differs at {first:?}; lengths {}/{}",
            a.len(),
            b.len()
        );
    }

    fn diagnostic_total(counts: FallbackDiagnostics) -> u64 {
        counts.unknown_entry
            + counts.unsupported_stub
            + counts.context_refused
            + counts.hard_grant
            + counts.source_changed
            + counts.stale_proof
            + counts.memory_or_guard
    }

    #[test]
    fn fallback_classifier_is_exclusive_and_preserves_state_and_proofs() {
        for expected in [
            FallbackCause::UnknownEntry,
            FallbackCause::UnsupportedStub,
            FallbackCause::ContextRefused,
            FallbackCause::HardGrant {
                required: 1,
                available: 0,
            },
            FallbackCause::SourceChanged,
            FallbackCause::StaleProof,
            FallbackCause::MemoryOrGuard,
        ] {
            let mut owner = owner(
                &[0x40, 0x90],
                u32::from(expected != FallbackCause::UnsupportedStub),
            );
            owner.set_fallback_diagnostics(true).unwrap();
            if expected == FallbackCause::ContextRefused {
                owner.session.debug_cpu_mut().flags |= flags::TRAP;
            }
            if matches!(
                expected,
                FallbackCause::SourceChanged | FallbackCause::StaleProof
            ) {
                let linear = if expected == FallbackCause::SourceChanged {
                    CODE
                } else {
                    CODE + 16
                };
                assert!(owner.session.debug_write_memory(linear, &[0x48]));
            }
            let mut frame = owner.begin_run(2).unwrap();
            if expected == FallbackCause::UnknownEntry {
                frame.address.offset += 1; // An interior instruction is no entry.
            }
            if matches!(expected, FallbackCause::HardGrant { .. }) {
                owner.grant = 0;
            }
            let state = owner.session.save_state().unwrap();
            let proofs: Vec<_> = owner.proofs[0]
                .iter()
                .map(|proof| proof.map(CodeVersion::parts))
                .collect();
            let grant = owner.grant;
            assert_eq!(owner.fallback_cause(frame.address), expected);
            owner.record_callback_diagnostic(frame.address, abi::REASON_FALLBACK);
            owner.record_callback_diagnostic(frame.address, abi::REASON_SERVICE);
            assert_eq!(owner.session.save_state().unwrap(), state);
            assert_eq!(owner.grant, grant);
            assert_eq!(
                owner.proofs[0]
                    .iter()
                    .map(|proof| proof.map(CodeVersion::parts))
                    .collect::<Vec<_>>(),
                proofs
            );
            let counts = owner.counters().fallback_diagnostics;
            assert_eq!(diagnostic_total(counts), counts.fallback_callbacks);
            assert_eq!(counts.fallback_callbacks, 1);
            assert_eq!(counts.service_callbacks, 1);
        }
    }

    #[test]
    fn fallback_source_comparison_covers_all_parts_before_stale_proof() {
        let session = protected_session(&[0x40, 0x90]);
        let mut metadata = metadata(&session, &[0x40], 1);
        metadata.regions[0].parts.push(CodePart {
            linear: CODE + 1,
            expected: vec![0x90],
        });
        let mut owner = StaticSession::new(session, metadata).unwrap();
        assert!(owner.session.debug_write_memory(CODE + 16, &[0x48]));
        let frame = owner.begin_run(2).unwrap();
        assert_eq!(
            owner.fallback_cause(frame.address),
            FallbackCause::StaleProof
        );
        assert!(owner.session.debug_write_memory(CODE + 1, &[0x40]));
        assert_eq!(
            owner.fallback_cause(frame.address),
            FallbackCause::SourceChanged
        );
    }

    #[test]
    fn fallback_diagnostics_default_off_and_enabled_callbacks_have_identical_state() {
        let mut disabled = owner(&[0x40, 0xEB, 0xFD], 2);
        let mut enabled = StaticSession::new(
            reference_session(disabled.session()),
            disabled.metadata().clone(),
        )
        .unwrap();
        assert!(!disabled.fallback_diagnostics_enabled());
        enabled.set_fallback_diagnostics(true).unwrap();
        for owner in [&mut disabled, &mut enabled] {
            let mut frame = owner.begin_run(4).unwrap();
            assert!(owner.set_fallback_diagnostics(false).is_err());
            assert_eq!(
                owner.callback(&mut frame, abi::REASON_SERVICE).unwrap(),
                Control::Continue
            );
            for _ in 0..4 {
                owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap();
            }
            owner.finish_run(&mut frame).unwrap();
        }
        assert_saved_equal(disabled.session(), enabled.session());
        assert_eq!(
            disabled.counters().fallback_diagnostics,
            FallbackDiagnostics::default()
        );
        let counts = enabled.counters().fallback_diagnostics;
        assert_eq!(counts.fallback_callbacks, 4);
        assert_eq!(counts.service_callbacks, 1);
        assert_eq!(counts.unknown_entry, 2);
        assert_eq!(counts.memory_or_guard, 2);
        assert_eq!(diagnostic_total(counts), counts.fallback_callbacks);
        enabled.set_fallback_diagnostics(false).unwrap();
        assert_eq!(
            enabled.counters().fallback_diagnostics,
            FallbackDiagnostics::default()
        );
    }

    #[test]
    fn fallback_diagnostic_uses_pre_refresh_grant_and_saturates() {
        let mut owner = owner(&[0x90], 1);
        owner.set_fallback_diagnostics(true).unwrap();
        let mut frame = owner.begin_run(1).unwrap();
        owner.grant = 0;
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Exit
        );
        assert_eq!(owner.counters().fallback_diagnostics.hard_grant, 1);
        assert_eq!(
            owner
                .counters()
                .fallback_diagnostics
                .hard_grant_required_steps_sum,
            1
        );
        assert_eq!(
            owner
                .counters()
                .fallback_diagnostics
                .hard_grant_available_steps_sum,
            0
        );
        assert_eq!(
            owner
                .counters()
                .fallback_diagnostics
                .hard_grant_required_steps_max,
            1
        );
        assert_eq!(
            owner
                .counters()
                .fallback_diagnostics
                .hard_grant_available_steps_max,
            0
        );
        owner.finish_run(&mut frame).unwrap();
        owner.counters.fallback_diagnostics = FallbackDiagnostics {
            fallback_callbacks: u64::MAX,
            unknown_entry: u64::MAX,
            service_callbacks: u64::MAX,
            ..FallbackDiagnostics::default()
        };
        let unknown = Address {
            space: 0,
            offset: 0,
        };
        owner.record_callback_diagnostic(unknown, abi::REASON_FALLBACK);
        owner.record_callback_diagnostic(unknown, abi::REASON_SERVICE);
        assert_eq!(
            owner.counters().fallback_diagnostics.fallback_callbacks,
            u64::MAX
        );
        assert_eq!(
            owner.counters().fallback_diagnostics.unknown_entry,
            u64::MAX
        );
        assert_eq!(
            owner.counters().fallback_diagnostics.service_callbacks,
            u64::MAX
        );
    }

    #[test]
    fn reservation_has_no_effect_and_soft_return_matches_complete_reference_state() {
        let mut owner = owner(&[0x40, 0x40], 2);
        let mut reference = reference_session(&owner.session);
        assert_saved_equal(&owner.session, &reference);
        let before_cpu = owner.session.debug_cpu().snapshot();
        let before_time = owner.session.emulated_nanos();
        let before_steps = owner.session.steps();
        let mut frame = owner.begin_run(1).unwrap();
        assert!(owner.admit(0, 2).unwrap());
        assert!(owner.session.debug_cpu().matches(&before_cpu));
        assert_eq!(owner.session.emulated_nanos(), before_time);
        assert_eq!(owner.session.steps(), before_steps);
        assert_eq!(reference.advance(2), 2);
        completed_frame(&owner, &reference, &mut frame, 2);
        assert_eq!(frame.remaining_budget, 0); // One whole-region soft overshoot.
        owner.finish_run(&mut frame).unwrap();
        owner.finish_run(&mut frame).unwrap(); // Never account a completed run twice.
        assert_eq!(owner.session.steps(), reference.steps());
        assert_eq!(owner.session.emulated_nanos(), reference.emulated_nanos());
        assert!(
            owner
                .session
                .debug_cpu()
                .matches(&reference.debug_cpu().snapshot())
        );
        assert_saved_equal(&owner.session, &reference);
        assert_eq!(owner.counters().translated_steps, 2);
        assert_eq!(owner.counters().interpreted_steps, 0);
    }

    #[test]
    fn service_commits_prior_work_and_fallback_does_not_double_charge() {
        let mut owner = owner(&[0x40, 0x40, 0x90], 2);
        let mut reference = reference_session(&owner.session);
        let mut frame = owner.begin_run(5).unwrap();
        assert!(owner.admit(0, 2).unwrap());
        assert_eq!(reference.advance(2), 2);
        completed_frame(&owner, &reference, &mut frame, 2);
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_SERVICE).unwrap(),
            Control::Continue
        );
        assert_eq!(owner.session.steps(), reference.steps());
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert_eq!(reference.advance(1), 1);
        assert_eq!(frame.charged_work, 3);
        assert_eq!(frame.remaining_budget, 2);
        owner.finish_run(&mut frame).unwrap();
        assert_saved_equal(&owner.session, &reference);
        assert_eq!(
            owner.counters(),
            Counters {
                translated_steps: 2,
                interpreted_steps: 1,
                callbacks: 2,
                guard_refusals: 0,
                ..Counters::default()
            }
        );
    }

    #[test]
    fn stale_page_proof_rechecks_unchanged_bytes_and_rejects_modified_opcode() {
        let mut owner = owner(&[0x40], 1);
        // An unrelated same-page edit invalidates a version without invalidating
        // the exact source bytes. Admission must refresh, rather than reject it.
        assert!(
            owner
                .session_mut()
                .unwrap()
                .debug_write_memory(CODE + 16, &[0x91])
        );
        let mut frame = owner.begin_run(1).unwrap();
        assert!(owner.admit(0, 1).unwrap());
        // INC's exact flags come from an ordinary one-step reference.
        let mut reference = reference_session(&owner.session);
        assert_eq!(reference.advance(1), 1);
        completed_frame(&owner, &reference, &mut frame, 1);
        owner.finish_run(&mut frame).unwrap();
        let session = owner.session_mut().unwrap();
        session.debug_cpu_mut().registers.ip = CODE;
        assert!(session.debug_write_memory(CODE, &[0x48])); // DEC replaces INC.
        let mut frame = owner.begin_run(1).unwrap();
        let before = owner.session.debug_cpu().snapshot();
        let time = owner.session.emulated_nanos();
        assert!(!owner.admit(0, 1).unwrap());
        assert_eq!(owner.pending, 0);
        assert!(owner.session.debug_cpu().matches(&before));
        assert_eq!(owner.session.emulated_nanos(), time);
        let eax = frame.gpr[0];
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Exit
        );
        owner.finish_run(&mut frame).unwrap();
        assert_eq!(
            owner.session.registers().get32(Gpr32::Eax),
            eax.wrapping_sub(1)
        );
        assert_eq!(frame.charged_work, 1);
        assert_eq!(owner.counters().guard_refusals, 1);
    }

    #[test]
    fn stale_source_retry_preserves_entry_and_following_memory_refusal_interprets() {
        // A direct-memory preflight may refuse this low-memory load before the
        // inline source guard runs. A refreshed proof must not retry it forever.
        let mut owner = owner(&[0xA1, 0, 2, 0, 0], 1);
        assert!(
            owner
                .session_mut()
                .unwrap()
                .debug_write_memory(CODE + 16, &[0x91])
        );
        let mut reference = reference_session(owner.session());
        let mut frame = owner.begin_run(2).unwrap();
        let entry_frame = frame;
        let state = owner.session.save_state().unwrap();
        owner.empty_services = MAX_EMPTY_SERVICES - 1;
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert_eq!(frame, entry_frame);
        assert_eq!(owner.session.save_state().unwrap(), state);
        assert_eq!(owner.counters().interpreted_steps, 0);
        assert_eq!(owner.empty_services, 0);
        assert_eq!(
            owner.fallback_cause(frame.address),
            FallbackCause::MemoryOrGuard
        );

        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert_eq!(reference.advance(1), 1);
        assert_eq!(frame.address.offset, 5);
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.remaining_budget, 1);
        assert_eq!(owner.counters().interpreted_steps, 1);
        owner.finish_run(&mut frame).unwrap();
        assert_saved_equal(owner.session(), &reference);
    }

    #[test]
    fn stale_source_retry_rejects_changed_bytes_in_a_later_part() {
        let session = protected_session(&[0x40, 0x90]);
        let mut metadata = metadata(&session, &[0x40], 2);
        metadata.regions[0].parts.push(CodePart {
            linear: CODE + 1,
            expected: vec![0x90],
        });
        let mut owner = StaticSession::new(session, metadata).unwrap();
        assert!(
            owner
                .session_mut()
                .unwrap()
                .debug_write_memory(CODE + 1, &[0x48])
        );
        let mut reference = reference_session(owner.session());
        let mut frame = owner.begin_run(2).unwrap();
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert_eq!(reference.advance(1), 1);
        assert_eq!(frame.address.offset, 1);
        assert_eq!(frame.charged_work, 1);
        assert_eq!(owner.counters().interpreted_steps, 1);
        owner.finish_run(&mut frame).unwrap();
        assert_saved_equal(owner.session(), &reference);
    }

    #[test]
    fn stale_source_retry_keeps_a_short_pre_refresh_hardware_grant_on_fallback() {
        let mut owner = owner(&[0x40, 0x40], 2);
        assert!(
            owner
                .session_mut()
                .unwrap()
                .debug_write_memory(CODE + 16, &[0x91])
        );
        let mut reference = reference_session(owner.session());
        let mut frame = owner.begin_run(2).unwrap();
        owner.grant = 1; // Only narrow the real hardware grant.
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert_eq!(reference.advance(1), 1);
        assert_eq!(frame.charged_work, 1);
        assert_eq!(owner.counters().interpreted_steps, 1);
        owner.finish_run(&mut frame).unwrap();
        assert_saved_equal(owner.session(), &reference);
    }

    #[test]
    fn partial_hardware_grant_refuses_whole_region_before_any_effect() {
        let mut owner = owner(&[0x40, 0x40], 2);
        let mut frame = owner.begin_run(10).unwrap();
        owner.grant = 1; // Narrow an actual event-free grant; never enlarge it.
        let before = owner.session.debug_cpu().snapshot();
        let steps = owner.session.steps();
        assert!(!owner.admit(0, 2).unwrap());
        assert_eq!(owner.grant, 1);
        assert_eq!(owner.pending, 0);
        assert!(owner.session.debug_cpu().matches(&before));
        assert_eq!(owner.session.steps(), steps);
        owner.finish_run(&mut frame).unwrap();
    }

    #[test]
    fn exact_real_hardware_horizon_is_reserved_and_accounted_without_crossing_event() {
        let mut owner = owner(&[0x40, 0xEB, 0xFD], 2);
        let mut reference = reference_session(&owner.session);
        let mut frame = owner.begin_run(u64::MAX).unwrap();
        let hard_grant = owner.grant;
        assert!(hard_grant > 0);
        let mut total = 0;
        while owner.admit(0, 2).unwrap() {
            total += 2;
            assert!(total <= hard_grant);
        }
        assert_eq!(total, hard_grant / 2 * 2);
        assert!(owner.grant < 2);
        assert_eq!(reference.advance(total), total);
        completed_frame(&owner, &reference, &mut frame, total);
        owner.finish_run(&mut frame).unwrap();
        assert_saved_equal(&owner.session, &reference);
    }

    #[test]
    fn changed_cpu_control_state_refuses_without_architectural_or_clock_effects() {
        for prohibited in [flags::TRAP, flags::RESUME, flags::VIRTUAL] {
            let mut owner = owner(&[0x90], 1);
            owner.session_mut().unwrap().debug_cpu_mut().flags |= prohibited;
            let mut frame = owner.begin_run(1).unwrap();
            let before = owner.session.debug_cpu().snapshot();
            let time = owner.session.emulated_nanos();
            assert!(!owner.admit(0, 1).unwrap());
            assert!(owner.session.debug_cpu().matches(&before));
            assert_eq!(owner.session.emulated_nanos(), time);
            owner.finish_run(&mut frame).unwrap();
        }
        for cr0 in [0, 0x8000_0001] {
            let mut owner = owner(&[0x90], 1);
            owner.session_mut().unwrap().debug_cpu_mut().control.cr0 = cr0;
            let mut frame = owner.begin_run(1).unwrap();
            assert!(!owner.admit(0, 1).unwrap());
            owner.finish_run(&mut frame).unwrap();
        }
    }

    #[test]
    fn fallback_sti_shadow_blocks_translation_until_ordinary_instruction_completes() {
        let session = protected_session(&[0xFB, 0x90, 0x90]);
        let mut metadata = metadata(&session, &[], 0);
        metadata.regions.push(RegionMetadata {
            id: 1,
            entry: Address {
                space: 1,
                offset: 1,
            },
            required_steps: 1,
            wait_proof_steps: 0,
            parts: vec![CodePart {
                linear: CODE + 1,
                expected: vec![0x90],
            }],
        });
        let mut owner = StaticSession::new(session, metadata).unwrap();
        let mut frame = owner.begin_run(3).unwrap();
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert!(owner.session.debug_cpu().has_interrupt_shadow());
        assert!(!owner.admit(1, 1).unwrap());
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert!(!owner.session.debug_cpu().has_interrupt_shadow());
        assert_eq!(frame.charged_work, 2);
        owner.finish_run(&mut frame).unwrap();
        assert_eq!(owner.counters().interpreted_steps, 2);
    }

    #[test]
    fn fallback_halt_exits_after_charging_only_the_completed_hlt() {
        let mut owner = owner(&[0xF4], 0);
        let mut frame = owner.begin_run(10).unwrap();
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Exit
        );
        assert_eq!(owner.session.status(), SessionStatus::Halted);
        assert!(owner.session.debug_cpu().is_halted());
        assert_eq!(frame.remaining_budget, 9);
        assert_eq!(frame.charged_work, 1);
        owner.finish_run(&mut frame).unwrap();
        assert_eq!(owner.counters().interpreted_steps, 1);
    }

    #[test]
    fn malformed_admission_count_or_control_flags_cannot_commit() {
        let mut owner = owner(&[0x90], 1);
        let mut frame = owner.begin_run(1).unwrap();
        assert!(matches!(owner.admit(0, 2), Err(Error::Protocol(_))));
        assert!(owner.finish_run(&mut frame).is_err());

        let mut owner = self::owner(&[0x90], 1);
        let mut frame = owner.begin_run(1).unwrap();
        assert!(owner.admit(0, 1).unwrap());
        frame.eflags ^= flags::INTERRUPT;
        frame.charged_work = 1;
        frame.remaining_budget = 0;
        let before = owner.session.debug_cpu().snapshot();
        assert!(matches!(
            owner.finish_run(&mut frame),
            Err(Error::Protocol(_))
        ));
        assert!(owner.session.debug_cpu().matches(&before));
    }

    #[test]
    fn direct_region_chain_admits_against_context_without_stale_gpr_or_ip_checks() {
        let mut owner = owner(&[0x40, 0xEB, 0xFD], 2);
        let mut reference = reference_session(&owner.session);
        let mut frame = owner.begin_run(4).unwrap();
        assert!(owner.admit(0, 2).unwrap());
        // A second direct entry still sees the old Session GPR/IP and old frame.
        assert!(owner.admit(0, 2).unwrap());
        assert_eq!(frame.charged_work, 0);
        assert_eq!(reference.advance(4), 4);
        completed_frame(&owner, &reference, &mut frame, 4);
        owner.finish_run(&mut frame).unwrap();
        assert_eq!(owner.counters().translated_steps, 4);
    }

    #[test]
    fn fallback_refreshes_changed_cs_and_maps_unknown_linear_pc() {
        // The second valid code descriptor has the same flat extent but a new CS.
        let session = protected_session(&[0xEA, 0, 2, 1, 0, 0x10, 0]);
        let mut metadata = metadata(&session, &[], 0);
        metadata.regions.push(RegionMetadata {
            id: 1,
            entry: Address {
                space: 1,
                offset: 7,
            },
            required_steps: 1,
            wait_proof_steps: 0,
            parts: vec![CodePart {
                linear: CODE + 7,
                expected: vec![0x90],
            }],
        });
        let mut owner = StaticSession::new(session, metadata).unwrap();
        let mut frame = owner.begin_run(2).unwrap();
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Continue
        );
        assert_eq!(owner.session.registers().cs, 0x10);
        assert_eq!(owner.session.debug_pc(), 0x10200);
        assert_eq!(
            frame.address,
            Address {
                space: 0,
                offset: 0x10200
            }
        );
        owner.finish_run(&mut frame).unwrap();
        assert_ne!(
            owner.session.registers().segment_cache(SegmentRegister::Cs),
            owner.metadata.context.cs
        );
        let mut frame = owner.begin_run(1).unwrap();
        assert!(!owner.admit(1, 1).unwrap());
        owner.finish_run(&mut frame).unwrap();
    }

    #[test]
    fn actual_pending_irq_cannot_receive_an_event_free_grant() {
        for reason in [abi::REASON_SERVICE, abi::REASON_FALLBACK] {
            let mut owner = owner(&[0x90], 1);
            assert!(
                owner
                    .session_mut()
                    .unwrap()
                    .debug_write_memory(CODE + 16, &[0x91])
            );
            // CLI during setup held IRQ0 pending. Clock actual hardware to a PIT
            // edge, then enable IF without installing a PM IRQ gate. A stale
            // source proof must not bypass ordinary interrupt delivery.
            for _ in 0..60 {
                assert_eq!(
                    owner
                        .session_mut()
                        .unwrap()
                        .debug_advance_device_time(1_000_000)
                        .unwrap(),
                    1_000_000
                );
            }
            owner.session_mut().unwrap().debug_cpu_mut().flags |= flags::INTERRUPT;
            assert!(owner.session.debug_irq_pending());
            let mut frame = owner.begin_run(4).unwrap();
            assert!(!owner.admit(0, 1).unwrap());
            assert_eq!(owner.callback(&mut frame, reason).unwrap(), Control::Exit);
            assert_eq!(owner.session.status(), SessionStatus::Faulted); // Ordinary PM IRQ validation.
            assert!(owner.session.fault().is_some());
            assert_eq!(frame.charged_work, 0); // A fault is never a completed instruction.
            assert_eq!(owner.counters().interpreted_steps, 0);
            owner.finish_run(&mut frame).unwrap();
        }
    }

    #[test]
    fn structural_zero_work_loop_has_bounded_no_progress_diagnostic() {
        let mut owner = owner(&[0x90], 0);
        let mut frame = owner.begin_run(10).unwrap();
        let time = owner.session.emulated_nanos();
        for _ in 1..MAX_EMPTY_SERVICES {
            assert_eq!(
                owner.callback(&mut frame, abi::REASON_SERVICE).unwrap(),
                Control::Continue
            );
        }
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_SERVICE),
            Err(Error::NoProgress)
        );
        assert_eq!(owner.session.emulated_nanos(), time);
        assert_eq!(frame.charged_work, 0);
        assert!(owner.finish_run(&mut frame).is_err());
    }

    #[test]
    fn accounting_mismatch_poisoning_prevents_commit_or_reuse() {
        let mut owner = owner(&[0x40], 1);
        let mut frame = owner.begin_run(2).unwrap();
        let before = owner.session.debug_cpu().snapshot();
        assert!(owner.admit(0, 1).unwrap());
        frame.gpr[0] = 0xBAD;
        // A reserved region did not report completion. No state is committed.
        assert!(matches!(
            owner.finish_run(&mut frame),
            Err(Error::Protocol(_))
        ));
        assert!(owner.session.debug_cpu().matches(&before));
        assert!(owner.begin_run(2).is_err());
        assert!(owner.session_mut().is_err());
    }

    #[test]
    fn constructor_rejects_invalid_metadata_before_owner_publication() {
        let original = protected_session(&[0x40]);
        let valid = metadata(&original, &[0x40], 1);
        let cases: &[fn(&mut Metadata)] = &[
            |m| m.objects[0].space = 0,
            |m| m.objects[0].size = 0,
            |m| m.objects[0].default32 = false,
            |m| m.objects.push(m.objects[0].clone()),
            |m| m.regions[0].id = 1,
            |m| m.regions[0].required_steps = 257,
            |m| m.regions[0].parts.clear(),
            |m| m.regions[0].parts[0].expected.clear(),
            |m| m.regions[0].parts[0].expected.resize(256, 0x40),
            |m| m.regions[0].parts[0].linear += 1,
            |m| m.context.cs.selector ^= 8,
            |m| m.regions.push(m.regions[0].clone()),
        ];
        for edit in cases {
            let session = original.capture_working_state().unwrap().restore().unwrap();
            let mut malformed = valid.clone();
            edit(&mut malformed);
            assert!(matches!(
                StaticSession::new(session, malformed),
                Err(Error::Metadata(_))
            ));
        }
    }

    #[test]
    fn changed_code_at_setup_refuses_only_its_region_and_can_match_again() {
        let mut session = protected_session(&[0x40, 0x90]);
        let mut metadata = metadata(&session, &[0x40], 1);
        metadata.regions.push(RegionMetadata {
            id: 1,
            entry: Address {
                space: 1,
                offset: 1,
            },
            required_steps: 1,
            wait_proof_steps: 0,
            parts: vec![CodePart {
                linear: CODE + 1,
                expected: vec![0x90],
            }],
        });
        assert!(session.debug_write_memory(CODE, &[0x48]));
        let mut owner = StaticSession::new(session, metadata).unwrap();
        owner.session_mut().unwrap().debug_cpu_mut().registers.ip = CODE + 1;
        let mut frame = owner.begin_run(2).unwrap();
        assert!(!owner.admit(0, 1).unwrap());
        assert!(owner.admit(1, 1).unwrap());
        // Reconcile a completed NOP from the independently valid second region.
        frame.address.offset = 2;
        frame.charged_work = 1;
        frame.remaining_budget = 1;
        owner.finish_run(&mut frame).unwrap();
        let session = owner.session_mut().unwrap();
        assert!(session.debug_write_memory(CODE, &[0x40]));
        session.debug_cpu_mut().registers.ip = CODE;
        let mut frame = owner.begin_run(1).unwrap();
        assert!(owner.admit(0, 1).unwrap());
        let mut reference = reference_session(&owner.session);
        assert_eq!(reference.advance(1), 1);
        completed_frame(&owner, &reference, &mut frame, 1);
        owner.finish_run(&mut frame).unwrap();
    }

    #[test]
    fn wait_callback_proves_skips_and_reconciles_logical_work_once() {
        // Default-code32 loop with a same-value store and two RAM operands.
        let body = [
            0xa1, 0, 2, 0, 0, // mov eax,[200h]
            0xa3, 4, 2, 0, 0, // mov [204h],eax
            0xa1, 4, 2, 0, 0, // mov eax,[204h]
            0x3b, 0x05, 8, 2, 0, 0, // cmp eax,[208h]
            0x74, 0xe9, // je head
        ];
        let mut session = protected_session(&body);
        assert_eq!(session.advance(5), 5);
        let mut metadata = metadata(&session, &[], 0);
        metadata.regions[0].wait_proof_steps = 10;
        let mut reference = reference_session(&session);
        let mut owner = StaticSession::new(session, metadata).unwrap();
        let mut frame = owner.begin_run(10_000).unwrap();
        assert_eq!(
            owner.callback(&mut frame, abi::REASON_FALLBACK).unwrap(),
            Control::Exit
        );
        owner.finish_run(&mut frame).unwrap();
        assert_eq!(reference.advance(10_000), 10_000);
        assert_saved_equal(owner.session(), &reference);
        assert_eq!(frame.charged_work, 10_000);
        assert_eq!(frame.remaining_budget, 0);
        let counts = owner.counters();
        assert_eq!(counts.wait_calls, 1);
        assert!(counts.skipped_wait_steps > 9_000);
        assert_eq!(counts.interpreted_steps + counts.skipped_wait_steps, 10_000);
        assert_eq!(counts.translated_steps, 0);
    }

    #[test]
    fn inline_reservations_commit_once_and_match_the_complete_reference() {
        use doslet_x86_wasm::static_program::memory::Effects;
        let mut owner = owner(&[0x40, 0x40], 2);
        let mut reference = reference_session(owner.session());
        let mut frame = owner.begin_run(1).unwrap();
        let grant = owner.grant;
        assert!(grant >= 2);
        assert_eq!(reference.advance(2), 2);
        completed_frame(&owner, &reference, &mut frame, 2);
        owner
            .complete_direct_memory(Effects {
                inline_reserved: 2,
                inline_remaining: grant - 2,
                ..Effects::default()
            })
            .unwrap();
        owner.finish_run(&mut frame).unwrap();
        owner.finish_run(&mut frame).unwrap();
        assert_saved_equal(owner.session(), &reference);
        assert_eq!(owner.counters().translated_steps, 2);
    }

    #[test]
    fn memory_helpers_accumulate_work_without_syncing_registers_or_advancing_time() {
        use doslet_x86_wasm::static_program::memory::Effects;
        let body = [
            0xa3, 0, 0x30, 0, 0, // mov [3000h],eax
            0xa3, 4, 0x30, 0, 0,    // mov [3004h],eax
            0x40, // inc eax
        ];
        let mut owner = owner(&body, 3);
        let mut reference = reference_session(owner.session());
        let value = owner
            .session()
            .debug_cpu()
            .registers
            .get32(doslet_x86::Gpr32::Eax);
        let mut frame = owner.begin_run(3).unwrap();
        let initial_grant = owner.grant;
        assert!(initial_grant >= 3);
        let initial_time = owner.session().emulated_nanos();
        let initial_pc = owner.session().debug_pc();
        let data_base = owner
            .session()
            .registers()
            .segment_cache(SegmentRegister::Ds)
            .base;
        for (index, offset) in [0x3000, 0x3004].into_iter().enumerate() {
            let address = data_base + offset;
            assert!(owner.memory_write_allowed(address, 4).unwrap());
            owner
                .complete_direct_memory(Effects {
                    inline_reserved: 1,
                    inline_remaining: initial_grant - index as u32 - 1,
                    ..Effects::default()
                })
                .unwrap();
            owner.memory_write(address, 4, value).unwrap();
            assert_eq!(owner.pending, index as u32 + 1);
            assert_eq!(owner.session().emulated_nanos(), initial_time);
            assert_eq!(owner.session().debug_pc(), initial_pc);
        }
        owner
            .complete_direct_memory(Effects {
                inline_reserved: 1,
                inline_remaining: initial_grant - 3,
                ..Effects::default()
            })
            .unwrap();
        assert_eq!(reference.advance(3), 3);
        completed_frame(&owner, &reference, &mut frame, 3);
        owner.finish_run(&mut frame).unwrap();
        assert_saved_equal(owner.session(), &reference);
        assert_eq!(owner.counters().memory_write_helpers, 2);
        assert_eq!(owner.counters().translated_steps, 3);
        assert_eq!(owner.counters().interpreted_steps, 0);
        assert_eq!(owner.counters().callbacks, 0);
    }

    #[test]
    fn memory_helper_span_refusals_preserve_all_state_and_reservations() {
        let mut owner = owner(&[0x40], 1);
        let mut frame = owner.begin_run(1).unwrap();
        let before = owner.session().save_state().unwrap();
        let grant = owner.grant;
        for (address, bytes) in [
            (0x30000, 0),
            (0x30000, 3),
            (u32::MAX, 2),
            (0x2000000, 1),
            (0x9ffff, 2),
            (0xaffff, 2),
        ] {
            assert!(!owner.memory_write_allowed(address, bytes).unwrap());
            assert_eq!(owner.pending, 0);
            assert_eq!(owner.grant, grant);
            assert_eq!(owner.session().save_state().unwrap(), before);
        }
        owner.finish_run(&mut frame).unwrap();
    }

    #[test]
    fn direct_source_guards_refresh_only_verified_bytes_and_protect_code_pages() {
        let mut owner = owner(&[0x40, 0x90], 1);
        let mut guards = owner.direct_guards().unwrap();
        assert_eq!(&guards[..4], &1u32.to_le_bytes());
        assert_eq!(owner.direct_code_pages().unwrap()[CODE as usize / 4096], 1);
        assert!(
            owner
                .session_mut()
                .unwrap()
                .debug_write_memory(CODE, &[0x40])
        );
        let mut frame = owner.begin_run(1).unwrap();
        assert!(
            owner
                .refresh_direct_guard(frame.address, &mut guards)
                .unwrap()
        );
        assert!(
            !owner
                .refresh_direct_guard(
                    Address {
                        space: 1,
                        offset: 0xffff
                    },
                    &mut guards
                )
                .unwrap()
        );
        let expected = owner
            .session()
            .debug_code_version(CODE, 1)
            .unwrap()
            .parts()
            .generations[0];
        assert_eq!(&guards[16..24], &expected.to_le_bytes());
        owner.finish_run(&mut frame).unwrap();
        assert!(
            owner
                .session_mut()
                .unwrap()
                .debug_write_memory(CODE, &[0x48])
        );
        let mut frame = owner.begin_run(1).unwrap();
        assert!(
            !owner
                .refresh_direct_guard(frame.address, &mut guards)
                .unwrap()
        );
        assert_eq!(&guards[..4], &0u32.to_le_bytes());
        owner.finish_run(&mut frame).unwrap();
    }

    #[test]
    fn invalid_inline_reservation_is_rejected_before_accounting_or_devices() {
        use doslet_x86_wasm::static_program::memory::Effects;
        let mut owner = owner(&[0x40], 1);
        let frame = owner.begin_run(1).unwrap();
        let before = owner.session().emulated_nanos();
        assert!(
            owner
                .complete_direct_memory(Effects {
                    inline_reserved: 1,
                    inline_remaining: owner.grant,
                    ..Effects::default()
                })
                .is_err()
        );
        assert_eq!(owner.session().emulated_nanos(), before);
        assert_eq!(owner.session().steps(), 6);
        assert_eq!(frame.charged_work, 0);
    }
}
