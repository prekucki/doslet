//! One-shot physical RAM edits, checked at bounded execution boundaries.
//! Host paths, file formats and executable identity checks belong to the loader.
use super::{RuntimeError, Session, SessionStatus};
use alloc::{string::String, vec::Vec};

pub const MAX_PATCHES: usize = 32;
pub const MAX_PATCH_BYTES: usize = 64 * 1024;
pub const MAX_PATCH_SPANS: usize = 128;

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "save-state", derive(prost::Message))]
#[cfg_attr(not(feature = "save-state"), derive(Debug))]
pub struct PatchWrite {
    #[cfg_attr(feature = "save-state", prost(uint32, tag = "1"))]
    pub address: u32,
    #[cfg_attr(feature = "save-state", prost(bytes = "vec", tag = "2"))]
    pub expected: Vec<u8>,
    #[cfg_attr(feature = "save-state", prost(bytes = "vec", tag = "3"))]
    pub replacement: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "save-state", derive(prost::Message))]
#[cfg_attr(not(feature = "save-state"), derive(Debug))]
pub struct PatchGuard {
    #[cfg_attr(feature = "save-state", prost(uint32, tag = "1"))]
    pub address: u32,
    #[cfg_attr(feature = "save-state", prost(bytes = "vec", tag = "2"))]
    pub expected: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "save-state", derive(prost::Message))]
#[cfg_attr(not(feature = "save-state"), derive(Debug))]
pub struct MemoryPatch {
    #[cfg_attr(feature = "save-state", prost(string, tag = "1"))]
    pub name: String,
    /// Absolute guest step deadline since launch, including earlier patches.
    #[cfg_attr(feature = "save-state", prost(uint64, tag = "2"))]
    pub timeout_steps: u64,
    #[cfg_attr(feature = "save-state", prost(message, repeated, tag = "3"))]
    pub writes: Vec<PatchWrite>,
    #[cfg_attr(feature = "save-state", prost(message, repeated, tag = "4"))]
    pub guards: Vec<PatchGuard>,
}

impl Session {
    /// Install a bounded plan before execution. No RAM is changed on failure.
    pub fn configure_patches(&mut self, patches: Vec<MemoryPatch>) -> Result<(), &'static str> {
        if self.status != SessionStatus::Ready || self.steps != 0 {
            return Err("patch setup requires a fresh, ready session");
        }
        self.validate_patches(&patches)?;
        self.patches = patches;
        self.patches_applied = 0;
        Ok(())
    }

    pub fn memory_patches(&self) -> &[MemoryPatch] {
        &self.patches
    }

    /// Replace the pending patches of the plan, for example with addresses
    /// moved to where a DOS extender loaded the program in this run. Applied
    /// patches must stay unchanged; no RAM is changed here.
    pub fn relocate_patches(&mut self, patches: Vec<MemoryPatch>) -> Result<(), &'static str> {
        if patches.len() != self.patches.len()
            || patches[..self.patches_applied] != self.patches[..self.patches_applied]
        {
            return Err("relocated patches must keep the plan and its applied patches");
        }
        self.validate_patches(&patches)?;
        self.patches = patches;
        Ok(())
    }

    /// Patches are applied in configuration order; this prefix is complete.
    pub fn applied_patch_count(&self) -> usize {
        self.patches_applied
    }

    pub(super) fn validate_patches(&self, patches: &[MemoryPatch]) -> Result<(), &'static str> {
        if patches.len() > MAX_PATCHES {
            return Err("too many memory patches (maximum 32)");
        }
        for patch in patches {
            if patch.name.is_empty() || patch.name.len() > 128 || patch.timeout_steps == 0 {
                return Err("patch needs a name (1..128 bytes) and a positive step deadline");
            }
            if patch.writes.is_empty() || patch.writes.len() + patch.guards.len() > MAX_PATCH_SPANS
            {
                return Err("patch needs writes and at most 128 write/guard spans");
            }
            let mut size = 0usize;
            for (address, expected) in patch
                .writes
                .iter()
                .map(|w| (w.address, &w.expected))
                .chain(patch.guards.iter().map(|g| (g.address, &g.expected)))
            {
                size = size
                    .checked_add(expected.len())
                    .ok_or("patch size overflow")?;
                if expected.is_empty() || size > MAX_PATCH_BYTES {
                    return Err("patch spans must be nonempty and total at most 64 KiB");
                }
                let end = u64::from(address) + expected.len() as u64;
                if end > self.machine.bus.ram_size() as u64
                    || (address < doslet_vga::APERTURE_START + doslet_vga::APERTURE_LEN as u32
                        && end > u64::from(doslet_vga::APERTURE_START))
                {
                    return Err("patch span must fit ordinary physical RAM (no MMIO)");
                }
            }
            if patch
                .writes
                .iter()
                .any(|w| w.expected.len() != w.replacement.len())
            {
                return Err("patch expected and replacement lengths must match");
            }
        }
        // Overlapping writes across files are ambiguous; reject the entire plan.
        for (index, write) in patches.iter().flat_map(|p| &p.writes).enumerate() {
            for previous in patches.iter().flat_map(|p| &p.writes).take(index) {
                if u64::from(write.address)
                    < u64::from(previous.address) + previous.expected.len() as u64
                    && u64::from(previous.address)
                        < u64::from(write.address) + write.expected.len() as u64
                {
                    return Err("memory patch writes overlap");
                }
            }
        }
        Ok(())
    }

    pub(super) fn check_patches(&mut self) {
        while let Some(patch) = self.patches.get(self.patches_applied) {
            if self.steps >= patch.timeout_steps {
                self.fault = Some(RuntimeError::PatchTimeout(self.patches_applied as u8 + 1));
                self.status = SessionStatus::Faulted;
                return;
            }
            let pc = if self.a20_enabled() {
                self.debug_pc()
            } else {
                self.debug_pc() & !(1 << 20)
            };
            let matches = patch.guards.iter().all(|g| {
                self.ram_view(g.address as usize..g.address as usize + g.expected.len())
                    == Some(&g.expected[..])
            }) && patch.writes.iter().all(|w| {
                // Starting at the first replacement byte is safe. Defer if the
                // current PC is inside the sequence being replaced.
                !(u64::from(w.address) + 1..u64::from(w.address) + w.expected.len() as u64)
                    .contains(&u64::from(pc))
                    && self.ram_view(w.address as usize..w.address as usize + w.expected.len())
                        == Some(&w.expected[..])
            });
            if !matches {
                return;
            }
            // Revalidate all mappings above before any write. The machine has one
            // owner and cannot execute between these edits. Mutation invalidates
            // decode/JIT page generations just like debugger RAM writes.
            for write in &patch.writes {
                self.machine
                    .bus
                    .debug_ram_mut(write.address, write.replacement.len() as u32)
                    .expect("validated patch RAM")
                    .copy_from_slice(&write.replacement);
            }
            self.patches_applied += 1;
            let idle = self.idle.enabled;
            self.set_idle_loops(false);
            self.set_idle_loops(idle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn plan(address: u32, expected: &[u8], replacement: &[u8]) -> MemoryPatch {
        MemoryPatch {
            name: "test".into(),
            timeout_steps: 20_000,
            writes: vec![PatchWrite {
                address,
                expected: expected.to_vec(),
                replacement: replacement.to_vec(),
            }],
            guards: Vec::new(),
        }
    }

    #[test]
    fn delayed_match_is_atomic_and_applied_once() {
        // The guest supplies the guard on its first instruction.
        let mut s = Session::new(&[0xc6, 0x06, 0x00, 0x02, 0x90, 0xeb, 0xfe]).unwrap();
        let mut patch = plan(0x10300, &[0], &[7]);
        patch.guards.push(PatchGuard {
            address: 0x10200,
            expected: vec![0x90],
        });
        patch.writes.push(PatchWrite {
            address: 0x10301,
            expected: vec![0],
            replacement: vec![8],
        });
        s.configure_patches(vec![patch]).unwrap();
        s.check_patches();
        assert_eq!(s.ram_view(0x10300..0x10302).unwrap(), &[0, 0]);
        s.start();
        s.advance(1);
        assert_eq!(s.applied_patch_count(), 1);
        assert_eq!(s.ram_view(0x10300..0x10302).unwrap(), &[7, 8]);
        assert!(s.debug_write_memory(0x10300, &[99]));
        s.advance(5);
        assert_eq!(s.ram_view(0x10300..0x10302).unwrap(), &[99, 8]);
    }

    #[test]
    fn relocation_replaces_only_pending_patches() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        s.configure_patches(vec![plan(0x10300, &[0], &[7]), plan(0x10400, &[1], &[8])])
            .unwrap();
        s.start();
        s.advance(1);
        assert_eq!(s.applied_patch_count(), 1);
        // The pending write waits at an address the guest never fills.
        let moved = vec![plan(0x10300, &[0], &[7]), plan(0x10500, &[0], &[9])];
        assert!(
            s.relocate_patches(vec![plan(0x10310, &[0], &[7]), plan(0x10500, &[0], &[9])])
                .is_err()
        );
        assert!(s.relocate_patches(moved[..1].to_vec()).is_err());
        s.relocate_patches(moved).unwrap();
        s.advance(1);
        assert_eq!(s.applied_patch_count(), 2);
        assert_eq!(s.ram_view(0x10500..0x10501).unwrap(), &[9]);
        assert_eq!(s.ram_view(0x10400..0x10401).unwrap(), &[0]);
    }

    #[test]
    fn later_patch_waits_for_the_configured_order() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        s.configure_patches(vec![plan(0x10300, &[1], &[7]), plan(0x10301, &[0], &[8])])
            .unwrap();
        s.start();
        s.advance(1);
        assert_eq!(s.applied_patch_count(), 0);
        assert_eq!(s.ram_view(0x10300..0x10302).unwrap(), &[0, 0]);
        assert!(s.debug_write_memory(0x10300, &[1]));
        s.advance(1);
        assert_eq!(s.applied_patch_count(), 2);
        assert_eq!(s.ram_view(0x10300..0x10302).unwrap(), &[7, 8]);
    }

    #[test]
    fn mismatch_faults_at_deadline_without_partial_writes() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        let mut patch = plan(0x10300, &[0], &[7]);
        patch.writes.push(PatchWrite {
            address: 0x10301,
            expected: vec![1],
            replacement: vec![8],
        });
        patch.timeout_steps = 5;
        s.configure_patches(vec![patch]).unwrap();
        s.start();
        s.advance(10_000);
        assert_eq!(s.steps(), 5);
        assert_eq!(s.fault(), Some(RuntimeError::PatchTimeout(1)));
        assert_eq!(s.ram_view(0x10300..0x10302).unwrap(), &[0, 0]);
    }

    #[test]
    fn invalid_spans_and_overlapping_files_are_rejected_before_execution() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        for patch in [
            plan(0xaffff, &[0, 0], &[1, 1]),
            plan(u32::MAX, &[0, 0], &[1, 1]),
            plan(0x10300, &[0], &[1, 2]),
        ] {
            assert!(s.configure_patches(vec![patch]).is_err());
        }
        assert!(
            s.configure_patches(vec![
                plan(0x10300, &[0, 0], &[1, 1]),
                plan(0x10301, &[0], &[2])
            ])
            .is_err()
        );
        assert!(s.memory_patches().is_empty());
    }

    #[test]
    fn a20_alias_is_never_patched_and_plan_can_wait_for_mapping() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        s.machine.bus.set_a20(false);
        s.configure_patches(vec![plan(0x110300, &[0], &[9])])
            .unwrap();
        s.start();
        s.advance(1);
        assert_eq!(s.applied_patch_count(), 0);
        assert_eq!(s.ram_view(0x10300..0x10301).unwrap(), &[0]);
        s.machine.bus.set_a20(true);
        s.advance(1);
        assert_eq!(s.applied_patch_count(), 1);
        assert_eq!(s.ram_view(0x110300..0x110301).unwrap(), &[9]);
    }

    #[test]
    fn code_patch_invalidates_cached_instructions_and_keeps_idle_policy() {
        // INC AX; JMP back. Decode it before applying DEC AX at that address.
        let mut s = Session::new(&[0x40, 0xeb, 0xfd]).unwrap();
        let mut patch = plan(0x10100, &[0x40], &[0x48]);
        patch.guards.push(PatchGuard {
            address: 0x10300,
            expected: vec![1],
        });
        s.configure_patches(vec![patch]).unwrap();
        s.set_idle_loops(true);
        s.start();
        s.advance(2);
        assert_eq!(s.registers().get(doslet_x86::Gpr16::Ax), 1);
        s.machine.bus.debug_ram_mut(0x10300, 1).unwrap()[0] = 1;
        s.advance(1);
        assert_eq!(s.registers().get(doslet_x86::Gpr16::Ax), 0);
        assert!(s.idle.enabled);
    }

    #[cfg(feature = "save-state")]
    #[test]
    fn snapshots_retain_pending_plan_deadline_and_applied_state() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        s.configure_patches(vec![plan(0x10300, &[1], &[7])])
            .unwrap();
        s.start();
        s.advance(10);
        let mut restored = Session::load_state(&s.save_state().unwrap()).unwrap();
        assert_eq!(restored.memory_patches(), s.memory_patches());
        assert_eq!(restored.steps(), 10);
        assert!(restored.debug_write_memory(0x10300, &[1]));
        restored.advance(1);
        assert_eq!(restored.applied_patch_count(), 1);
        assert!(restored.debug_write_memory(0x10300, &[42]));
        let mut again = Session::load_state(&restored.save_state().unwrap()).unwrap();
        again.advance(1);
        assert_eq!(again.applied_patch_count(), 1);
        assert_eq!(again.ram_view(0x10300..0x10301).unwrap(), &[42]);
    }
}
