//! Allocation telemetry from known guest allocators, never inferred from RAM contents.
use super::Session;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryUsage {
    pub installed_bytes: u64,
    pub protected_mode: bool,
    pub dos_allocated: u64,
    pub dos_free: u64,
    pub dos_largest_free: u64,
    pub dos_blocks: u32,
    pub dpmi_available: bool,
    pub dpmi_entered: bool,
    pub dpmi_capacity: u64,
    pub dpmi_requested: u64,
    pub dpmi_committed: u64,
    pub dpmi_peak: u64,
    pub dpmi_blocks: u32,
}

impl Session {
    /// Sample at presentation boundaries. Guest-managed extended memory remains unknown.
    pub fn memory_usage(&self) -> MemoryUsage {
        let dos = self.memory.stats();
        let dpmi = self.dpmi_stats();
        MemoryUsage {
            installed_bytes: self.machine.bus.ram_size() as u64,
            protected_mode: self.machine.cpu.protected_mode(),
            dos_allocated: dos.allocated_bytes,
            dos_free: dos.free_bytes,
            dos_largest_free: dos.largest_free_bytes,
            dos_blocks: dos.allocated_blocks,
            dpmi_available: dpmi.is_some(),
            dpmi_entered: dpmi.is_some_and(|d| d.entered),
            dpmi_capacity: self.dpmi_memory_capacity().unwrap_or(0),
            dpmi_requested: dpmi.map_or(0, |d| d.memory.requested_bytes),
            dpmi_committed: dpmi.map_or(0, |d| d.memory.committed_bytes),
            dpmi_peak: dpmi.map_or(0, |d| d.memory.peak_committed_bytes),
            dpmi_blocks: self.dpmi_blocks().count() as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::dpmi::Mode;

    #[test]
    fn installed_host_is_distinct_from_an_entered_client() {
        let mut session = Session::demo();
        let dos = session.memory_usage();
        assert!(!dos.dpmi_available);
        assert!(dos.dos_allocated > 0);
        assert!(dos.installed_bytes >= dos.dos_allocated + dos.dos_free);
        session.enable_dpmi(Mode::Direct).unwrap();
        let host = session.memory_usage();
        assert!(host.dpmi_available);
        assert!(!host.dpmi_entered);
        assert_eq!(host.dpmi_committed, 0);
        assert_eq!(host.dpmi_capacity, host.installed_bytes - 0x120000);
    }
}
