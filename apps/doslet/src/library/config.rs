//! Game configuration window state. It edits the profile's TOML in place, so
//! comments survive and a setting left at Default stays out of the file, where
//! the launch fills it from the global settings or built-in defaults.
use super::{Game, machine_profile, resolve_game_store_dir};
use crate::global_settings::GlobalSettings;
use doslet_config::{MachineSpec, SourceKind, WritePolicy};
use std::path::{Path, PathBuf};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

/// Sections of the game configuration window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConfigPage {
    #[default]
    General,
    Machine,
    Sound,
    Input,
    Display,
    Drives,
}
impl ConfigPage {
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Machine,
        Self::Sound,
        Self::Input,
        Self::Display,
        Self::Drives,
    ];
}

/// A profile key a game may leave to the global settings or built-in defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Preset,
    ExecutionMode,
    CpuMhz,
    CpuMode,
    Memory,
    X87,
    Dpmi,
    Sb16,
    SoundCard,
    SbIrq,
    SbDma8,
    SbDma16,
    Mpu401,
    Audio,
    GameMidi,
    HostMidi,
    MidiOutput,
    VideoFilter,
    VideoFilterFallback,
    FrameLimit,
    Keyboard,
    Gameport,
    MouseMode,
    HideCursor,
    PadIndex,
    PadId,
    Deadzone,
    AxisX,
    AxisY,
    Button1,
    Button2,
}
impl Setting {
    pub const ALL: [Self; 31] = [
        Self::Preset,
        Self::ExecutionMode,
        Self::CpuMhz,
        Self::CpuMode,
        Self::Memory,
        Self::X87,
        Self::Dpmi,
        Self::Sb16,
        Self::SoundCard,
        Self::SbIrq,
        Self::SbDma8,
        Self::SbDma16,
        Self::Mpu401,
        Self::Audio,
        Self::GameMidi,
        Self::HostMidi,
        Self::MidiOutput,
        Self::VideoFilter,
        Self::VideoFilterFallback,
        Self::FrameLimit,
        Self::Keyboard,
        Self::Gameport,
        Self::MouseMode,
        Self::HideCursor,
        Self::PadIndex,
        Self::PadId,
        Self::Deadzone,
        Self::AxisX,
        Self::AxisY,
        Self::Button1,
        Self::Button2,
    ];
    /// Settings a hardware preset supplies.
    const PRESET: [Self; 7] = [
        Self::CpuMhz,
        Self::CpuMode,
        Self::Memory,
        Self::X87,
        Self::Sb16,
        Self::SoundCard,
        Self::Mpu401,
    ];
    /// Table (empty for the top level) and key.
    pub const fn key(self) -> (&'static str, &'static str) {
        match self {
            Self::Preset => ("", "preset"),
            Self::ExecutionMode => ("game", "mode"),
            Self::CpuMhz => ("machine", "cpu_mhz"),
            Self::CpuMode => ("machine", "cpu_mode"),
            Self::Memory => ("machine", "memory_mib"),
            Self::X87 => ("machine", "x87"),
            Self::Dpmi => ("machine", "dpmi"),
            Self::Sb16 => ("sound", "sb16"),
            Self::SoundCard => ("sound", "card"),
            Self::SbIrq => ("sound", "irq"),
            Self::SbDma8 => ("sound", "dma8"),
            Self::SbDma16 => ("sound", "dma16"),
            Self::Mpu401 => ("sound", "mpu401"),
            Self::Audio => ("output", "audio"),
            Self::GameMidi => ("output", "game_midi"),
            Self::HostMidi => ("output", "host_midi"),
            Self::MidiOutput => ("output", "midi_output"),
            Self::VideoFilter => ("output", "video_filter"),
            Self::VideoFilterFallback => ("output", "video_filter_fallback"),
            Self::FrameLimit => ("output", "frame_limit"),
            Self::Keyboard => ("input", "keyboard"),
            Self::Gameport => ("input", "gameport"),
            Self::MouseMode => ("input", "mouse_mode"),
            Self::HideCursor => ("input", "hide_cursor"),
            Self::PadIndex => ("input", "pad_index"),
            Self::PadId => ("input", "pad_id"),
            Self::Deadzone => ("input", "deadzone_percent"),
            Self::AxisX => ("input", "axis_x"),
            Self::AxisY => ("input", "axis_y"),
            Self::Button1 => ("input", "button_1"),
            Self::Button2 => ("input", "button_2"),
        }
    }
    /// An older spelling of the same setting.
    const fn legacy(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::CpuMhz => Some(("machine", "clock_hz")),
            Self::MouseMode => Some(("input", "capture")),
            Self::SoundCard => Some(("sound", "sb16")),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum GameEdit {
    Open,
    Cancel,
    Save,
    Page(ConfigPage),
    Set(Setting, Value),
    Unset(Setting),
    Name(String),
    Executable(String),
    Arguments(String),
    WorkingDirectory(String),
    StartupKeys(String),
    ActionName(usize, String),
    ActionExecutable(usize, String),
    ActionArguments(usize, String),
    AddAction,
    RemoveAction(usize),
    /// True names C: in the profile; false mounts the executable's folder.
    DriveC(bool),
    DriveCSource(String),
    DriveCPolicy(WritePolicy),
    DriveCStore(String),
    DriveDSource(String),
}

/// Drive C: as the launch will mount it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveC {
    /// Named by `[[drives]]`; otherwise the executable's folder.
    pub explicit: bool,
    pub kind: SourceKind,
    pub path: PathBuf,
    pub exists: bool,
    pub policy: WritePolicy,
    /// Where persistent changes are kept.
    pub store: Option<PathBuf>,
}

/// One `[[game.actions]]` entry as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionText {
    pub name: String,
    pub executable: String,
    pub arguments: String,
}

pub struct ConfigWindow {
    pub page: ConfigPage,
    profile: PathBuf,
    document: DocumentMut,
    original: String,
    /// The machine the launch builds from this profile and the global settings.
    pub effective: MachineSpec,
    /// The same machine with every [`Setting`] except the preset left out.
    pub defaults: MachineSpec,
    pub name: String,
    pub executable: String,
    pub arguments: String,
    pub working_directory: String,
    pub startup_keys: String,
    pub actions: Vec<ActionText>,
    pub drive_c: Option<DriveC>,
    pub drive_d: Option<(PathBuf, bool)>,
    /// Programs on C:, spelled as `game.executable` values.
    pub programs: Vec<String>,
    programs_from: Option<(SourceKind, PathBuf)>,
    /// A version-1 profile is rewritten as version 2 on save.
    pub converted: bool,
    pub problem: Option<String>,
}

impl ConfigWindow {
    pub fn open(game: &Game, global: &GlobalSettings) -> Result<Self, String> {
        let text = std::fs::read_to_string(&game.profile)
            .map_err(|e| format!("Cannot read {}: {e}", game.profile.display()))?;
        let mut document: DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
        let converted = document.get("version").and_then(Item::as_integer) != Some(2);
        if converted {
            document = machine_profile::export_machine_profile(&game.spec)
                .parse()
                .map_err(|e| format!("{e}"))?;
        }
        let str_at = |table: &str, key: &str| {
            document
                .get(table)
                .and_then(|t| t.get(key))
                .and_then(Item::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let mut window = Self {
            page: ConfigPage::General,
            profile: game.profile.clone(),
            name: document
                .get("name")
                .and_then(Item::as_str)
                .unwrap_or_default()
                .into(),
            executable: str_at("game", "executable"),
            arguments: game.spec.game.arguments.join(" "),
            working_directory: str_at("game", "working_directory"),
            startup_keys: game.spec.game.startup_keys.clone(),
            actions: game
                .spec
                .game
                .actions
                .iter()
                .map(|action| ActionText {
                    name: action.name.clone(),
                    executable: action.executable.clone(),
                    arguments: action.arguments.join(" "),
                })
                .collect(),
            original: if converted {
                String::new()
            } else {
                document.to_string()
            },
            document,
            effective: game.spec.clone(),
            defaults: game.spec.clone(),
            drive_c: None,
            drive_d: None,
            programs: Vec::new(),
            programs_from: None,
            converted,
            problem: None,
        };
        window.refresh(global);
        Ok(window)
    }

    pub fn profile(&self) -> &Path {
        &self.profile
    }
    pub fn changed(&self) -> bool {
        self.document.to_string() != self.original
    }
    pub fn text(&self) -> String {
        self.document.to_string()
    }
    /// The value the profile sets, or `None` when it follows the default.
    pub fn get(&self, setting: Setting) -> Option<&Value> {
        let (table, key) = setting.key();
        let item = if table.is_empty() {
            self.document.get(key)
        } else {
            self.document.get(table)?.get(key)
        };
        item?.as_value()
    }
    pub fn is_set(&self, setting: Setting) -> bool {
        let legacy = setting.legacy().is_some_and(|(table, key)| {
            self.document.get(table).and_then(|t| t.get(key)).is_some()
        });
        self.get(setting).is_some() || legacy
    }

    pub fn edit(&mut self, edit: GameEdit, global: &GlobalSettings) {
        match edit {
            GameEdit::Open | GameEdit::Cancel | GameEdit::Save => return,
            GameEdit::Page(page) => {
                self.page = page;
                return;
            }
            GameEdit::Set(Setting::Preset, value) => {
                self.unset_preset_fields();
                self.set(Setting::Preset, value);
            }
            GameEdit::Unset(Setting::Preset) => {
                self.unset_preset_fields();
                self.unset(Setting::Preset);
            }
            GameEdit::Set(setting, value) => self.set(setting, value),
            GameEdit::Unset(setting) => self.unset(setting),
            GameEdit::Name(v) => {
                assign(&mut self.document["name"], Value::from(v.as_str()));
                self.name = v;
            }
            GameEdit::Executable(v) => {
                self.put(
                    "game",
                    "executable",
                    (!v.is_empty()).then(|| v.as_str().into()),
                );
                self.executable = v;
            }
            GameEdit::Arguments(v) => {
                self.put("game", "arguments", words(&v));
                self.arguments = v;
            }
            GameEdit::WorkingDirectory(v) => {
                self.put(
                    "game",
                    "working_directory",
                    (!v.is_empty()).then(|| v.as_str().into()),
                );
                self.working_directory = v;
            }
            GameEdit::StartupKeys(v) => {
                self.put(
                    "game",
                    "startup_keys",
                    (!v.is_empty()).then(|| v.as_str().into()),
                );
                self.startup_keys = v;
            }
            GameEdit::ActionName(index, v) => {
                if let Some(action) = self.action_mut(index) {
                    assign(&mut action["name"], Value::from(v.as_str()));
                }
                if let Some(text) = self.actions.get_mut(index) {
                    text.name = v;
                }
            }
            GameEdit::ActionExecutable(index, v) => {
                if let Some(action) = self.action_mut(index) {
                    assign(&mut action["executable"], Value::from(v.as_str()));
                }
                if let Some(text) = self.actions.get_mut(index) {
                    text.executable = v;
                }
            }
            GameEdit::ActionArguments(index, v) => {
                if let Some(action) = self.action_mut(index) {
                    match words(&v) {
                        Some(list) => action["arguments"] = Item::Value(list),
                        None => {
                            action.remove("arguments");
                        }
                    }
                }
                if let Some(text) = self.actions.get_mut(index) {
                    text.arguments = v;
                }
            }
            GameEdit::AddAction => {
                let number = self.actions.len() + 1;
                let mut action = Table::new();
                assign(&mut action["id"], Value::from(format!("program-{number}")));
                assign(
                    &mut action["name"],
                    Value::from(format!("Program {number}")),
                );
                assign(&mut action["executable"], Value::from(""));
                self.actions_mut().push(action);
                self.actions.push(ActionText {
                    name: format!("Program {number}"),
                    ..ActionText::default()
                });
            }
            GameEdit::RemoveAction(index) => {
                if index < self.actions.len() {
                    self.actions_mut().remove(index);
                    self.actions.remove(index);
                    if self.actions.is_empty() {
                        self.section("game").remove("actions");
                    }
                }
            }
            GameEdit::DriveC(true) => {
                if self.drive_index(b'C').is_none() {
                    let source = self.drive_c.as_ref().map_or_else(String::new, |c| {
                        let folder = Path::new(&self.executable)
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if folder.is_empty() {
                            c.path.to_string_lossy().into_owned()
                        } else {
                            folder
                        }
                    });
                    let mut drive = Table::new();
                    assign(&mut drive["letter"], Value::from("C"));
                    assign(&mut drive["kind"], Value::from("directory"));
                    assign(&mut drive["source"], Value::from(source));
                    assign(&mut drive["policy"], Value::from("temporary"));
                    self.drives_mut().push(drive);
                }
            }
            GameEdit::DriveC(false) => self.remove_drive(b'C'),
            GameEdit::DriveCSource(v) => {
                let zip = v.to_ascii_lowercase().ends_with(".zip");
                self.drive_source(b'C', &v, if zip { "zip" } else { "directory" });
            }
            GameEdit::DriveCPolicy(policy) => {
                if let Some(drive) = self.drive_mut(b'C') {
                    assign(
                        &mut drive["policy"],
                        Value::from(match policy {
                            WritePolicy::ReadOnly => "read-only",
                            WritePolicy::Temporary => "temporary",
                            WritePolicy::Persistent => "persistent",
                            WritePolicy::LegacyWriteback => "legacy-rw",
                        }),
                    );
                }
            }
            GameEdit::DriveCStore(v) => {
                let store = self
                    .drive_mut(b'C')
                    .and_then(|drive| drive.get("store")?.as_str().map(str::to_owned));
                if let Some(binding) = store.and_then(|id| self.binding_mut(&id))
                    && !v.is_empty()
                {
                    assign(&mut binding["location"], Value::from(v));
                } else if let Some(drive) = self.drive_mut(b'C') {
                    if v.is_empty() {
                        drive.remove("store");
                    } else {
                        assign(&mut drive["store"], Value::from(v));
                    }
                }
            }
            GameEdit::DriveDSource(v) => {
                if v.trim().is_empty() {
                    self.remove_drive(b'D');
                } else {
                    self.drive_source(b'D', &v, "iso");
                }
            }
        }
        self.refresh(global);
    }

    /// Text of a `[[drives]]` field, through its host binding when it names one.
    pub fn drive_text(&self, letter: u8, key: &str) -> String {
        let Some(index) = self.drive_index(letter) else {
            return String::new();
        };
        let value = self
            .drive(index)
            .and_then(|drive| drive.get(key))
            .and_then(Item::as_str)
            .unwrap_or_default();
        self.bindings()
            .and_then(|bindings| {
                bindings
                    .iter()
                    .find(|b| b.get("id").and_then(Item::as_str) == Some(value))
            })
            .and_then(|b| b.get("location").and_then(Item::as_str))
            .unwrap_or(value)
            .to_owned()
    }

    fn set(&mut self, setting: Setting, value: Value) {
        if let Some((table, key)) = setting.legacy() {
            self.remove_key(table, key);
        }
        let (table, key) = setting.key();
        if table.is_empty() {
            assign(&mut self.document[key], value);
        } else {
            assign(&mut self.section(table)[key], value);
        }
    }
    fn unset(&mut self, setting: Setting) {
        if let Some((table, key)) = setting.legacy() {
            self.remove_key(table, key);
        }
        let (table, key) = setting.key();
        self.remove_key(table, key);
    }
    fn unset_preset_fields(&mut self) {
        for setting in Setting::PRESET {
            self.unset(setting);
        }
    }
    fn remove_key(&mut self, table: &str, key: &str) {
        if table.is_empty() {
            self.document.remove(key);
            return;
        }
        let Some(section) = self
            .document
            .get_mut(table)
            .and_then(Item::as_table_like_mut)
        else {
            return;
        };
        section.remove(key);
        if section.is_empty() {
            self.document.remove(table);
        }
    }
    /// Sets or, for `None`, removes a key of a section.
    fn put(&mut self, table: &str, key: &str, value: Option<Value>) {
        match value {
            Some(value) => assign(&mut self.section(table)[key], value),
            None => self.remove_key(table, key),
        }
    }
    /// A section as a standard table, converting an inline one.
    fn section(&mut self, name: &str) -> &mut Table {
        let item = self
            .document
            .entry(name)
            .or_insert_with(|| Item::Table(Table::new()));
        if let Item::Value(Value::InlineTable(inline)) = item {
            *item = Item::Table(std::mem::take(inline).into_table());
        }
        if !item.is_table() {
            *item = Item::Table(Table::new());
        }
        item.as_table_mut().expect("a table was just inserted")
    }
    fn actions_mut(&mut self) -> &mut ArrayOfTables {
        let item = self
            .section("game")
            .entry("actions")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
        if !item.is_array_of_tables() {
            *item = Item::ArrayOfTables(ArrayOfTables::new());
        }
        item.as_array_of_tables_mut()
            .expect("an array was just inserted")
    }
    fn action_mut(&mut self, index: usize) -> Option<&mut Table> {
        self.document
            .get_mut("game")?
            .get_mut("actions")?
            .as_array_of_tables_mut()?
            .get_mut(index)
    }
    fn drives(&self) -> impl Iterator<Item = &Table> {
        self.document
            .get("drives")
            .and_then(Item::as_array_of_tables)
            .into_iter()
            .flat_map(ArrayOfTables::iter)
    }
    fn drive(&self, index: usize) -> Option<&Table> {
        self.drives().nth(index)
    }
    fn drives_mut(&mut self) -> &mut ArrayOfTables {
        let item = self
            .document
            .entry("drives")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
        if !item.is_array_of_tables() {
            *item = Item::ArrayOfTables(ArrayOfTables::new());
        }
        item.as_array_of_tables_mut()
            .expect("an array was just inserted")
    }
    fn drive_index(&self, letter: u8) -> Option<usize> {
        self.drives().position(|drive| {
            drive
                .get("letter")
                .and_then(Item::as_str)
                .is_some_and(|l| l.as_bytes().first().map(u8::to_ascii_uppercase) == Some(letter))
        })
    }
    fn drive_mut(&mut self, letter: u8) -> Option<&mut Table> {
        let index = self.drive_index(letter)?;
        self.drives_mut().get_mut(index)
    }
    fn remove_drive(&mut self, letter: u8) {
        if let Some(index) = self.drive_index(letter) {
            self.drives_mut().remove(index);
            if self.drives().next().is_none() {
                self.document.remove("drives");
            }
        }
    }
    fn bindings(&self) -> Option<&ArrayOfTables> {
        self.document.get("bindings")?.as_array_of_tables()
    }
    fn binding_mut(&mut self, id: &str) -> Option<&mut Table> {
        self.document
            .get_mut("bindings")?
            .as_array_of_tables_mut()?
            .iter_mut()
            .find(|b| b.get("id").and_then(Item::as_str) == Some(id))
    }
    /// Points a drive at `source`, through its host binding when it names one.
    fn drive_source(&mut self, letter: u8, source: &str, kind: &str) {
        let Some(index) = self.drive_index(letter) else {
            let mut drive = Table::new();
            assign(
                &mut drive["letter"],
                Value::from(char::from(letter).to_string()),
            );
            assign(&mut drive["kind"], Value::from(kind));
            assign(&mut drive["source"], Value::from(source));
            assign(
                &mut drive["policy"],
                Value::from(if kind == "iso" {
                    "read-only"
                } else {
                    "temporary"
                }),
            );
            self.drives_mut().push(drive);
            return;
        };
        let id = self
            .drive(index)
            .and_then(|drive| drive.get("source"))
            .and_then(Item::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(binding) = self.binding_mut(&id) {
            assign(&mut binding["location"], Value::from(source));
            assign(&mut binding["kind"], Value::from(kind));
        } else if let Some(drive) = self.drives_mut().get_mut(index) {
            assign(&mut drive["source"], Value::from(source));
        }
        if let Some(drive) = self.drives_mut().get_mut(index) {
            assign(&mut drive["kind"], Value::from(kind));
        }
    }

    /// Recomputes the effective machine, its defaults and the drives, and
    /// validates the profile as the launch will read it.
    fn refresh(&mut self, global: &GlobalSettings) {
        let resolve = |document: &DocumentMut| {
            let mut spec = machine_profile::parse_machine_profile(&document.to_string())?;
            global.fill_profile_defaults(&mut spec, document);
            spec.resolve().map_err(|e| e.to_string())?;
            Ok::<_, String>(spec)
        };
        let mut bare = self.document.clone();
        for setting in Setting::ALL {
            if setting == Setting::Preset {
                continue;
            }
            for (table, key) in [Some(setting.key()), setting.legacy()]
                .into_iter()
                .flatten()
            {
                if let Some(section) = bare.get_mut(table).and_then(Item::as_table_like_mut) {
                    section.remove(key);
                }
            }
        }
        if let Ok(defaults) = resolve(&bare) {
            self.defaults = defaults;
        }
        self.problem = match resolve(&self.document) {
            Ok(spec) => {
                self.effective = spec;
                working_directory_problem(&self.effective.game.working_directory)
            }
            Err(error) => Some(error),
        };
        if self.problem.is_none() && self.actions.iter().any(|a| a.executable.trim().is_empty()) {
            self.problem = Some("Every other program needs an executable.".into());
        }
        self.refresh_drives();
    }

    fn refresh_drives(&mut self) {
        let spec = &self.effective;
        let base = self.profile.parent().unwrap_or(Path::new("."));
        let stem = self
            .profile
            .file_stem()
            .map_or(String::new(), |s| s.to_string_lossy().into_owned());
        self.drive_c = match spec.mounts.iter().find(|m| m.letter == b'C') {
            Some(mount) => {
                let path = base.join(spec.binding(&mount.source));
                Some(DriveC {
                    explicit: true,
                    kind: mount.kind,
                    exists: path.exists(),
                    policy: mount.policy,
                    store: (mount.policy == WritePolicy::Persistent)
                        .then(|| resolve_game_store_dir(base, &stem, spec)),
                    path,
                })
            }
            None if !spec.game.executable.is_empty() => {
                let executable = base.join(&spec.game.executable);
                executable.parent().map(|folder| DriveC {
                    explicit: false,
                    kind: SourceKind::NativeDirectory,
                    path: folder.to_path_buf(),
                    exists: folder.is_dir(),
                    policy: WritePolicy::Temporary,
                    store: None,
                })
            }
            None => None,
        };
        self.drive_d = spec.mounts.iter().find(|m| m.letter != b'C').map(|mount| {
            let path = base.join(spec.binding(&mount.source));
            let exists = path.is_file();
            (path, exists)
        });
        let source = self.drive_c.as_ref().map(|c| (c.kind, c.path.clone()));
        if source != self.programs_from {
            self.programs = programs(self.drive_c.as_ref(), &self.executable, base);
            self.programs_from = source;
        }
    }
}

/// Replaces a value and keeps the spacing and comments around the old one.
fn assign(slot: &mut Item, mut value: Value) {
    if let Some(old) = slot.as_value() {
        *value.decor_mut() = old.decor().clone();
    }
    *slot = Item::Value(value);
}

/// `game.arguments` from typed text; `None` for no arguments.
fn words(text: &str) -> Option<Value> {
    let list: Array = text.split_whitespace().collect();
    (!list.is_empty()).then_some(Value::Array(list))
}

fn working_directory_problem(directory: &str) -> Option<String> {
    (!directory.is_ascii()
        || directory.bytes().any(|byte| byte < 0x20 || byte == 0x7F)
        || !(directory.starts_with('\\') || directory.to_ascii_uppercase().starts_with("C:\\")))
    .then(|| "Working directory must be an absolute DOS path on C:, such as C:\\GAME".into())
}

/// Programs on drive C:, spelled as `game.executable` values: relative to the
/// profile for a folder and inside the archive for a ZIP.
fn programs(drive: Option<&DriveC>, executable: &str, base: &Path) -> Vec<String> {
    const LIMIT: usize = 300;
    let program = |name: &str| {
        let name = name.to_ascii_uppercase();
        name.ends_with(".EXE") || name.ends_with(".COM") || name.ends_with(".BAT")
    };
    let Some(drive) = drive else {
        return Vec::new();
    };
    let mut list = Vec::new();
    match drive.kind {
        SourceKind::BrowserZip => {
            // A ZIP mounts the executable's folder inside the archive as C:.
            let executable = executable.replace('\\', "/");
            let prefix = executable.rfind('/').map_or("", |i| &executable[..=i]);
            let Ok(file) = std::fs::File::open(&drive.path) else {
                return list;
            };
            let Ok(mut archive) = zip::ZipArchive::new(file) else {
                return list;
            };
            for index in 0..archive.len().min(4096) {
                let Ok(entry) = archive.by_index_raw(index) else {
                    continue;
                };
                let name = entry.name().replace('\\', "/");
                if !entry.is_dir()
                    && name
                        .to_ascii_uppercase()
                        .starts_with(&prefix.to_ascii_uppercase())
                    && program(&name)
                {
                    list.push(name);
                }
            }
        }
        _ => {
            let folder = drive.path.strip_prefix(base).unwrap_or(&drive.path);
            let folder = folder.to_string_lossy().replace('\\', "/");
            let mut stack = vec![(
                drive.path.clone(),
                folder.trim_end_matches('/').to_owned(),
                0,
            )];
            let mut seen = 0;
            while let Some((directory, prefix, depth)) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&directory) else {
                    continue;
                };
                for entry in entries.flatten() {
                    seen += 1;
                    if seen > 4096 || list.len() >= LIMIT {
                        break;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    match entry.file_type() {
                        Ok(kind) if kind.is_dir() && depth < 6 => {
                            stack.push((entry.path(), path, depth + 1));
                        }
                        Ok(kind) if kind.is_file() && program(&name) => list.push(path),
                        _ => {}
                    }
                }
            }
        }
    }
    list.truncate(LIMIT);
    list.sort_by_key(|name| name.to_ascii_uppercase());
    list
}
