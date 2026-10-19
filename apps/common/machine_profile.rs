//! Shared native/browser TOML profile codec. No filesystem operations occur here.
use doslet_config::*;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, TableLike, value};
fn fields(t: &dyn TableLike, allowed: &[&str], prefix: &str) -> Result<(), String> {
    for (k, _) in t.iter() {
        if !allowed.contains(&k) {
            return Err(format!("unknown field {prefix}{k}"));
        }
    }
    Ok(())
}
fn section<'a>(t: &'a dyn TableLike, k: &str) -> Result<Option<&'a dyn TableLike>, String> {
    t.get(k)
        .map(|i| {
            i.as_table_like()
                .ok_or_else(|| format!("{k} must be a table"))
        })
        .transpose()
}
fn string(t: &dyn TableLike, k: &str) -> Result<Option<String>, String> {
    t.get(k)
        .map(|i| {
            i.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{k} must be a string"))
        })
        .transpose()
}
fn boolean(t: &dyn TableLike, k: &str) -> Result<Option<bool>, String> {
    t.get(k)
        .map(|i| i.as_bool().ok_or_else(|| format!("{k} must be a boolean")))
        .transpose()
}
fn number(t: &dyn TableLike, k: &str) -> Result<Option<u8>, String> {
    t.get(k)
        .map(|i| {
            i.as_integer()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or_else(|| format!("{k} must be an integer from 0 to 255"))
        })
        .transpose()
}
fn required(t: &dyn TableLike, k: &str) -> Result<String, String> {
    string(t, k)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{k} is required"))
}
pub(crate) fn cpu_mhz(t: &dyn TableLike) -> Result<Option<u16>, String> {
    let mhz = t
        .get("cpu_mhz")
        .map(|item| {
            item.as_integer()
                .filter(|v| (1..=1000).contains(v))
                .map(|v| v as u16)
                .ok_or("machine.cpu_mhz must be an integer from 1 to 1000")
        })
        .transpose()?;
    let legacy = t
        .get("clock_hz")
        .map(|item| {
            item.as_integer()
                .filter(|v| (1_000_000..=1_000_000_000).contains(v) && v % 1_000_000 == 0)
                .map(|v| (v / 1_000_000) as u16)
                .ok_or("machine.clock_hz must be a whole number of MHz from 1 to 1000")
        })
        .transpose()?;
    if mhz.is_some() && legacy.is_some() && mhz != legacy {
        return Err("machine.cpu_mhz and machine.clock_hz disagree".into());
    }
    Ok(mhz.or(legacy))
}
pub fn patch_paths(game: &dyn TableLike) -> Result<Vec<String>, String> {
    let Some(item) = game.get("patches") else {
        return Ok(Vec::new());
    };
    let paths = item
        .as_array()
        .ok_or("game.patches must be an array of paths")?;
    if paths.len() > 32 {
        return Err("game.patches supports at most 32 files".into());
    }
    paths
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "game.patches must contain nonempty paths".into())
        })
        .collect()
}
fn startup_keys(game: &dyn TableLike) -> Result<String, String> {
    let Some(item) = game.get("startup_keys") else {
        return Ok(String::new());
    };
    let text = if let Some(value) = item.as_str() {
        value.to_owned()
    } else if let Some(values) = item.as_array() {
        let mut text = String::new();
        for value in values.iter() {
            if let Some(character) = value.as_str().filter(|value| value.len() == 1) {
                text.push_str(character);
            } else if let Some(digit) = value.as_integer().filter(|digit| (0..=9).contains(digit)) {
                text.push(char::from(b'0' + digit as u8));
            } else {
                return Err("game.startup_keys array entries must be single ASCII characters or digits 0 through 9".into());
            }
        }
        text
    } else {
        return Err("game.startup_keys must be a string or an array of characters".into());
    };
    if text.len() > 15 {
        return Err("game.startup_keys supports at most 15 characters".into());
    }
    if !text
        .bytes()
        .all(|byte| doslet_config::bios_key_for_ascii(byte).is_some())
    {
        return Err(
            "game.startup_keys supports printable ASCII and Enter, Tab, Backspace or Escape".into(),
        );
    }
    Ok(text)
}

/// A nonempty, bounded label, or a list of them.
fn labels(t: &dyn TableLike, k: &str) -> Result<Vec<String>, String> {
    let error = || format!("info.{k} must be a nonempty string or at most 16 of them");
    let Some(item) = t.get(k) else {
        return Ok(Vec::new());
    };
    let values: Vec<_> = match item.as_array() {
        Some(array) => array.iter().map(|v| v.as_str()).collect(),
        None => vec![item.as_str()],
    };
    if values.is_empty() || values.len() > 16 {
        return Err(error());
    }
    values
        .into_iter()
        .map(|v| {
            v.filter(|s| !s.trim().is_empty() && s.chars().count() <= 200)
                .map(str::to_owned)
                .ok_or_else(error)
        })
        .collect()
}
fn game_info(t: &dyn TableLike) -> Result<GameInfo, String> {
    fields(
        t,
        &[
            "cover",
            "released",
            "publishers",
            "developers",
            "genres",
            "gameplay",
            "description",
        ],
        "info.",
    )?;
    let cover = string(t, "cover")?;
    if cover.as_ref().is_some_and(String::is_empty) {
        return Err("info.cover must be a nonempty path".into());
    }
    let released = t
        .get("released")
        .map(|v| {
            v.as_integer()
                .filter(|year| (1970..=2100).contains(year))
                .map(|year| year as u16)
                .ok_or("info.released must be a year from 1970 to 2100")
        })
        .transpose()?;
    let description = string(t, "description")?;
    if description
        .as_ref()
        .is_some_and(|text| text.trim().is_empty() || text.chars().count() > 4000)
    {
        return Err("info.description must be nonempty text of at most 4000 characters".into());
    }
    Ok(GameInfo {
        cover,
        released,
        publishers: labels(t, "publishers")?,
        developers: labels(t, "developers")?,
        genres: labels(t, "genres")?,
        gameplay: labels(t, "gameplay")?,
        description,
    })
}

pub fn parse_machine_profile(text: &str) -> Result<MachineSpec, String> {
    let doc = text.parse::<DocumentMut>().map_err(|e| e.to_string())?;
    let t = doc.as_table();
    if t.get("version").and_then(Item::as_integer) == Some(1) {
        return parse_v1(t);
    }
    fields(
        t,
        &[
            "version", "name", "machine", "sound", "input", "output", "game", "info", "drives",
            "bindings",
        ],
        "",
    )?;
    if t.get("version").and_then(Item::as_integer) != Some(2) {
        return Err("version must be the integer 2".into());
    }
    let mut spec = MachineSpec {
        name: string(t, "name")?.unwrap_or_default(),
        ..Default::default()
    };
    if let Some(m) = section(t, "machine")? {
        fields(
            m,
            &[
                "platform",
                "cpu",
                "engine",
                "timing",
                "clock_hz",
                "cpu_mhz",
                "memory_mib",
                "x87",
                "video",
                "vram_kib",
            ],
            "machine.",
        )?;
        for (key, expected) in [
            ("platform", "pc-at"),
            ("cpu", "i386"),
            ("engine", "interpreter"),
            ("timing", "functional"),
            ("video", "vga-vbe"),
        ] {
            if string(m, key)?.is_some_and(|v| v != expected) {
                return Err(format!("machine.{key} supports only {expected}"));
            }
        }
        if let Some(item) = m.get("vram_kib")
            && item.as_integer() != Some(512)
        {
            return Err("machine.vram_kib must be 512".into());
        }
        if let Some(v) = cpu_mhz(m)? {
            spec.hardware.cpu_mhz = v;
        }
        if let Some(v) = number(m, "memory_mib")? {
            spec.hardware.memory_mib = v;
        }
        if let Some(v) = boolean(m, "x87")? {
            spec.hardware.x87 = v;
        }
    }
    if let Some(s) = section(t, "sound")? {
        fields(s, &["sb16", "mpu401", "irq", "dma8", "dma16"], "sound.")?;
        if let Some(v) = boolean(s, "sb16")? {
            spec.hardware.sb16 = v;
        }
        if let Some(v) = boolean(s, "mpu401")? {
            spec.hardware.mpu401 = v;
        }
        if let Some(v) = number(s, "irq")? {
            spec.hardware.sb_irq = v;
        }
        if let Some(v) = number(s, "dma8")? {
            spec.hardware.sb_dma8 = v;
        }
        if let Some(v) = number(s, "dma16")? {
            spec.hardware.sb_dma16 = v;
        }
    }
    if let Some(i) = section(t, "input")? {
        fields(
            i,
            &[
                "keyboard",
                "gameport",
                "capture",
                "mouse_mode",
                "hide_cursor",
                "pad_index",
                "pad_id",
                "deadzone_percent",
                "axis_x",
                "axis_y",
                "button_1",
                "button_2",
            ],
            "input.",
        )?;
        if let Some(k) = string(i, "keyboard")? {
            spec.hardware.keyboard = match k.as_str() {
                "bios" => KeyboardMode::Bios,
                "at" => KeyboardMode::At,
                _ => return Err("input.keyboard must be bios or at".into()),
            };
        }
        if let Some(v) = boolean(i, "gameport")? {
            spec.hardware.gameport = v;
        }
        spec.input.pad_id = string(i, "pad_id")?.filter(|s| !s.is_empty());
        let legacy_capture = boolean(i, "capture")?;
        if let Some(value) = string(i, "mouse_mode")? {
            if legacy_capture.is_some() {
                return Err("input.capture and input.mouse_mode cannot be combined".into());
            }
            spec.input.mouse_mode = value.parse::<MouseMode>()?;
        } else if let Some(capture) = legacy_capture {
            spec.input.mouse_mode = if capture {
                MouseMode::Auto
            } else {
                MouseMode::NoCapture
            };
        }
        if let Some(v) = boolean(i, "hide_cursor")? {
            spec.input.hide_cursor = v;
        }
        for (key, target) in [
            ("pad_index", &mut spec.input.pad_index),
            ("deadzone_percent", &mut spec.input.deadzone_percent),
            ("axis_x", &mut spec.input.axis_x),
            ("axis_y", &mut spec.input.axis_y),
            ("button_1", &mut spec.input.button_1),
            ("button_2", &mut spec.input.button_2),
        ] {
            if let Some(v) = number(i, key)? {
                *target = v;
            }
        }
    }
    if let Some(o) = section(t, "output")? {
        fields(
            o,
            &[
                "audio",
                "video_filter",
                "game_midi",
                "host_midi",
                "midi_output",
                "rom_binding",
            ],
            "output.",
        )?;
        if let Some(v) = boolean(o, "audio")? {
            spec.output.audio = v;
        }
        if let Some(v) = string(o, "video_filter")? {
            spec.output.video_filter = v.parse::<VideoFilter>()?;
        }
        if let Some(v) = string(o, "game_midi")? {
            spec.output.game_midi = v;
        }
        if let Some(v) = string(o, "host_midi")? {
            spec.output.host_midi = v;
        }
        if let Some(v) = string(o, "midi_output")? {
            spec.output.midi_output = v;
        }
        spec.output.rom_binding = string(o, "rom_binding")?;
    }
    if let Some(info) = section(t, "info")? {
        spec.info = game_info(info)?;
    }
    if let Some(g) = section(t, "game")? {
        fields(
            g,
            &[
                "executable",
                "arguments",
                "working_directory",
                "patches",
                "startup_keys",
            ],
            "game.",
        )?;
        spec.game.executable = string(g, "executable")?.unwrap_or_default();
        spec.game.patches = patch_paths(g)?;
        spec.game.startup_keys = startup_keys(g)?;
        if let Some(v) = string(g, "working_directory")? {
            spec.game.working_directory = v;
        }
        if let Some(a) = g.get("arguments") {
            for v in a
                .as_array()
                .ok_or("game.arguments must be an array")?
                .iter()
            {
                spec.game.arguments.push(
                    v.as_str()
                        .ok_or("game.arguments must contain only strings")?
                        .into(),
                );
            }
        }
    }
    if let Some(d) = t.get("drives") {
        for d in d
            .as_array_of_tables()
            .ok_or("drives must use [[drives]]")?
            .iter()
        {
            fields(
                d,
                &["letter", "kind", "source", "policy", "store"],
                "drives.",
            )?;
            let letter = required(d, "letter")?;
            if letter.len() != 1 {
                return Err("drives.letter must contain one letter".into());
            }
            let kind = match required(d, "kind")?.as_str() {
                "directory" => SourceKind::NativeDirectory,
                "zip" => SourceKind::BrowserZip,
                "iso" => SourceKind::LocalIso,
                "https-iso" => SourceKind::HttpsIso,
                _ => return Err("unsupported drives.kind".into()),
            };
            let policy = match string(d, "policy")?.as_deref().unwrap_or(
                if matches!(kind, SourceKind::LocalIso | SourceKind::HttpsIso) {
                    "read-only"
                } else {
                    "temporary"
                },
            ) {
                "read-only" => WritePolicy::ReadOnly,
                "temporary" => WritePolicy::Temporary,
                "persistent" => WritePolicy::Persistent,
                "legacy-rw" => WritePolicy::LegacyWriteback,
                _ => return Err("unsupported drives.policy".into()),
            };
            spec.mounts.push(MountSpec {
                letter: letter.as_bytes()[0].to_ascii_uppercase(),
                source: required(d, "source")?,
                kind,
                policy,
                store: string(d, "store")?,
            });
        }
    }
    if let Some(b) = t.get("bindings") {
        for b in b
            .as_array_of_tables()
            .ok_or("bindings must use [[bindings]]")?
            .iter()
        {
            fields(b, &["id", "kind", "location"], "bindings.")?;
            let kind = match required(b, "kind")?.as_str() {
                "directory" => BindingKind::NativeDirectory,
                "zip" => BindingKind::BrowserZip,
                "iso" => BindingKind::LocalIso,
                "https-iso" => BindingKind::HttpsIso,
                "native-store" => BindingKind::NativeStore,
                "indexeddb" => BindingKind::IndexedDb,
                _ => {
                    return Err("unsupported bindings.kind (browser storage uses indexeddb)".into());
                }
            };
            spec.host.bindings.push(HostBinding {
                id: required(b, "id")?,
                kind,
                location: required(b, "location")?,
            });
        }
    }
    spec.resolve().map_err(|e| e.to_string())?;
    Ok(spec)
}

fn parse_v1(t: &dyn TableLike) -> Result<MachineSpec, String> {
    fields(
        t,
        &[
            "version", "name", "game", "drives", "midi", "audio", "machine",
        ],
        "",
    )?;
    let mut spec = MachineSpec {
        name: string(t, "name")?.unwrap_or_default(),
        ..Default::default()
    };
    if let Some(machine) = section(t, "machine")? {
        fields(machine, &["cpu_mhz", "clock_hz"], "machine.")?;
        if let Some(v) = cpu_mhz(machine)? {
            spec.hardware.cpu_mhz = v;
        }
    }
    if let Some(game) = section(t, "game")? {
        fields(
            game,
            &["executable", "arguments", "working_directory", "patches"],
            "game.",
        )?;
        spec.game.executable = string(game, "executable")?.unwrap_or_default();
        spec.game.patches = patch_paths(game)?;
        if let Some(v) = string(game, "working_directory")? {
            spec.game.working_directory = v;
        }
        if let Some(arguments) = game.get("arguments") {
            for argument in arguments
                .as_array()
                .ok_or("game.arguments must be an array of strings")?
                .iter()
            {
                spec.game.arguments.push(
                    argument
                        .as_str()
                        .ok_or("game.arguments must contain only strings")?
                        .into(),
                );
            }
        }
    }
    if let Some(midi) = section(t, "midi")? {
        fields(
            midi,
            &["game_mode", "host_mode", "output", "rom_directory"],
            "midi.",
        )?;
        if let Some(v) = string(midi, "game_mode")? {
            spec.output.game_midi = v;
        }
        if let Some(v) = string(midi, "host_mode")? {
            spec.output.host_midi = v;
        }
        if let Some(v) = string(midi, "output")? {
            spec.output.midi_output = v;
        }
        spec.output.rom_binding = string(midi, "rom_directory")?;
    }
    if let Some(audio) = section(t, "audio")? {
        fields(audio, &["enabled"], "audio.")?;
        if let Some(v) = boolean(audio, "enabled")? {
            spec.output.audio = v;
        }
    }
    if let Some(drives) = t.get("drives") {
        let drives = drives
            .as_array_of_tables()
            .ok_or("drives must use [[drives]] tables")?;
        if drives.is_empty() || drives.len() > 2 {
            return Err("drives supports one C: directory and at most one CD-ROM image".into());
        }
        for drive in drives.iter() {
            fields(
                drive,
                &["letter", "kind", "source", "writable", "mode"],
                "drives.",
            )?;
            let letter = required(drive, "letter")?;
            if letter.len() != 1 {
                return Err("drives.letter must contain one letter".into());
            }
            let letter = letter.as_bytes()[0].to_ascii_uppercase();
            let kind = match required(drive, "kind")?.as_str() {
                "directory" => SourceKind::NativeDirectory,
                "cdrom" => SourceKind::LocalIso,
                _ => return Err("drives.kind must be directory or cdrom".into()),
            };
            let writable = boolean(drive, "writable")?;
            let mode = string(drive, "mode")?;
            if kind == SourceKind::NativeDirectory && writable.is_some() && mode.is_some() {
                return Err("drives.mode and drives.writable are mutually exclusive".into());
            }
            let policy = match mode.as_deref() {
                Some("ro") => WritePolicy::ReadOnly,
                Some("overlay") => WritePolicy::Temporary,
                Some("rw") => WritePolicy::LegacyWriteback,
                Some(_) => return Err("drives.mode must be ro, overlay or rw".into()),
                None => {
                    if writable.unwrap_or(kind == SourceKind::NativeDirectory) {
                        WritePolicy::Temporary
                    } else {
                        WritePolicy::ReadOnly
                    }
                }
            };
            spec.mounts.push(MountSpec {
                letter,
                kind,
                source: required(drive, "source")?,
                policy,
                store: None,
            });
        }
        if !spec
            .mounts
            .iter()
            .any(|mount| mount.letter == b'C' && mount.kind == SourceKind::NativeDirectory)
        {
            return Err("drives must include a C: directory".into());
        }
    }
    spec.resolve().map_err(|e| e.to_string())?;
    Ok(spec)
}
pub fn export_machine_profile(spec: &MachineSpec) -> String {
    let mut d = DocumentMut::new();
    d["version"] = value(2);
    d["name"] = value(&spec.name);
    for (k, v) in [
        ("platform", "pc-at"),
        ("cpu", "i386"),
        ("engine", "interpreter"),
        ("timing", "functional"),
        ("video", "vga-vbe"),
    ] {
        d["machine"][k] = value(v);
    }
    d["machine"]["cpu_mhz"] = value(i64::from(spec.hardware.cpu_mhz));
    d["machine"]["vram_kib"] = value(512);
    d["machine"]["memory_mib"] = value(i64::from(spec.hardware.memory_mib));
    d["machine"]["x87"] = value(spec.hardware.x87);
    d["sound"]["sb16"] = value(spec.hardware.sb16);
    d["sound"]["mpu401"] = value(spec.hardware.mpu401);
    for (k, v) in [
        ("irq", spec.hardware.sb_irq),
        ("dma8", spec.hardware.sb_dma8),
        ("dma16", spec.hardware.sb_dma16),
    ] {
        d["sound"][k] = value(i64::from(v));
    }
    d["input"]["keyboard"] = value(if spec.hardware.keyboard == KeyboardMode::At {
        "at"
    } else {
        "bios"
    });
    d["input"]["gameport"] = value(spec.hardware.gameport);
    d["input"]["mouse_mode"] = value(spec.input.mouse_mode.as_str());
    d["input"]["hide_cursor"] = value(spec.input.hide_cursor);
    if let Some(id) = &spec.input.pad_id {
        d["input"]["pad_id"] = value(id);
    }
    for (k, v) in [
        ("pad_index", spec.input.pad_index),
        ("deadzone_percent", spec.input.deadzone_percent),
        ("axis_x", spec.input.axis_x),
        ("axis_y", spec.input.axis_y),
        ("button_1", spec.input.button_1),
        ("button_2", spec.input.button_2),
    ] {
        d["input"][k] = value(i64::from(v));
    }
    d["output"]["audio"] = value(spec.output.audio);
    d["output"]["video_filter"] = value(spec.output.video_filter.as_str());
    d["output"]["game_midi"] = value(&spec.output.game_midi);
    d["output"]["host_midi"] = value(&spec.output.host_midi);
    d["output"]["midi_output"] = value(&spec.output.midi_output);
    if let Some(v) = &spec.output.rom_binding {
        d["output"]["rom_binding"] = value(v);
    }
    d["game"]["executable"] = value(&spec.game.executable);
    d["game"]["working_directory"] = value(&spec.game.working_directory);
    let mut a = Array::new();
    for arg in &spec.game.arguments {
        a.push(arg.as_str());
    }
    d["game"]["arguments"] = value(a);
    if !spec.game.patches.is_empty() {
        let mut paths = Array::new();
        for path in &spec.game.patches {
            paths.push(path.as_str());
        }
        d["game"]["patches"] = value(paths);
    }
    if !spec.game.startup_keys.is_empty() {
        d["game"]["startup_keys"] = value(&spec.game.startup_keys);
    }
    let info = &spec.info;
    if let Some(cover) = &info.cover {
        d["info"]["cover"] = value(cover);
    }
    if let Some(year) = info.released {
        d["info"]["released"] = value(i64::from(year));
    }
    for (key, list) in [
        ("publishers", &info.publishers),
        ("developers", &info.developers),
        ("genres", &info.genres),
        ("gameplay", &info.gameplay),
    ] {
        if !list.is_empty() {
            d["info"][key] = value(list.iter().map(String::as_str).collect::<Array>());
        }
    }
    if let Some(description) = &info.description {
        d["info"]["description"] = value(description);
    }
    let mut drives = ArrayOfTables::new();
    for mount in &spec.mounts {
        let mut t = Table::new();
        t["letter"] = value(char::from(mount.letter).to_string());
        t["kind"] = value(match mount.kind {
            SourceKind::NativeDirectory => "directory",
            SourceKind::BrowserZip => "zip",
            SourceKind::LocalIso => "iso",
            SourceKind::HttpsIso => "https-iso",
        });
        t["source"] = value(&mount.source);
        t["policy"] = value(match mount.policy {
            WritePolicy::ReadOnly => "read-only",
            WritePolicy::Temporary => "temporary",
            WritePolicy::Persistent => "persistent",
            WritePolicy::LegacyWriteback => "legacy-rw",
        });
        if let Some(store) = &mount.store {
            t["store"] = value(store);
        }
        drives.push(t);
    }
    if !drives.is_empty() {
        d["drives"] = Item::ArrayOfTables(drives);
    }
    let mut bindings = ArrayOfTables::new();
    for b in &spec.host.bindings {
        let mut t = Table::new();
        t["id"] = value(&b.id);
        t["location"] = value(&b.location);
        t["kind"] = value(match b.kind {
            BindingKind::NativeDirectory => "directory",
            BindingKind::BrowserZip => "zip",
            BindingKind::LocalIso => "iso",
            BindingKind::HttpsIso => "https-iso",
            BindingKind::NativeStore => "native-store",
            BindingKind::IndexedDb => "indexeddb",
        });
        bindings.push(t);
    }
    if !bindings.is_empty() {
        d["bindings"] = Item::ArrayOfTables(bindings);
    }
    d.to_string()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn video_filter_round_trips_and_rejects_unknown_values() {
        for filter in [
            VideoFilter::Nearest,
            VideoFilter::SharpBilinear,
            VideoFilter::Hq2x,
        ] {
            let profile = format!("version=2\n[output]\nvideo_filter='{}'\n", filter.as_str());
            let spec = parse_machine_profile(&profile).unwrap();
            assert_eq!(spec.output.video_filter, filter);
            assert_eq!(
                parse_machine_profile(&export_machine_profile(&spec))
                    .unwrap()
                    .output
                    .video_filter,
                filter
            );
        }
        assert_eq!(
            parse_machine_profile("version=2")
                .unwrap()
                .output
                .video_filter,
            VideoFilter::Nearest
        );
        for value in ["'bilinear'", "42"] {
            assert!(
                parse_machine_profile(&format!("version=2\n[output]\nvideo_filter={value}"))
                    .is_err()
            );
        }
    }
    #[test]
    fn startup_text_accepts_printable_ascii_and_legacy_digit_arrays() {
        for (value, expected) in [
            ("\"15\"", "15"),
            ("[1, 5]", "15"),
            ("[\"A\", \"?\"]", "A?"),
            ("\"A?\\n\"", "A?\n"),
        ] {
            let profile = format!("version=2\n[game]\nstartup_keys={value}\n");
            let spec = parse_machine_profile(&profile).unwrap();
            assert_eq!(spec.game.startup_keys, expected);
            assert_eq!(
                parse_machine_profile(&export_machine_profile(&spec))
                    .unwrap()
                    .game
                    .startup_keys,
                expected
            );
        }
        for value in ["\"zażółć\"", "\"\\u0000\"", "[10]", "[\"AB\"]"] {
            assert!(
                parse_machine_profile(&format!("version=2\n[game]\nstartup_keys={value}\n"))
                    .is_err()
            );
        }
    }
    #[test]
    fn documented_profile_is_accepted() {
        let guide = include_str!("../../docs/configuration.md");
        let profile = guide
            .split("```toml\n")
            .nth(1)
            .unwrap()
            .split("```")
            .next()
            .unwrap();
        let spec = parse_machine_profile(profile).unwrap();
        assert_eq!(spec.mounts[0].policy, WritePolicy::Persistent);
        assert!(spec.resolve().is_ok());
    }
    #[test]
    fn shared_version_one_import_preserves_paths_modes_arguments_and_defaults() {
        let spec=parse_machine_profile("version=1\nname='Legacy'\n[game]\nexecutable='../GAME.EXE'\narguments=['/test','two words']\nworking_directory='C:\\SUB'\n[midi]\ngame_mode='mt32'\nhost_mode='gm'\noutput='rust'\n[audio]\nenabled=false\n[[drives]]\nletter='C'\nkind='directory'\nsource='../game'\nmode='rw'\n[[drives]]\nletter='D'\nkind='cdrom'\nsource='../disc.iso'\nwritable=false\nmode='ro'\n").unwrap();
        assert_eq!(spec.hardware, HardwareConfig::default());
        assert_eq!(spec.game.executable, "../GAME.EXE");
        assert_eq!(spec.mounts[0].source, "../game");
        assert_eq!(spec.mounts[0].policy, WritePolicy::LegacyWriteback);
        assert_eq!(spec.mounts[1].policy, WritePolicy::ReadOnly);
        assert!(!spec.output.audio);
        assert_eq!(
            spec.resolve().unwrap().command_tail,
            b" /test \"two words\""
        );
        assert_eq!(
            parse_machine_profile(&export_machine_profile(&spec)).unwrap(),
            spec
        );
    }
    #[test]
    fn round_trip() {
        let mut s = MachineSpec {
            name: "quotes \" and \\".into(),
            ..Default::default()
        };
        s.hardware.keyboard = KeyboardMode::At;
        s.hardware.cpu_mhz = 132;
        s.input.deadzone_percent = 20;
        s.input.mouse_mode = MouseMode::NoCapture;
        s.input.hide_cursor = true;
        s.info = GameInfo {
            cover: Some("art/cover.jpg".into()),
            released: Some(1994),
            publishers: vec!["Apogee Software, Ltd.".into()],
            developers: vec!["Apogee Software, Ltd.".into()],
            genres: vec!["Action".into()],
            gameplay: vec!["Shooter".into(), "Arcade".into()],
            description: Some("First paragraph.\n\nSecond \"quoted\" one.".into()),
        };
        assert_eq!(
            parse_machine_profile(&export_machine_profile(&s)).unwrap(),
            s
        );
    }
    #[test]
    fn mouse_capture_modes_round_trip_and_legacy_capture_remains_readable() {
        for mode in MouseMode::ALL {
            let profile = format!("version=2\n[input]\nmouse_mode='{}'", mode.as_str());
            let spec = parse_machine_profile(&profile).unwrap();
            assert_eq!(spec.input.mouse_mode, mode);
            assert_eq!(
                parse_machine_profile(&export_machine_profile(&spec)).unwrap(),
                spec
            );
        }
        for (capture, mode) in [(true, MouseMode::Auto), (false, MouseMode::NoCapture)] {
            let spec =
                parse_machine_profile(&format!("version=2\n[input]\ncapture={capture}")).unwrap();
            assert_eq!(spec.input.mouse_mode, mode);
        }
        assert_eq!(
            parse_machine_profile("version=2").unwrap().input.mouse_mode,
            MouseMode::Auto
        );
        for input in [
            "mouse_mode='invalid'",
            "mouse_mode=true",
            "mouse_mode='auto'\ncapture=false",
        ] {
            assert!(parse_machine_profile(&format!("version=2\n[input]\n{input}")).is_err());
        }
    }

    #[test]
    fn cpu_clock_supports_both_profile_versions_and_rejects_ambiguous_values() {
        for version in [1, 2] {
            for setting in [
                "cpu_mhz=132",
                "clock_hz=132000000",
                "cpu_mhz=132\nclock_hz=132000000",
            ] {
                let spec =
                    parse_machine_profile(&format!("version={version}\n[machine]\n{setting}"))
                        .unwrap();
                assert_eq!(spec.hardware.cpu_mhz, 132);
            }
            for setting in [
                "cpu_mhz=0",
                "cpu_mhz=1001",
                "cpu_mhz=33.5",
                "cpu_mhz='132'",
                "clock_hz=33500000",
                "cpu_mhz=132\nclock_hz=33000000",
            ] {
                assert!(
                    parse_machine_profile(&format!("version={version}\n[machine]\n{setting}"))
                        .is_err(),
                    "{setting}"
                );
            }
        }
    }
    #[test]
    fn info_lists_accept_one_label_or_an_array() {
        let spec = parse_machine_profile(
            "version=2\n[info]\nreleased=1994\ngenres='Action'\npublishers=['A','B']",
        )
        .unwrap();
        assert_eq!(spec.info.released, Some(1994));
        assert_eq!(spec.info.genres, ["Action"]);
        assert_eq!(spec.info.publishers, ["A", "B"]);
        assert!(spec.info.cover.is_none() && spec.info.gameplay.is_empty());
    }
    #[test]
    fn reject_unknown_and_unsupported() {
        for text in [
            "version=2\n[machine]\ncpu='pentium'",
            "version=2\n[input]\nkeyboard='usb'",
            "version=2\n[[bindings]]\nid='saves'\nkind='sqlite'\nlocation='saves'",
            "version=2\n[game]\ncover='a.jpg'",
            "version=2\n[info]\ncover=''",
            "version=2\n[info]\ncover=['a.jpg']",
            "version=2\n[info]\nreleased=1800",
            "version=2\n[info]\nreleased='1994'",
            "version=2\n[info]\ngenres=[]",
            "version=2\n[info]\ngenres=['']",
            "version=2\n[info]\ngenre='Action'",
            "version=2\n[info]\ndescription='  '",
            "version=2\n[info]\ndescription=['text']",
        ] {
            assert!(parse_machine_profile(text).is_err());
        }
    }
}
