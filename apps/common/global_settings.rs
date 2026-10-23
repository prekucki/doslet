//! Global host settings (`~/.config/doslet/settings.toml`) and hardware presets.
//! Holds host-wide defaults for audio mixer, MIDI output, gamepads, display
//! scaling/filtering, and user-defined hardware presets.
use doslet_config::{
    CpuMode, DpmiHost, HardwarePreset, InputSettings, KeyboardMode, MachineSpec, MouseMode,
    VideoFilter,
};
use toml_edit::{DocumentMut, Item, Table, TableLike, value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalSettings {
    pub language: String,
    pub default_preset: String,
    /// `fit` or `pixel-perfect`.
    pub scaling: String,
    /// Screen shape: `4:3`, `16:9` or `16:9-smart`.
    pub shape: String,
    /// Full screen style: `borderless` or `native`.
    pub fullscreen: String,
    pub video_filter: VideoFilter,
    /// Default `output.video_filter_fallback`.
    pub video_filter_fallback: Option<VideoFilter>,
    /// Default `output.frame_limit`; 0 for no limit.
    pub frame_limit: u16,
    pub audio_enabled: bool,
    pub audio_volume: u8,
    pub audio_mixer: String,
    pub midi_output: String,
    pub game_midi: String,
    pub host_midi: String,
    pub mt32_rom_dir: Option<String>,
    pub dpmi: DpmiHost,
    pub sb_irq: u8,
    pub sb_dma8: u8,
    pub sb_dma16: u8,
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
            shape: "4:3".into(),
            fullscreen: "borderless".into(),
            video_filter: VideoFilter::SharpBilinear,
            video_filter_fallback: None,
            frame_limit: 0,
            audio_enabled: true,
            audio_volume: 100,
            audio_mixer: "system".into(),
            midi_output: "rust".into(),
            game_midi: "gm".into(),
            host_midi: "gm".into(),
            mt32_rom_dir: None,
            dpmi: DpmiHost::Direct,
            sb_irq: 7,
            sb_dma8: 1,
            sb_dma16: 5,
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

    /// Fills every setting a version-2 profile leaves out with these global
    /// defaults, one key at a time, so a profile only overrides what it names.
    pub fn fill_profile_defaults(&self, spec: &mut MachineSpec, profile: &DocumentMut) {
        let has = |table: &str, key: &str| {
            profile
                .get(table)
                .and_then(Item::as_table_like)
                .is_some_and(|t| t.contains_key(key))
        };
        let preset = match &spec.preset {
            // The parser already applied built-in presets.
            Some(id) if HardwarePreset::find_builtin(id).is_none() => self.find_preset(id),
            None => self.find_preset(&self.default_preset),
            _ => None,
        };
        if let Some(preset) = preset {
            let mut hardware = spec.hardware;
            let mut mode = spec.cpu_mode;
            preset.apply_to(&mut hardware, &mut mode);
            if !has("machine", "cpu_mhz") && !has("machine", "clock_hz") {
                spec.hardware.cpu_mhz = hardware.cpu_mhz;
            }
            if !has("machine", "memory_mib") {
                spec.hardware.memory_mib = hardware.memory_mib;
            }
            if !has("machine", "x87") {
                spec.hardware.x87 = hardware.x87;
            }
            if !has("machine", "cpu_mode") {
                spec.cpu_mode = mode;
            }
            if !has("sound", "sb16") {
                spec.hardware.sb16 = hardware.sb16;
            }
            if !has("sound", "mpu401") {
                spec.hardware.mpu401 = hardware.mpu401;
            }
        }
        if !has("machine", "dpmi") {
            spec.dpmi = Some(self.dpmi);
        }
        if !has("sound", "irq") {
            spec.hardware.sb_irq = self.sb_irq;
        }
        if !has("sound", "dma8") {
            spec.hardware.sb_dma8 = self.sb_dma8;
        }
        if !has("sound", "dma16") {
            spec.hardware.sb_dma16 = self.sb_dma16;
        }
        if !has("output", "video_filter") {
            spec.output.video_filter = self.video_filter;
        }
        if !has("output", "video_filter_fallback") {
            spec.output.video_filter_fallback = self.video_filter_fallback;
        }
        if !has("output", "frame_limit") {
            spec.output.frame_limit = self.frame_limit;
        }
        if !has("output", "game_midi") {
            spec.output.game_midi = self.game_midi.clone();
        }
        if !has("output", "host_midi") {
            spec.output.host_midi = self.host_midi.clone();
        }
        if !has("output", "midi_output") && !self.midi_output.is_empty() {
            spec.output.midi_output = self.midi_output.clone();
        }
        if !has("input", "keyboard") {
            spec.hardware.keyboard = self.keyboard;
        }
        if !has("input", "gameport") {
            spec.hardware.gameport = self.gameport;
        }
        let input = &mut spec.input;
        let global = &self.input;
        if !has("input", "mouse_mode") && !has("input", "capture") {
            input.mouse_mode = global.mouse_mode;
        }
        if !has("input", "hide_cursor") {
            input.hide_cursor = global.hide_cursor;
        }
        if !has("input", "pad_id") {
            input.pad_id.clone_from(&global.pad_id);
        }
        for (key, target, value) in [
            ("pad_index", &mut input.pad_index, global.pad_index),
            (
                "deadzone_percent",
                &mut input.deadzone_percent,
                global.deadzone_percent,
            ),
            ("axis_x", &mut input.axis_x, global.axis_x),
            ("axis_y", &mut input.axis_y, global.axis_y),
            ("button_1", &mut input.button_1, global.button_1),
            ("button_2", &mut input.button_2, global.button_2),
        ] {
            if !has("input", key) {
                *target = value;
            }
        }
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
                // Earlier versions wrote `integer`.
                settings.scaling = if v == "integer" { "pixel-perfect" } else { v }.to_owned();
            }
            if let Some(v) = display.get("shape").and_then(Item::as_str) {
                settings.shape = v.to_owned();
            }
            if let Some(v) = display.get("fullscreen").and_then(Item::as_str) {
                settings.fullscreen = v.to_owned();
            }
            if let Some(v) = display.get("video_filter").and_then(Item::as_str) {
                settings.video_filter = v.parse::<VideoFilter>()?;
            }
            if let Some(v) = display.get("video_filter_fallback").and_then(Item::as_str) {
                settings.video_filter_fallback =
                    Some(v.parse::<VideoFilter>()?).filter(|filter| !filter.needs_gpu());
            }
            if let Some(v) = display.get("frame_limit").and_then(Item::as_integer)
                && let Ok(v) = u16::try_from(v)
                && (v == 0 || doslet_config::OutputRouting::FRAME_LIMITS.contains(&v))
            {
                settings.frame_limit = v;
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
        if let Some(machine) = t.get("machine").and_then(Item::as_table_like)
            && let Some(v) = machine.get("dpmi").and_then(Item::as_str)
        {
            settings.dpmi =
                DpmiHost::parse(v).ok_or("machine.dpmi must be direct, reflect or off")?;
        }
        if let Some(sound) = t.get("sound").and_then(Item::as_table_like) {
            for (key, field, allowed) in [
                ("irq", &mut settings.sb_irq, &[5, 7][..]),
                ("dma8", &mut settings.sb_dma8, &[0, 1, 3]),
                ("dma16", &mut settings.sb_dma16, &[5, 6, 7]),
            ] {
                if let Some(v) = sound.get(key).and_then(Item::as_integer)
                    && let Ok(v) = u8::try_from(v)
                    && allowed.contains(&v)
                {
                    *field = v;
                }
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
        d["display"]["shape"] = value(&self.shape);
        d["display"]["fullscreen"] = value(&self.fullscreen);
        d["display"]["video_filter"] = value(self.video_filter.as_str());
        d["display"]["frame_limit"] = value(i64::from(self.frame_limit));
        if let Some(fallback) = self.video_filter_fallback {
            d["display"]["video_filter_fallback"] = value(fallback.as_str());
        }

        d["audio"]["enabled"] = value(self.audio_enabled);
        d["audio"]["volume"] = value(i64::from(self.audio_volume));
        d["audio"]["mixer"] = value(&self.audio_mixer);
        d["audio"]["midi_output"] = value(&self.midi_output);
        d["audio"]["game_midi"] = value(&self.game_midi);
        d["audio"]["host_midi"] = value(&self.host_midi);
        if let Some(dir) = &self.mt32_rom_dir {
            d["audio"]["mt32_rom_dir"] = value(dir);
        }

        d["machine"]["dpmi"] = value(self.dpmi.as_str());
        d["sound"]["irq"] = value(i64::from(self.sb_irq));
        d["sound"]["dma8"] = value(i64::from(self.sb_dma8));
        d["sound"]["dma16"] = value(i64::from(self.sb_dma16));

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
        settings.input.axis_x = 2;
        settings.input.pad_id = Some("8BitDo".into());
        settings.dpmi = DpmiHost::Reflect;
        settings.sb_irq = 5;
        settings.sb_dma16 = 7;
        settings.frame_limit = 35;
        settings.shape = "16:9-smart".into();
        settings.fullscreen = "native".into();
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

    #[test]
    fn older_settings_keep_working() {
        let parsed =
            GlobalSettings::parse("version=1\n[display]\nscaling='integer'\naspect='4:3'\n")
                .unwrap();
        assert_eq!(parsed.scaling, "pixel-perfect");
        assert_eq!(parsed.shape, "4:3");
    }

    #[test]
    fn global_defaults_fill_only_what_a_profile_leaves_out() {
        let settings = GlobalSettings {
            dpmi: DpmiHost::Off,
            sb_irq: 5,
            frame_limit: 35,
            keyboard: KeyboardMode::At,
            ..GlobalSettings::default()
        };
        let profile: DocumentMut = "[sound]\nirq = 7\n[input]\nkeyboard = 'bios'\n"
            .parse()
            .unwrap();
        let mut spec = MachineSpec::default();
        settings.fill_profile_defaults(&mut spec, &profile);
        assert_eq!(spec.dpmi, Some(DpmiHost::Off));
        assert_eq!(spec.output.frame_limit, 35);
        // The profile's own values stay.
        assert_eq!(spec.hardware.sb_irq, 7);
        assert_eq!(spec.hardware.keyboard, KeyboardMode::Bios);
        // The default preset supplies the machine.
        assert_eq!(spec.hardware.cpu_mhz, 66);
    }
}
