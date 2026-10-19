//! Portable machine intent and validation, independent of frontends and devices.
#![no_std]
extern crate alloc;
use alloc::{format, string::String, vec::Vec};
use core::fmt;
pub mod hardware;
pub use hardware::{AudioHardware, HardwareConfig, HardwareError, KeyboardMode};

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Auto,
    Capture,
    NoCapture,
}
impl MouseMode {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Capture, Self::NoCapture];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Capture => "capture",
            Self::NoCapture => "no-capture",
        }
    }
    pub const fn relative(self, guest_relative: bool) -> bool {
        match self {
            Self::Auto => guest_relative,
            Self::Capture => true,
            Self::NoCapture => false,
        }
    }
}
impl core::fmt::Display for MouseMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Auto => "Auto",
            Self::Capture => "Capture",
            Self::NoCapture => "No capture",
        })
    }
}
impl core::str::FromStr for MouseMode {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "capture" => Ok(Self::Capture),
            "no-capture" => Ok(Self::NoCapture),
            _ => Err("input.mouse_mode must be auto, capture, or no-capture"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSettings {
    pub mouse_mode: MouseMode,
    pub hide_cursor: bool,
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
            mouse_mode: MouseMode::Auto,
            hide_cursor: false,
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
    pub video_filter: VideoFilter,
    pub game_midi: String,
    pub host_midi: String,
    pub midi_output: String,
    pub rom_binding: Option<String>,
}
impl Default for OutputRouting {
    fn default() -> Self {
        Self {
            audio: true,
            video_filter: VideoFilter::Nearest,
            game_midi: "gm".into(),
            host_midi: "gm".into(),
            midi_output: "rust".into(),
            rom_binding: None,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoFilter {
    #[default]
    Nearest,
    SharpBilinear,
    Hq2x,
}
impl VideoFilter {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::SharpBilinear => "sharp-bilinear",
            Self::Hq2x => "hq2x",
        }
    }
}
impl core::str::FromStr for VideoFilter {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "nearest" => Ok(Self::Nearest),
            "sharp-bilinear" => Ok(Self::SharpBilinear),
            "hq2x" => Ok(Self::Hq2x),
            _ => Err("output.video_filter must be nearest, sharp-bilinear, or hq2x"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameSpec {
    pub executable: String,
    /// Native patch manifests, resolved relative to the launch profile.
    pub patches: Vec<String>,
    pub arguments: Vec<String>,
    pub working_directory: String,
    /// ASCII characters queued in the BIOS keyboard buffer at the first wait.
    pub startup_keys: String,
}
impl Default for GameSpec {
    fn default() -> Self {
        Self {
            executable: String::new(),
            patches: Vec::new(),
            arguments: Vec::new(),
            working_directory: "C:\\".into(),
            startup_keys: String::new(),
        }
    }
}
/// Translate a startup character into the BIOS scan/ASCII pair for a US PC/AT
/// keyboard. This accepts printable ASCII and common DOS control keys.
pub fn bios_key_for_ascii(byte: u8) -> Option<(u8, u8)> {
    match byte {
        b'\n' | b'\r' => return Some((0x1c, b'\r')),
        b'\t' => return Some((0x0f, b'\t')),
        8 => return Some((0x0e, 8)),
        27 => return Some((0x01, 27)),
        b' '..=b'~' => {}
        _ => return None,
    }
    const SHIFTED: &[u8] = b"!@#$%^&*()_+{}|:\"~<>?";
    const BASE: &[u8] = b"1234567890-=[]\\;'`,./";
    let base = SHIFTED
        .iter()
        .position(|candidate| *candidate == byte)
        .map_or(byte.to_ascii_lowercase(), |index| BASE[index]);
    for (row, first) in [
        (&b"qwertyuiop[]"[..], 0x10),
        (&b"asdfghjkl;'"[..], 0x1e),
        (&b"zxcvbnm,./"[..], 0x2c),
    ] {
        if let Some(index) = row.iter().position(|candidate| *candidate == base) {
            return Some((first + index as u8, byte));
        }
    }
    let scan = match base {
        b'1'..=b'9' => base - b'1' + 2,
        b'0' => 0x0b,
        b'-' => 0x0c,
        b'=' => 0x0d,
        b'`' => 0x29,
        b'\\' => 0x2b,
        b' ' => 0x39,
        _ => return None,
    };
    Some((scan, byte))
}
#[cfg(test)]
mod startup_key_tests {
    use super::bios_key_for_ascii;

    #[test]
    fn printable_ascii_has_bios_scan_codes() {
        for ascii in b' '..=b'~' {
            let (scan, actual) = bios_key_for_ascii(ascii).unwrap();
            assert_ne!(scan, 0);
            assert_eq!(actual, ascii);
        }
        assert_eq!(bios_key_for_ascii(b'1'), Some((0x02, b'1')));
        assert_eq!(bios_key_for_ascii(b'5'), Some((0x06, b'5')));
        assert_eq!(bios_key_for_ascii(b'A'), Some((0x1e, b'A')));
        assert_eq!(bios_key_for_ascii(b'?'), Some((0x35, b'?')));
        assert_eq!(bios_key_for_ascii(b'\n'), Some((0x1c, b'\r')));
        assert_eq!(bios_key_for_ascii(0), None);
    }
}
/// Descriptive metadata for launchers and generated pages. Emulation ignores it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameInfo {
    /// Image path, resolved relative to the launch profile.
    pub cover: Option<String>,
    pub released: Option<u16>,
    pub publishers: Vec<String>,
    pub developers: Vec<String>,
    pub genres: Vec<String>,
    pub gameplay: Vec<String>,
    /// Plain-text summary; blank lines separate paragraphs.
    pub description: Option<String>,
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
    pub info: GameInfo,
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
        support: "Selected instruction subset; configurable nominal CPU clock",
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
        support: "DSP / PCM and AdLib / OPL3 FM; I/O 220h and 388h",
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
                    HardwareError::CpuClock => "machine.cpu_mhz",
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
        if self.game.patches.len() > 32 || self.game.patches.iter().any(String::is_empty) {
            return Err(ConfigError::new(
                "game.patches",
                "must contain at most 32 nonempty paths",
            ));
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
                owner: "OPL3",
                first: 0x388,
                last: 0x38b,
            });
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
