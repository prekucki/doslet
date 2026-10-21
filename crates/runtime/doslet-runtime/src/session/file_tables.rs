//! DOS's own file tables in guest memory, kept equal to the session's handles
//! after every open, duplicate, close, read, write and seek. The layout is in
//! `doslet_dos::tables`.
use super::*;
use doslet_dos::loader::PSP_SEGMENT;
use doslet_dos::tables::{self, SftEntry};

/// Every file reports one deterministic noon timestamp, through 57h and the SFT.
pub(super) const FILE_TIME: u16 = 0x6000;
pub(super) const FILE_DATE: u16 = (46 << 9) | (10 << 5) | 14;

impl Session {
    /// IOCTL 4400h's device information word; the SFT entry holds the same.
    pub(super) fn device_info(&self, handle: u16) -> Option<u16> {
        if handle <= 2 {
            return Some(tables::CON_DEVICE_INFO);
        }
        let file = self.handles.get(handle).ok()?;
        Some(self.file_device_info(file))
    }
    fn file_device_info(&self, file: &OpenFile) -> u16 {
        match (file.mount.0, &self.cdrom) {
            (1, Some(cd)) => u16::from(cd.drive - b'A'),
            _ => 2,
        }
    }
    /// List of Lists fields that launch setup may still change.
    pub(super) fn list_of_lists(&mut self) -> Result<(u16, u16), BusError> {
        let lastdrive = self
            .cdrom
            .as_ref()
            .map_or(5, |cd| (cd.drive - b'A' + 1).max(5));
        tables::set_system(&mut self.machine.bus, self.memory.first_mcb(), lastdrive)?;
        Ok((tables::SEGMENT, tables::LIST_OF_LISTS))
    }
    /// Publish a handle's JFT slot and its description's SFT entry after it
    /// was opened, duplicated or closed. `previous` is the description it
    /// referred to before.
    pub(super) fn sync_handle(
        &mut self,
        handle: u16,
        previous: Option<usize>,
    ) -> Result<(), BusError> {
        let index = self.handles.description_index(handle);
        tables::set_handle(
            &mut self.machine.bus,
            PSP_SEGMENT,
            handle,
            index.map(|index| tables::FIRST_FILE + index),
        )?;
        if let Some(previous) = previous.filter(|previous| Some(*previous) != index) {
            self.sync_description(previous)?;
        }
        if let Some(index) = index {
            self.sync_description(index)?;
        }
        Ok(())
    }
    /// Refresh the descriptions over a backend handle after its position or
    /// length changed.
    pub(super) fn sync_backend(&mut self, backend: u16) -> Result<(), BusError> {
        let mount = MountId(u8::from(CdromImage::owns_handle(backend)));
        for index in 0..doslet_vfs::HANDLE_COUNT {
            if self
                .handles
                .description(index)
                .is_some_and(|file| file.mount == mount && file.node == NodeId(u64::from(backend)))
            {
                self.sync_description(index)?;
            }
        }
        Ok(())
    }
    fn sync_description(&mut self, index: usize) -> Result<(), BusError> {
        let entry = self.sft_entry(index).unwrap_or_default();
        tables::write_entry(&mut self.machine.bus, tables::FIRST_FILE + index, &entry)
    }
    fn sft_entry(&self, index: usize) -> Option<SftEntry> {
        let file = self.handles.description(index)?;
        let backend = file.node.0 as u16;
        let mut entry = SftEntry {
            references: self.handles.references(index),
            mode: u16::from(file.mode),
            attributes: 0x20,
            device_info: self.file_device_info(file),
            time: FILE_TIME,
            date: FILE_DATE,
            owner: PSP_SEGMENT,
            ..SftEntry::default()
        };
        if CdromImage::owns_handle(backend) {
            let (position, length) = self.cdrom.as_ref()?.handle_info(backend).ok()?;
            entry.attributes = 0x21;
            entry.size = u32::try_from(length).unwrap_or(u32::MAX);
            entry.position = position;
        } else {
            let info = self.drive.handle_info(backend).ok()?;
            // Synthetic disk locations: one cluster and directory slot per
            // file, so readers deriving inode numbers tell files apart.
            if info.length != 0 {
                entry.cluster = (info.file % 0xFFED) as u16 + 2;
            }
            entry.directory_sector = info.file as u32 / 16 + 1;
            entry.directory_entry = (info.file % 16) as u8;
            entry.name = info.short_name;
            entry.size = info.length;
            entry.position = info.position;
        }
        Some(entry)
    }
}
