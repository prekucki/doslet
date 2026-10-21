//! Global host settings (`~/.config/doslet/settings.toml`) and hardware presets.
//! Holds host-wide defaults for audio mixer, MIDI output, gamepads, display
//! scaling/filtering, and user-defined hardware presets.
use doslet_config::{CpuMode, HardwarePreset, InputSettings, KeyboardMode, MouseMode, VideoFilter};
use toml_edit::{DocumentMut, Item, Table, TableLike, value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalSettings {
    pub language: String,
    pub default_preset: String,
    pub scaling: String,
    pub aspect: String,
    pub video_filter: VideoFilter,
    pub audio_enabled: bool,
    pub audio_volume: u8,
    pub audio_mixer: String,
    pub midi_output: String,
    pub game_midi: String,
    pub host_midi: String,
    pub mt32_rom_dir: Option<String>,
    pub keyboard: KeyboardMode,
    pub gameport: bool,
    pub input: InputSettings,
    pub presets: Vec<HardwarePreset>,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            default_preset: "486dx2-66".into(),
            scaling: "fit".into(),
            aspect: "original".into(),
            video_filter: VideoFilter::SharpBilinear,
            audio_enabled: true,
            audio_volume: 100,
            audio_mixer: "system".into(),
            midi_output: "rust".into(),
            game_midi: "gm".into(),
            host_midi: "gm".into(),
            mt32_rom_dir: None,
            keyboard: KeyboardMode::Bios,
            gameport: false,
            input: InputSettings::default(),
            presets: Vec::new(),
        }
    }
}

impl GlobalSettings {
    /// Load global settings from `~/.config/doslet/settings.toml`, or return defaults.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_from_disk() -> Self {
        let Ok(dirs) = crate::host_dirs::HostDirs::detect() else {
            return Self::default();
        };
        let path = dirs.settings_file();
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text).unwrap_or_default()
    }

    /// Save global settings to `~/.config/doslet/settings.toml`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_to_disk(&self) -> Result<(), String> {
        let dirs = crate::host_dirs::HostDirs::detect()?;
        let path = dirs.settings_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, self.export()).map_err(|e| e.to_string())
    }

    pub fn all_presets(&self) -> Vec<HardwarePreset> {
        let mut list = HardwarePreset::builtins();
        for custom in &self.presets {
            if let Some(existing) = list.iter_mut().find(|p| p.id == custom.id) {
                *existing = custom.clone();
            } else {
                list.push(custom.clone());
            }
        }
        list
    }

    pub fn find_preset(&self, id: &str) -> Option<HardwarePreset> {
        self.all_presets().into_iter().find(|p| p.id == id)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let doc = text.parse::<DocumentMut>().map_err(|e| e.to_string())?;
        let t = doc.as_table();
        let mut settings = Self::default();
        if let Some(lang) = t.get("language").and_then(Item::as_str) {
            settings.language = lang.to_owned();
        }
        if let Some(preset) = t.get("default_preset").and_then(Item::as_str) {
            settings.default_preset = preset.to_owned();
        }
        if let Some(display) = t.get("display").and_then(Item::as_table_like) {
            if let Some(v) = display.get("scaling").and_then(Item::as_str) {
                settings.scaling = v.to_owned();
            }
            if let Some(v) = display.get("aspect").and_then(Item::as_str) {
                settings.aspect = v.to_owned();
            }
            if let Some(v) = display.get("video_filter").and_then(Item::as_str) {
                settings.video_filter = v.parse::<VideoFilter>()?;
            }
        }
        if let Some(audio) = t.get("audio").and_then(Item::as_table_like) {
            if let Some(v) = audio.get("enabled").and_then(Item::as_bool) {
                settings.audio_enabled = v;
            }
            if let Some(v) = audio.get("volume").and_then(Item::as_integer) {
                settings.audio_volume = v.clamp(0, 100) as u8;
            }
            if let Some(v) = audio.get("mixer").and_then(Item::as_str) {
                settings.audio_mixer = v.to_owned();
            }
            if let Some(v) = audio.get("midi_output").and_then(Item::as_str) {
                settings.midi_output = v.to_owned();
            }
            if let Some(v) = audio.get("game_midi").and_then(Item::as_str) {
                settings.game_midi = v.to_owned();
            }
            if let Some(v) = audio.get("host_midi").and_then(Item::as_str) {
                settings.host_midi = v.to_owned();
            }
            if let Some(v) = audio.get("mt32_rom_dir").and_then(Item::as_str) {
                settings.mt32_rom_dir = (!v.trim().is_empty()).then(|| v.to_owned());
            }
        }
        if let Some(input) = t.get("input").and_then(Item::as_table_like) {
            if let Some(v) = input.get("keyboard").and_then(Item::as_str) {
                settings.keyboard = if v == "at" {
                    KeyboardMode::At
                } else {
                    KeyboardMode::Bios
                };
            }
            if let Some(v) = input.get("gameport").and_then(Item::as_bool) {
                settings.gameport = v;
            }
            if let Some(v) = input.get("mouse_mode").and_then(Item::as_str) {
                settings.input.mouse_mode = v.parse::<MouseMode>()?;
            }
            if let Some(v) = input.get("hide_cursor").and_then(Item::as_bool) {
                settings.input.hide_cursor = v;
            }
            if let Some(v) = input.get("pad_id").and_then(Item::as_str) {
                settings.input.pad_id = (!v.trim().is_empty()).then(|| v.to_owned());
            }
            for (k, field) in [
                ("pad_index", &mut settings.input.pad_index),
                ("deadzone_percent", &mut settings.input.deadzone_percent),
                ("axis_x", &mut settings.input.axis_x),
                ("axis_y", &mut settings.input.axis_y),
                ("button_1", &mut settings.input.button_1),
                ("button_2", &mut settings.input.button_2),
            ] {
                if let Some(v) = input.get(k).and_then(Item::as_integer)
                    && let Ok(v) = u8::try_from(v)
                {
                    *field = v;
                }
            }
        }
        if let Some(presets) = t.get("presets").and_then(Item::as_table_like) {
            for (id, item) in presets.iter() {
                let Some(p) = item.as_table_like() else {
                    continue;
                };
                settings.presets.push(parse_preset(id, p)?);
            }
        }
        Ok(settings)
    }

    pub fn export(&self) -> String {
        let mut d = DocumentMut::new();
        d["version"] = value(1);
        d["language"] = value(&self.language);
        d["default_preset"] = value(&self.default_preset);

        d["display"]["scaling"] = value(&self.scaling);
        d["display"]["aspect"] = value(&self.aspect);
        d["display"]["video_filter"] = value(self.video_filter.as_str());

        d["audio"]["enabled"] = value(self.audio_enabled);
        d["audio"]["volume"] = value(i64::from(self.audio_volume));
        d["audio"]["mixer"] = value(&self.audio_mixer);
        d["audio"]["midi_output"] = value(&self.midi_output);
        d["audio"]["game_midi"] = value(&self.game_midi);
        d["audio"]["host_midi"] = value(&self.host_midi);
        if let Some(dir) = &self.mt32_rom_dir {
            d["audio"]["mt32_rom_dir"] = value(dir);
        }

        d["input"]["keyboard"] = value(if self.keyboard == KeyboardMode::At {
            "at"
        } else {
            "bios"
        });
        d["input"]["gameport"] = value(self.gameport);
        d["input"]["mouse_mode"] = value(self.input.mouse_mode.as_str());
        d["input"]["hide_cursor"] = value(self.input.hide_cursor);
        if let Some(id) = &self.input.pad_id {
            d["input"]["pad_id"] = value(id);
        }
        for (k, v) in [
            ("pad_index", self.input.pad_index),
            ("deadzone_percent", self.input.deadzone_percent),
            ("axis_x", self.input.axis_x),
            ("axis_y", self.input.axis_y),
            ("button_1", self.input.button_1),
            ("button_2", self.input.button_2),
        ] {
            d["input"][k] = value(i64::from(v));
        }

        if !self.presets.is_empty() {
            let mut presets = Table::new();
            for p in &self.presets {
                let mut t = Table::new();
                t["name"] = value(&p.name);
                t["cpu_mhz"] = value(i64::from(p.cpu_mhz));
                t["cpu_mode"] = value(p.cpu_mode.as_str());
                t["memory_mib"] = value(i64::from(p.memory_mib));
                t["x87"] = value(p.x87);
                t["sb16"] = value(p.sb16);
                t["mpu401"] = value(p.mpu401);
                presets[&p.id] = Item::Table(t);
            }
            d["presets"] = Item::Table(presets);
        }
        d.to_string()
    }
}

fn parse_preset(id: &str, p: &dyn TableLike) -> Result<HardwarePreset, String> {
    let name = p
        .get("name")
        .and_then(Item::as_str)
        .unwrap_or(id)
        .to_owned();
    let cpu_mhz = p
        .get("cpu_mhz")
        .and_then(Item::as_integer)
        .filter(|v| (1..=1000).contains(v))
        .unwrap_or(66) as u16;
    let cpu_mode = match p.get("cpu_mode").and_then(Item::as_str) {
        Some(mode) => mode.parse::<CpuMode>()?,
        None => CpuMode::Adaptive,
    };
    let memory_mib = p
        .get("memory_mib")
        .and_then(Item::as_integer)
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(16);
    let x87 = p.get("x87").and_then(Item::as_bool).unwrap_or(true);
    let sb16 = p.get("sb16").and_then(Item::as_bool).unwrap_or(true);
    let mpu401 = p.get("mpu401").and_then(Item::as_bool).unwrap_or(true);
    Ok(HardwarePreset {
        id: id.to_owned(),
        name,
        cpu_mhz,
        cpu_mode,
        memory_mib,
        x87,
        sb16,
        mpu401,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_settings_round_trip_with_custom_presets() {
        let mut settings = GlobalSettings {
            audio_mixer: "application".into(),
            midi_output: "moont".into(),
            ..GlobalSettings::default()
        };
        settings.input.deadzone_percent = 18;
        settings.presets.push(HardwarePreset {
            id: "fast-dos".into(),
            name: "Fast Protected Mode".into(),
            cpu_mhz: 166,
            cpu_mode: CpuMode::Adaptive,
            memory_mib: 32,
            x87: true,
            sb16: true,
            mpu401: true,
        });
        let exported = settings.export();
        let parsed = GlobalSettings::parse(&exported).unwrap();
        assert_eq!(parsed, settings);
        assert!(parsed.find_preset("fast-dos").is_some());
        assert!(parsed.find_preset("486dx2-66").is_some());
    }
}
