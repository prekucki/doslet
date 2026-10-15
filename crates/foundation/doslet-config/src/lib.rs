//! Portable machine intent and validation, independent of frontends and devices.
#![no_std]
extern crate alloc;
use alloc::{format, string::String, vec::Vec};
use core::fmt;
pub mod hardware;
pub use hardware::{HardwareConfig, HardwareError, KeyboardMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WritePolicy {
    ReadOnly,
    #[default]
    Temporary,
    Persistent,
    LegacyWriteback,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    NativeDirectory,
    BrowserZip,
    LocalIso,
    HttpsIso,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountSpec {
    pub letter: u8,
    pub source: String,
    pub kind: SourceKind,
    pub policy: WritePolicy,
    pub store: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    NativeDirectory,
    BrowserZip,
    LocalIso,
    HttpsIso,
    NativeStore,
    IndexedDb,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBinding {
    pub id: String,
    pub kind: BindingKind,
    pub location: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostBindingManifest {
    pub bindings: Vec<HostBinding>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSettings {
    pub capture: bool,
    pub pad_index: u8,
    pub pad_id: Option<String>,
    pub deadzone_percent: u8,
    pub axis_x: u8,
    pub axis_y: u8,
    pub button_1: u8,
    pub button_2: u8,
}
impl Default for InputSettings {
    fn default() -> Self {
        Self {
            capture: true,
            pad_index: 0,
            pad_id: None,
            deadzone_percent: 12,
            axis_x: 0,
            axis_y: 1,
            button_1: 0,
            button_2: 1,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputRouting {
    pub audio: bool,
    pub game_midi: String,
    pub host_midi: String,
    pub midi_output: String,
    pub rom_binding: Option<String>,
}
impl Default for OutputRouting {
    fn default() -> Self {
        Self {
            audio: true,
            game_midi: "gm".into(),
            host_midi: "gm".into(),
            midi_output: "rust".into(),
            rom_binding: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameSpec {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
}
impl Default for GameSpec {
    fn default() -> Self {
        Self {
            executable: String::new(),
            arguments: Vec::new(),
            working_directory: "C:\\".into(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineSpec {
    pub name: String,
    pub hardware: HardwareConfig,
    pub mounts: Vec<MountSpec>,
    pub host: HostBindingManifest,
    pub input: InputSettings,
    pub output: OutputRouting,
    pub game: GameSpec,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceClaim {
    pub owner: &'static str,
    pub first: u16,
    pub last: u16,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMachinePlan {
    pub spec: MachineSpec,
    pub hardware: HardwareConfig,
    pub io: Vec<ResourceClaim>,
    pub command_tail: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub field: String,
    pub message: String,
}
impl ConfigError {
    pub fn new(field: &str, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

pub struct Component {
    pub id: &'static str,
    pub name: &'static str,
    pub support: &'static str,
}
pub const CATALOG: &[Component] = &[
    Component {
        id: "cpu.i386",
        name: "i386 interpreter",
        support: "Selected instruction subset; functional 33 MHz timing",
    },
    Component {
        id: "fpu.x87",
        name: "x87",
        support: "Selected arithmetic and control instructions",
    },
    Component {
        id: "video.vga-vbe",
        name: "VGA / VBE",
        support: "512 KiB VRAM; existing VBE modes",
    },
    Component {
        id: "sound.sb16",
        name: "Sound Blaster 16",
        support: "DSP / PCM; fixed I/O 220h",
    },
    Component {
        id: "midi.mpu401",
        name: "MPU-401 UART",
        support: "Fixed I/O 330h–331h",
    },
    Component {
        id: "input.at",
        name: "AT keyboard",
        support: "i8042 scan codes and IRQ 1",
    },
    Component {
        id: "input.gameport",
        name: "ISA gameport",
        support: "Two axes and two buttons at 201h",
    },
];
impl MachineSpec {
    pub fn resolve(&self) -> Result<ResolvedMachinePlan, ConfigError> {
        self.hardware.validate().map_err(|e| {
            ConfigError::new(
                match e {
                    HardwareError::MemorySize => "machine.memory_mib",
                    HardwareError::SoundIrq => "sound.irq",
                    HardwareError::SoundDma8 => "sound.dma8",
                    HardwareError::SoundDma16 => "sound.dma16",
                },
                format!("unsupported assignment: {e:?}"),
            )
        })?;
        if self.input.deadzone_percent > 95 {
            return Err(ConfigError::new(
                "input.deadzone_percent",
                "must be between 0 and 95",
            ));
        }
        if self.input.axis_x == self.input.axis_y {
            return Err(ConfigError::new("input.axis_y", "must differ from axis_x"));
        }
        if self.input.axis_x >= 8 || self.input.axis_y >= 8 {
            return Err(ConfigError::new(
                "input.axes",
                "supported controller axes are 0 through 7",
            ));
        }
        if self.input.button_1 >= 32 || self.input.button_2 >= 32 {
            return Err(ConfigError::new(
                "input.buttons",
                "supported controller buttons are 0 through 31",
            ));
        }
        if self.input.button_1 == self.input.button_2 {
            return Err(ConfigError::new(
                "input.button_2",
                "must differ from button_1",
            ));
        }
        for mode in [&self.output.game_midi, &self.output.host_midi] {
            if !matches!(mode.as_str(), "gm" | "mt32") {
                return Err(ConfigError::new(
                    "output",
                    "MIDI dialect must be gm or mt32",
                ));
            }
        }
        if self.output.midi_output.is_empty() {
            return Err(ConfigError::new("output.midi_output", "must not be empty"));
        }
        if self.mounts.len() > 2 {
            return Err(ConfigError::new("drives", "supports C: and one CD-ROM"));
        }
        for (index, mount) in self.mounts.iter().enumerate() {
            if let Some(binding) = self
                .host
                .bindings
                .iter()
                .find(|binding| binding.id == mount.source)
            {
                let valid = matches!(
                    (mount.kind, binding.kind),
                    (SourceKind::NativeDirectory, BindingKind::NativeDirectory)
                        | (SourceKind::BrowserZip, BindingKind::BrowserZip)
                        | (SourceKind::LocalIso, BindingKind::LocalIso)
                        | (SourceKind::HttpsIso, BindingKind::HttpsIso)
                );
                if !valid {
                    return Err(ConfigError::new(
                        "drives.source",
                        "source binding has an incompatible kind",
                    ));
                }
            }
            if let Some(binding) = mount
                .store
                .as_ref()
                .and_then(|id| self.host.bindings.iter().find(|binding| binding.id == *id))
                && !matches!(
                    binding.kind,
                    BindingKind::NativeStore | BindingKind::IndexedDb
                )
            {
                return Err(ConfigError::new(
                    "drives.store",
                    "save binding must name a native store or IndexedDB namespace",
                ));
            }
            if self.mounts[..index]
                .iter()
                .any(|m| m.letter == mount.letter)
            {
                return Err(ConfigError::new("drives.letter", "duplicate drive letter"));
            }
            if mount.source.is_empty() {
                return Err(ConfigError::new(
                    "drives.source",
                    "source binding is required",
                ));
            }
            match mount.kind {
                SourceKind::NativeDirectory | SourceKind::BrowserZip => {
                    if mount.letter != b'C' {
                        return Err(ConfigError::new(
                            "drives.letter",
                            "directory or ZIP tree must use C:",
                        ));
                    }
                }
                SourceKind::LocalIso | SourceKind::HttpsIso => {
                    if !(b'D'..=b'Z').contains(&mount.letter)
                        || mount.policy != WritePolicy::ReadOnly
                    {
                        return Err(ConfigError::new(
                            "drives",
                            "CD-ROM requires D:–Z: and read-only policy",
                        ));
                    }
                }
            }
            if mount.policy == WritePolicy::Persistent
                && mount.store.as_deref().is_none_or(str::is_empty)
            {
                return Err(ConfigError::new(
                    "drives.store",
                    "persistent changes require an explicit save store",
                ));
            }
            if mount.kind == SourceKind::HttpsIso
                && !mount.source.starts_with("https://")
                && !self.host.bindings.iter().any(|b| {
                    b.id == mount.source
                        && b.kind == BindingKind::HttpsIso
                        && b.location.starts_with("https://")
                })
            {
                return Err(ConfigError::new(
                    "drives.source",
                    "remote ISO requires HTTPS",
                ));
            }
        }
        for (index, binding) in self.host.bindings.iter().enumerate() {
            if binding.id.is_empty()
                || binding.location.is_empty()
                || self.host.bindings[..index]
                    .iter()
                    .any(|b| b.id == binding.id)
            {
                return Err(ConfigError::new(
                    "bindings",
                    "binding names must be unique and locations nonempty",
                ));
            }
        }
        let directory = &self.game.working_directory;
        if !directory.is_ascii()
            || directory.bytes().any(|b| b < 32 || b == 127)
            || !(directory.starts_with('\\') || directory.to_ascii_uppercase().starts_with("C:\\"))
        {
            return Err(ConfigError::new(
                "game.working_directory",
                "must be an absolute ASCII DOS path on C:",
            ));
        }
        let mut tail = Vec::new();
        for arg in &self.game.arguments {
            if !arg.is_ascii() || arg.bytes().any(|b| b < 32 || b == 127 || b == b'"') {
                return Err(ConfigError::new(
                    "game.arguments",
                    "must be printable ASCII without double quotes",
                ));
            }
            tail.push(b' ');
            let quote = arg.is_empty() || arg.contains(' ');
            if quote {
                tail.push(b'"');
            }
            tail.extend_from_slice(arg.as_bytes());
            if quote {
                tail.push(b'"');
            }
            if tail.len() > 126 {
                return Err(ConfigError::new(
                    "game.arguments",
                    "exceeds the 126-byte DOS command tail limit",
                ));
            }
        }
        let mut io = Vec::new();
        for (owner, first, last) in [
            ("DMA", 0, 15),
            ("PIC", 0x20, 0x21),
            ("PIT", 0x40, 0x43),
            ("keyboard", 0x60, 0x64),
            ("RTC", 0x70, 0x71),
            ("DMA pages", 0x80, 0x8f),
            ("A20", 0x92, 0x92),
            ("PIC slave", 0xa0, 0xa1),
            ("DMA16", 0xc0, 0xdf),
            ("VGA", 0x3b0, 0x3df),
        ] {
            io.push(ResourceClaim { owner, first, last });
        }
        if self.hardware.sb16 {
            io.push(ResourceClaim {
                owner: "SB16",
                first: 0x220,
                last: 0x22f,
            });
        }
        if self.hardware.mpu401 {
            io.push(ResourceClaim {
                owner: "MPU-401",
                first: 0x330,
                last: 0x331,
            });
        }
        if self.hardware.gameport {
            io.push(ResourceClaim {
                owner: "gameport",
                first: 0x201,
                last: 0x201,
            });
        }
        for (index, claim) in io.iter().enumerate() {
            if let Some(other) = io[..index]
                .iter()
                .find(|other| other.first <= claim.last && claim.first <= other.last)
            {
                return Err(ConfigError::new(
                    "resources.io",
                    format!(
                        "{} conflicts with {} at {:04X}h–{:04X}h",
                        claim.owner, other.owner, claim.first, claim.last
                    ),
                ));
            }
        }
        Ok(ResolvedMachinePlan {
            spec: self.clone(),
            hardware: self.hardware,
            io,
            command_tail: tail,
        })
    }
    pub fn binding<'a>(&'a self, id: &'a str) -> &'a str {
        self.host
            .bindings
            .iter()
            .find(|b| b.id == id)
            .map_or(id, |b| b.location.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_resource_validation() {
        let mut spec = MachineSpec::default();
        assert_eq!(spec.resolve().unwrap().hardware.memory_mib, 16);
        spec.hardware.sb_dma8 = 2;
        assert_eq!(spec.resolve().unwrap_err().field, "sound.dma8");
        spec.hardware.sb16 = false;
        assert!(spec.resolve().is_ok());
    }
    #[test]
    fn persistent_requires_store_and_cd_stays_readonly() {
        let mut spec = MachineSpec::default();
        spec.mounts.push(MountSpec {
            letter: b'C',
            source: String::from("game"),
            kind: SourceKind::BrowserZip,
            policy: WritePolicy::Persistent,
            store: None,
        });
        assert_eq!(spec.resolve().unwrap_err().field, "drives.store");
        spec.mounts[0].store = Some(String::from("saves"));
        assert!(spec.resolve().is_ok());
        spec.mounts[0].kind = SourceKind::LocalIso;
        assert!(spec.resolve().is_err());
    }
}
