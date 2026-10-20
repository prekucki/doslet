//! Game library shown when Doslet starts without a game. Each launch profile
//! becomes a card; the window shell starts the chosen game in its place.
use crate::global_settings::GlobalSettings;
use crate::machine_profile;
use doslet_config::{
    CpuMode, GameAction, GameInfo, HardwarePreset, KeyboardMode, MachineSpec, MouseMode,
    SourceKind, VideoFilter,
};
use iced::widget::image::Handle;
use iced::{Color, Subscription, Task};
use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item};

mod view;

const MAX_PROFILE_BYTES: usize = 1024 * 1024;
const MAX_COVER_BYTES: usize = 16 * 1024 * 1024;
const MAX_GAMES: usize = 512;
/// Library and detail art never need more pixels than this on a 2x display.
const COVER_PIXELS: u32 = 1280;
const BACKDROP_PIXELS: u32 = 72;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GameTab {
    #[default]
    Overview,
    VfsAndState,
    Configuration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VfsViewMode {
    #[default]
    AllFiles,
    ChangesOnly,
}

#[derive(Clone, Debug)]
pub struct AutosaveInfo {
    pub compressed_bytes: u64,
    pub uncompressed_bytes: Option<u32>,
    pub age_label: String,
}

fn inspect_autosave(path: &Path) -> Option<AutosaveInfo> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let compressed_bytes = meta.len();
    let age_label = meta
        .modified()
        .ok()
        .and_then(|mtime| mtime.elapsed().ok())
        .map_or_else(
            || "saved".to_owned(),
            |elapsed| {
                let secs = elapsed.as_secs();
                if secs < 60 {
                    "just now".to_owned()
                } else if secs < 3600 {
                    format!("{} min ago", secs / 60)
                } else if secs < 86400 {
                    format!("{} h ago", secs / 3600)
                } else {
                    format!("{} d ago", secs / 86400)
                }
            },
        );
    let uncompressed_bytes = if compressed_bytes >= 18
        && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gz"))
    {
        use std::io::{Seek, SeekFrom};
        File::open(path).ok().and_then(|mut file| {
            file.seek(SeekFrom::End(-4)).ok()?;
            let mut tail = [0u8; 4];
            file.read_exact(&mut tail).ok()?;
            Some(u32::from_le_bytes(tail))
        })
    } else {
        None
    };
    Some(AutosaveInfo {
        compressed_bytes,
        uncompressed_bytes,
        age_label,
    })
}

fn resolve_game_store_dir(base: &Path, stem: &str, spec: &MachineSpec) -> PathBuf {
    let configured = spec
        .mounts
        .iter()
        .find(|m| m.letter == b'C')
        .and_then(|m| m.store.as_deref())
        .map(|s| spec.binding(s));
    let store_ref = configured.unwrap_or(stem);
    if Path::new(store_ref).components().count() == 1 {
        crate::host_dirs::HostDirs::detect()
            .map(|d| d.saves().join(store_ref))
            .unwrap_or_else(|_| base.join(format!("{store_ref}.saves")))
    } else {
        base.join(store_ref)
    }
}

fn resolve_base_c_source(
    base: &Path,
    spec: &MachineSpec,
    document: &DocumentMut,
) -> Option<(SourceKind, PathBuf)> {
    if let Some(m) = spec.mounts.iter().find(|m| m.letter == b'C') {
        return Some((m.kind, base.join(spec.binding(&m.source))));
    }
    if let Some(drives) = document.get("drives").and_then(Item::as_array_of_tables) {
        for drive in drives.iter() {
            let letter = drive.get("letter").and_then(Item::as_str).unwrap_or("");
            let kind = drive.get("kind").and_then(Item::as_str).unwrap_or("");
            if letter.eq_ignore_ascii_case("C")
                && kind == "directory"
                && let Some(source) = drive.get("source").and_then(Item::as_str)
            {
                return Some((SourceKind::NativeDirectory, base.join(source)));
            }
        }
    }
    if !spec.game.executable.is_empty() {
        let exe = base.join(&spec.game.executable);
        if let Some(parent) = exe.parent() {
            return Some((SourceKind::NativeDirectory, parent.to_path_buf()));
        }
    }
    None
}

fn scan_base_vfs(
    source: Option<&(SourceKind, PathBuf)>,
    executable: &str,
) -> std::collections::BTreeMap<String, u32> {
    let mut map = std::collections::BTreeMap::new();
    let Some((kind, root_path)) = source else {
        return map;
    };
    match kind {
        SourceKind::NativeDirectory => {
            let mut stack = vec![(root_path.clone(), String::new(), 0usize)];
            while let Some((dir, prefix, depth)) = stack.pop() {
                if depth > 6 || map.len() >= 4096 {
                    break;
                }
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if map.len() >= 4096 {
                        break;
                    }
                    let name = entry.file_name();
                    let Some(name_str) = name.to_str() else {
                        continue;
                    };
                    if name_str.starts_with('.') || !name_str.is_ascii() {
                        continue;
                    }
                    let dos_name = if prefix.is_empty() {
                        name_str.to_ascii_uppercase()
                    } else {
                        format!("{prefix}\\{}", name_str.to_ascii_uppercase())
                    };
                    let Ok(meta) = entry.metadata() else {
                        continue;
                    };
                    if meta.is_dir() {
                        stack.push((entry.path(), dos_name, depth + 1));
                    } else if meta.is_file() {
                        let size = meta.len().min(u64::from(u32::MAX)) as u32;
                        map.insert(dos_name, size);
                    }
                }
            }
        }
        SourceKind::BrowserZip => {
            let Ok(file) = File::open(root_path) else {
                return map;
            };
            let Ok(mut zip) = zip::ZipArchive::new(file) else {
                return map;
            };
            let exe_norm = executable.replace('\\', "/").to_ascii_uppercase();
            let prefix = exe_norm
                .rfind('/')
                .map_or("", |idx| &exe_norm[..=idx])
                .to_owned();
            for i in 0..zip.len().min(4096) {
                let Ok(entry) = zip.by_index_raw(i) else {
                    continue;
                };
                if entry.is_dir() || !entry.name().is_ascii() {
                    continue;
                }
                let norm = entry.name().replace('\\', "/").to_ascii_uppercase();
                let Some(rel) = norm.strip_prefix(&prefix) else {
                    continue;
                };
                if rel.is_empty() {
                    continue;
                }
                let dos_path = rel.replace('/', "\\");
                let size = entry.size().min(u64::from(u32::MAX)) as u32;
                map.insert(dos_path, size);
            }
        }
        _ => {}
    }
    map
}

fn read_base_preview(
    source: Option<&(SourceKind, PathBuf)>,
    executable: &str,
    dos_path: &str,
) -> Option<String> {
    let (kind, root_path) = source?;
    match kind {
        SourceKind::NativeDirectory => {
            let mut current = root_path.clone();
            for part in dos_path.split('\\').filter(|p| !p.is_empty()) {
                let entries = std::fs::read_dir(&current).ok()?;
                let mut found = None;
                for entry in entries.flatten() {
                    if entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(part))
                    {
                        found = Some(entry.path());
                        break;
                    }
                }
                current = found?;
            }
            let file = File::open(current).ok()?;
            let mut bytes = Vec::new();
            file.take(256).read_to_end(&mut bytes).ok()?;
            Some(crate::native_store_format::format_preview(0, &bytes))
        }
        SourceKind::BrowserZip => {
            let file = File::open(root_path).ok()?;
            let mut zip = zip::ZipArchive::new(file).ok()?;
            let exe_norm = executable.replace('\\', "/").to_ascii_uppercase();
            let prefix = exe_norm.rfind('/').map_or("", |idx| &exe_norm[..=idx]);
            let wanted = format!("{prefix}{}", dos_path.replace('\\', "/").to_ascii_uppercase());
            for i in 0..zip.len() {
                let entry = zip.by_index(i).ok()?;
                if entry.is_dir() {
                    continue;
                }
                let norm = entry.name().replace('\\', "/").to_ascii_uppercase();
                if norm == wanted {
                    let mut bytes = Vec::new();
                    entry.take(256).read_to_end(&mut bytes).ok()?;
                    return Some(crate::native_store_format::format_preview(0, &bytes));
                }
            }
            None
        }
        _ => None,
    }
}

pub struct Game {
    pub profile: PathBuf,
    pub name: String,
    pub info: GameInfo,
    pub program: String,
    pub drive: Option<String>,
    cover: Option<PathBuf>,
    pub art: Option<Artwork>,
    pub actions: Vec<GameAction>,
    pub spec: MachineSpec,
    pub autosave: PathBuf,
    pub has_autosave: bool,
    pub autosave_info: Option<AutosaveInfo>,
    pub store_dir: PathBuf,
    pub base_c_source: Option<(SourceKind, PathBuf)>,
    pub store_summary: Option<crate::native_store_format::StoreSummary>,
}
impl Game {
    fn load(path: &Path) -> Result<Self, String> {
        let text = String::from_utf8(read(path, MAX_PROFILE_BYTES)?)
            .map_err(|_| "profile is not UTF-8".to_string())?;
        let base = path.parent().unwrap_or(Path::new("."));
        let stem = path
            .file_stem()
            .map_or(String::new(), |s| s.to_string_lossy().into_owned());
        let document: DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
        let autosave = crate::host_dirs::HostDirs::detect()
            .map(|d| d.autosave_file(&stem))
            .unwrap_or_else(|_| base.join(format!("{stem}.autosave.state.gz")));
        let autosave_info = inspect_autosave(&autosave);
        let has_autosave = autosave_info.is_some();
        let mut game = Self {
            profile: std::path::absolute(path).unwrap_or_else(|_| path.to_owned()),
            name: stem.clone(),
            info: GameInfo::default(),
            program: String::new(),
            drive: None,
            cover: None,
            art: None,
            actions: Vec::new(),
            spec: MachineSpec::default(),
            autosave,
            has_autosave,
            autosave_info,
            store_dir: PathBuf::new(),
            base_c_source: None,
            store_summary: None,
        };
        if document.get("version").and_then(Item::as_integer) == Some(2) {
            let spec = machine_profile::parse_machine_profile(&text)?;
            if !spec.name.is_empty() {
                game.name = spec.name.clone();
            }
            game.drive = spec.mounts.iter().find(|m| m.letter == b'C').map(|m| {
                let kind = match m.kind {
                    SourceKind::BrowserZip => "ZIP archive",
                    SourceKind::NativeDirectory => "Folder",
                    _ => "Disc image",
                };
                let source = spec.binding(&m.source);
                let file = Path::new(source)
                    .file_name()
                    .map_or(source.into(), |f| f.to_string_lossy());
                format!("{kind} · {file}")
            });
            game.cover = spec.info.cover.as_ref().map(|cover| base.join(cover));
            game.program = spec.game.executable.clone();
            game.info = spec.info.clone();
            game.actions = spec.game.actions.clone();
            game.spec = spec;
        } else {
            // Version-1 profiles have no [info]; show their name and program.
            if let Some(name) = document.get("name").and_then(Item::as_str) {
                game.name = name.into();
            }
            game.program = document
                .get("game")
                .and_then(|g| g.get("executable"))
                .and_then(Item::as_str)
                .unwrap_or_default()
                .into();
            if let Ok(spec) = machine_profile::parse_machine_profile(&text) {
                game.spec = spec;
            }
            if game.spec.game.executable.is_empty() {
                game.spec.game.executable = game.program.clone();
            }
        }
        game.store_dir = resolve_game_store_dir(base, &stem, &game.spec);
        game.base_c_source = resolve_base_c_source(base, &game.spec, &document);
        game.program = game
            .program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        game.refresh_state();
        Ok(game)
    }
    pub fn refresh_state(&mut self) {
        use crate::native_store_format::{OverlayFileEntry, OverlayFileKind, StoreSummary};
        self.autosave_info = inspect_autosave(&self.autosave);
        self.has_autosave = self.autosave_info.is_some();
        let base_files = scan_base_vfs(self.base_c_source.as_ref(), &self.spec.game.executable);
        let store = crate::native_store_format::inspect_store(&self.store_dir)
            .ok()
            .flatten();
        if let Some(mut summary) = store {
            for entry in &mut summary.files {
                if entry.size.is_none() {
                    let key = entry.base_path.as_deref().unwrap_or(&entry.path);
                    entry.size = base_files.get(key).copied();
                }
            }
            for (dos_path, size) in &base_files {
                let already_present = summary.files.iter().any(|e| {
                    e.path.eq_ignore_ascii_case(dos_path)
                        || e.base_path
                            .as_deref()
                            .is_some_and(|b| b.eq_ignore_ascii_case(dos_path))
                });
                if !already_present {
                    summary.files.push(OverlayFileEntry {
                        path: dos_path.clone(),
                        base_path: Some(dos_path.clone()),
                        kind: OverlayFileKind::Unchanged,
                        size: Some(*size),
                        preview: None,
                    });
                }
            }
            summary.files.sort_by(|a, b| a.path.cmp(&b.path));
            summary.changed_count = summary.files.iter().filter(|f| f.kind.is_changed()).count();
            self.store_summary = Some(summary);
        } else if !base_files.is_empty() {
            let files = base_files
                .into_iter()
                .map(|(path, size)| OverlayFileEntry {
                    base_path: Some(path.clone()),
                    path,
                    kind: OverlayFileKind::Unchanged,
                    size: Some(size),
                    preview: None,
                })
                .collect();
            self.store_summary = Some(StoreSummary {
                revision: 0,
                used_bytes: 0,
                changed_count: 0,
                files,
            });
        } else {
            self.store_summary = None;
        }
    }
    /// Year and first genre, for compact card captions.
    pub fn caption(&self) -> String {
        let mut parts = Vec::new();
        if let Some(year) = self.info.released {
            parts.push(year.to_string());
        }
        parts.extend(self.info.genres.first().cloned());
        if parts.is_empty() {
            parts.push(self.program.clone());
        }
        parts.join(" · ")
    }
    fn matches(&self, query: &str, filter: &Filter) -> bool {
        let query = query.trim().to_lowercase();
        let text = query.is_empty()
            || self.name.to_lowercase().contains(&query)
            || self.program.to_lowercase().contains(&query)
            || [
                &self.info.developers,
                &self.info.publishers,
                &self.info.genres,
            ]
            .into_iter()
            .flatten()
            .any(|label| label.to_lowercase().contains(&query));
        text && filter.admits(self)
    }
}

/// The sidebar selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Genre(String),
    Settings,
}
impl Filter {
    pub fn admits(&self, game: &Game) -> bool {
        match self {
            Self::All | Self::Settings => true,
            Self::Genre(genre) => game.info.genres.iter().any(|g| g == genre),
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum GlobalEdit {
    AudioEnabled(bool),
    AudioVolume(u8),
    AudioMixer(String),
    MidiOutput(String),
    RefreshMidiPorts,
    GameMidi(String),
    HostMidi(String),
    Mt32RomDir(String),
    Keyboard(KeyboardMode),
    Gameport(bool),
    MouseMode(MouseMode),
    HideCursor(bool),
    PadIndex(u8),
    Deadzone(u8),
    VideoFilter(VideoFilter),
    Scaling(String),
    Aspect(String),
    DefaultPreset(String),
}

#[derive(Clone, Debug)]
pub enum GameEdit {
    Toggle,
    Preset(String),
    CpuMhz(u16),
    CpuMode(CpuMode),
    MemoryMib(u8),
    Sb16(bool),
    Mpu401(bool),
    GameMidi(String),
    Gameport(bool),
    Executable(String),
    NewActionName(String),
    NewActionExe(String),
    AddAction,
    RemoveAction(usize),
    Save,
}

/// Decoded art: the cover for cards and pages, and a tiny blurred copy that
/// the game page stretches into its backdrop.
#[derive(Clone, Debug)]
pub struct Artwork {
    pub cover: Handle,
    pub backdrop: Handle,
    pub tint: Color,
}
fn artwork(path: &Path) -> Result<Artwork, String> {
    use image::imageops::{self, FilterType};
    let bytes = read(path, MAX_COVER_BYTES)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| e.to_string())?;
    let cover = if decoded.width().max(decoded.height()) > COVER_PIXELS {
        decoded.resize(COVER_PIXELS, COVER_PIXELS, FilterType::Triangle)
    } else {
        decoded
    }
    .to_rgba8();
    let small = imageops::thumbnail(&cover, BACKDROP_PIXELS, BACKDROP_PIXELS * 9 / 16);
    let backdrop = imageops::blur(&small, 2.5);
    let pixels = small.pixels().count().max(1) as f32;
    let [r, g, b] = small.pixels().fold([0.0f32; 3], |sum, p| {
        [
            sum[0] + f32::from(p[0]),
            sum[1] + f32::from(p[1]),
            sum[2] + f32::from(p[2]),
        ]
    });
    let tint = Color::from_rgb(r / pixels / 255.0, g / pixels / 255.0, b / pixels / 255.0);
    let handle = |image: image::RgbaImage| {
        let (width, height) = image.dimensions();
        Handle::from_rgba(width, height, image.into_raw())
    };
    Ok(Artwork {
        cover: handle(cover),
        backdrop: handle(backdrop),
        tint,
    })
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err(format!("larger than {} MiB", limit / (1024 * 1024)));
    }
    Ok(bytes)
}

/// The user's profile directory only: a `games` directory beside the working
/// directory may hold unrelated TOML files, such as a `mise.toml`.
pub fn directories() -> Vec<PathBuf> {
    crate::host_dirs::HostDirs::detect()
        .map(|dirs| vec![dirs.profiles()])
        .unwrap_or_default()
}

fn scan(directories: &[PathBuf]) -> Vec<Game> {
    let mut seen = BTreeSet::new();
    let mut games = Vec::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
                    && path.is_file()
            })
            .collect();
        paths.sort();
        for path in paths {
            if games.len() == MAX_GAMES
                || !seen.insert(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()))
            {
                continue;
            }
            match Game::load(&path) {
                Ok(game) => games.push(game),
                Err(error) => {
                    eprintln!(
                        "[WARN doslet] Library: skipping {}: {error}",
                        path.display()
                    )
                }
            }
        }
    }
    games.sort_by_cached_key(|game| game.name.to_lowercase());
    games
}

fn discover_midi_choices() -> Vec<String> {
    let mut choices = vec!["rust".to_string(), "moont".to_string()];
    #[cfg(target_os = "macos")]
    choices.push("apple".to_string());
    if let Ok(ports) = crate::native_midi::ports() {
        for name in ports.names {
            if !choices.contains(&name) {
                choices.push(name);
            }
        }
    }
    choices
}

#[derive(Clone, Debug)]
pub enum Message {
    Search(String),
    Filter(Filter),
    Open(usize),
    Back,
    Launch(usize),
    Restart(usize),
    LaunchAction(usize, String),
    LaunchDemo,
    SelectTab(GameTab),
    SetVfsMode(VfsViewMode),
    NavigateVfsDir(String),
    SelectSaveFile(Option<usize>),
    DiscardAutosave(usize),
    RequestResetOverlay(bool),
    ConfirmResetOverlay(usize),
    Global(GlobalEdit),
    GameConfig(GameEdit),
    Art(PathBuf, Option<Artwork>),
    Rescan,
    Dismiss,
    Key(iced::keyboard::Event),
}

pub struct Library {
    directories: Vec<PathBuf>,
    pub games: Vec<Game>,
    pub search: String,
    pub filter: Filter,
    pub selected: Option<usize>,
    pub notice: Option<String>,
    /// The game whose machine is being prepared for this window.
    pub starting: Option<usize>,
    pub global: GlobalSettings,
    pub midi_choices: Vec<String>,
    pub game_tab: GameTab,
    pub vfs_mode: VfsViewMode,
    pub vfs_dir: String,
    pub confirm_reset_overlay: bool,
    pub selected_save_file: Option<usize>,
    pub new_action_name: String,
    pub new_action_exe: String,
}
impl Library {
    pub fn discover() -> Self {
        let directories = directories();
        Self {
            games: scan(&directories),
            directories,
            search: String::new(),
            filter: Filter::All,
            selected: None,
            notice: None,
            starting: None,
            global: GlobalSettings::load_from_disk(),
            midi_choices: discover_midi_choices(),
            game_tab: GameTab::Overview,
            vfs_mode: VfsViewMode::AllFiles,
            vfs_dir: String::new(),
            confirm_reset_overlay: false,
            selected_save_file: None,
            new_action_name: String::new(),
            new_action_exe: String::new(),
        }
    }
    pub fn refresh_autosaves(&mut self) {
        for game in &mut self.games {
            game.refresh_state();
        }
    }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }
    pub fn directories(&self) -> &[PathBuf] {
        &self.directories
    }
    /// Genres of every game, in first-seen order of the sorted library.
    pub fn genres(&self) -> Vec<&str> {
        let mut genres: Vec<&str> = Vec::new();
        for genre in self.games.iter().flat_map(|g| &g.info.genres) {
            if !genres.contains(&genre.as_str()) {
                genres.push(genre);
            }
        }
        genres
    }
    pub fn visible(&self) -> impl Iterator<Item = (usize, &Game)> {
        self.games
            .iter()
            .enumerate()
            .filter(|(_, game)| game.matches(&self.search, &self.filter))
    }
    pub fn load_art(&self) -> Task<Message> {
        Task::batch(self.games.iter().filter_map(|game| {
            let path = game.cover.clone()?;
            Some(Task::perform(
                async move {
                    let art = artwork(&path).map_err(|error| {
                        eprintln!("[WARN doslet] Library: cover {}: {error}", path.display())
                    });
                    (path, art.ok())
                },
                |(path, art)| Message::Art(path, art),
            ))
        }))
    }
    fn edit_global(&mut self, edit: GlobalEdit) {
        match edit {
            GlobalEdit::AudioEnabled(v) => self.global.audio_enabled = v,
            GlobalEdit::AudioVolume(v) => self.global.audio_volume = v.min(100),
            GlobalEdit::AudioMixer(v) => self.global.audio_mixer = v,
            GlobalEdit::MidiOutput(v) => self.global.midi_output = v,
            GlobalEdit::RefreshMidiPorts => {
                self.midi_choices = discover_midi_choices();
                return;
            }
            GlobalEdit::GameMidi(v) => self.global.game_midi = v,
            GlobalEdit::HostMidi(v) => self.global.host_midi = v,
            GlobalEdit::Mt32RomDir(v) => {
                self.global.mt32_rom_dir = (!v.trim().is_empty()).then_some(v);
            }
            GlobalEdit::Keyboard(v) => self.global.keyboard = v,
            GlobalEdit::Gameport(v) => self.global.gameport = v,
            GlobalEdit::MouseMode(v) => self.global.input.mouse_mode = v,
            GlobalEdit::HideCursor(v) => self.global.input.hide_cursor = v,
            GlobalEdit::PadIndex(v) => self.global.input.pad_index = v,
            GlobalEdit::Deadzone(v) => self.global.input.deadzone_percent = v.min(95),
            GlobalEdit::VideoFilter(v) => self.global.video_filter = v,
            GlobalEdit::Scaling(v) => self.global.scaling = v,
            GlobalEdit::Aspect(v) => self.global.aspect = v,
            GlobalEdit::DefaultPreset(v) => self.global.default_preset = v,
        }
        if let Err(error) = self.global.save_to_disk() {
            self.notice = Some(format!("Cannot save global settings: {error}"));
        }
    }
    fn edit_game(&mut self, edit: GameEdit) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(game) = self.games.get_mut(index) else {
            return;
        };
        match edit {
            GameEdit::Toggle => {
                self.game_tab = if self.game_tab == GameTab::Configuration {
                    GameTab::Overview
                } else {
                    GameTab::Configuration
                };
            }
            GameEdit::Preset(id) => {
                if id.is_empty() || id == "default" {
                    game.spec.preset = None;
                    if let Some(p) = self.global.find_preset(&self.global.default_preset) {
                        p.apply_to(&mut game.spec.hardware, &mut game.spec.cpu_mode);
                    }
                } else {
                    game.spec.preset = Some(id.clone());
                    if let Some(p) = self
                        .global
                        .find_preset(&id)
                        .or_else(|| HardwarePreset::find_builtin(&id))
                    {
                        p.apply_to(&mut game.spec.hardware, &mut game.spec.cpu_mode);
                    }
                }
            }
            GameEdit::CpuMhz(mhz) => game.spec.hardware.cpu_mhz = mhz.clamp(1, 1000),
            GameEdit::CpuMode(mode) => game.spec.cpu_mode = Some(mode),
            GameEdit::MemoryMib(mib) => game.spec.hardware.memory_mib = mib,
            GameEdit::Sb16(v) => game.spec.hardware.sb16 = v,
            GameEdit::Mpu401(v) => game.spec.hardware.mpu401 = v,
            GameEdit::GameMidi(v) => game.spec.output.game_midi = v,
            GameEdit::Gameport(v) => game.spec.hardware.gameport = v,
            GameEdit::Executable(v) => game.spec.game.executable = v,
            GameEdit::NewActionName(v) => self.new_action_name = v,
            GameEdit::NewActionExe(v) => self.new_action_exe = v,
            GameEdit::AddAction => {
                let exe = self.new_action_exe.trim().to_owned();
                if exe.is_empty() {
                    self.notice = Some("Enter an executable name (e.g. SETUP.EXE).".into());
                    return;
                }
                let name = if self.new_action_name.trim().is_empty() {
                    exe.clone()
                } else {
                    self.new_action_name.trim().to_owned()
                };
                let id: String = name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_lowercase()
                        } else {
                            '-'
                        }
                    })
                    .collect();
                game.spec.game.actions.push(GameAction {
                    id,
                    name,
                    executable: exe,
                    arguments: Vec::new(),
                });
                game.actions = game.spec.game.actions.clone();
                self.new_action_name.clear();
                self.new_action_exe.clear();
            }
            GameEdit::RemoveAction(action_idx) => {
                if action_idx < game.spec.game.actions.len() {
                    game.spec.game.actions.remove(action_idx);
                    game.actions = game.spec.game.actions.clone();
                }
            }
            GameEdit::Save => {
                let text = machine_profile::export_machine_profile(&game.spec);
                match std::fs::write(&game.profile, text) {
                    Ok(()) => {
                        game.actions = game.spec.game.actions.clone();
                        self.notice = Some(format!("Saved configuration for {}.", game.name));
                    }
                    Err(error) => {
                        self.notice = Some(format!("Cannot save {}: {error}", game.profile.display()));
                    }
                }
            }
        }
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        use iced::keyboard::{Event, Key, key::Named};
        match message {
            Message::Search(text) => {
                self.search = text;
                self.selected = None;
                self.game_tab = GameTab::Overview;
                self.selected_save_file = None;
                if self.filter == Filter::Settings {
                    self.filter = Filter::All;
                }
            }
            Message::Filter(filter) => {
                self.filter = filter;
                self.selected = None;
                self.game_tab = GameTab::Overview;
                self.selected_save_file = None;
            }
            Message::Open(index) => {
                self.selected = Some(index).filter(|&i| i < self.games.len());
                self.game_tab = GameTab::Overview;
                self.vfs_dir.clear();
                self.confirm_reset_overlay = false;
                self.selected_save_file = None;
                self.refresh_autosaves();
            }
            Message::Back => {
                self.selected = None;
                self.game_tab = GameTab::Overview;
                self.vfs_dir.clear();
                self.confirm_reset_overlay = false;
                self.selected_save_file = None;
            }
            Message::SelectTab(tab) => {
                self.game_tab = tab;
                self.confirm_reset_overlay = false;
                if tab == GameTab::VfsAndState {
                    self.refresh_autosaves();
                }
            }
            Message::SetVfsMode(mode) => {
                self.vfs_mode = mode;
                self.selected_save_file = None;
            }
            Message::NavigateVfsDir(dir) => {
                self.vfs_dir = dir;
                self.selected_save_file = None;
            }
            Message::SelectSaveFile(file_idx) => {
                if self.selected_save_file == file_idx {
                    self.selected_save_file = None;
                } else {
                    self.selected_save_file = file_idx;
                    if let (Some(game_idx), Some(f_idx)) = (self.selected, file_idx)
                        && let Some(game) = self.games.get_mut(game_idx)
                        && let Some(summary) = &mut game.store_summary
                        && let Some(entry) = summary.files.get_mut(f_idx)
                        && entry.preview.is_none()
                        && entry.kind == crate::native_store_format::OverlayFileKind::Unchanged
                    {
                        entry.preview = read_base_preview(
                            game.base_c_source.as_ref(),
                            &game.spec.game.executable,
                            &entry.path,
                        );
                    }
                }
            }
            Message::DiscardAutosave(index) => {
                if let Some(game) = self.games.get_mut(index) {
                    let _ = std::fs::remove_file(&game.autosave);
                    game.refresh_state();
                    self.notice = Some(
                        "Discarded paused machine state. Persistent disk saves remain intact."
                            .into(),
                    );
                }
            }
            Message::RequestResetOverlay(confirm) => {
                self.confirm_reset_overlay = confirm;
            }
            Message::ConfirmResetOverlay(index) => {
                self.confirm_reset_overlay = false;
                self.selected_save_file = None;
                if let Some(game) = self.games.get_mut(index) {
                    let _ = std::fs::remove_file(
                        game.store_dir.join(crate::native_store_format::STORE_FILE),
                    );
                    game.refresh_state();
                    self.notice = Some(
                        "Reset persistent disk overlay to the clean base image.".into(),
                    );
                }
            }
            // The shell owns the window and starts games itself.
            Message::Launch(_)
            | Message::Restart(_)
            | Message::LaunchAction(_, _)
            | Message::LaunchDemo => {}
            Message::Global(edit) => self.edit_global(edit),
            Message::GameConfig(edit) => self.edit_game(edit),
            Message::Art(path, art) => {
                for game in &mut self.games {
                    if game.cover.as_ref() == Some(&path) {
                        game.art.clone_from(&art);
                    }
                }
            }
            Message::Dismiss => self.notice = None,
            // Indices name cards; keep them stable while a game starts.
            Message::Rescan if self.starting.is_none() => {
                self.games = scan(&self.directories);
                self.selected = None;
                self.game_tab = GameTab::Overview;
                self.vfs_dir.clear();
                self.confirm_reset_overlay = false;
                self.selected_save_file = None;
                return self.load_art();
            }
            Message::Rescan => {}
            Message::Key(Event::KeyPressed { key, .. }) => match (key, self.selected) {
                (Key::Named(Named::Escape), Some(_)) => {
                    if self.selected_save_file.is_some() {
                        self.selected_save_file = None;
                    } else if self.game_tab != GameTab::Overview {
                        self.game_tab = GameTab::Overview;
                    } else {
                        self.selected = None;
                    }
                }
                (Key::Named(Named::Enter), Some(index))
                    if self.game_tab != GameTab::Configuration =>
                {
                    return Task::done(Message::Launch(index));
                }
                _ => {}
            },
            Message::Key(_) => {}
        }
        Task::none()
    }
    pub fn view(&self) -> iced::Element<'_, Message> {
        view::view(self)
    }
    pub fn subscription(&self) -> Subscription<Message> {
        iced::keyboard::listen().map(Message::Key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "doslet-library-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn profiles_become_sorted_cards_with_their_info() {
        let dir = Directory::new();
        let write = |name: &str, text: &str| std::fs::write(dir.0.join(name), text).unwrap();
        write(
            "b.toml",
            "version=2\nname='Zeta'\n[info]\nreleased=1994\ngenres=['Action']\ndescription='Text.'\n[game]\nexecutable='dir/ZETA.EXE'\n[[game.actions]]\nid='setup'\nname='Setup'\nexecutable='dir/SETUP.EXE'\n[[drives]]\nletter='C'\nkind='zip'\nsource='zeta.zip'\n",
        );
        write(
            "a.toml",
            "version=1\nname='Alpha'\n[game]\nexecutable='ALPHA.EXE'\n",
        );
        write("broken.toml", "version=2\n[info]\nreleased='soon'\n");
        write("notes.txt", "not a profile");
        let games = scan(std::slice::from_ref(&dir.0));
        let names: Vec<_> = games.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Zeta"]);
        let zeta = &games[1];
        assert_eq!(zeta.program, "ZETA.EXE");
        assert_eq!(zeta.actions.len(), 1);
        assert_eq!(zeta.actions[0].name, "Setup");
        assert_eq!(zeta.drive.as_deref(), Some("ZIP archive · zeta.zip"));
        assert_eq!(zeta.caption(), "1994 · Action");
        assert_eq!(zeta.info.description.as_deref(), Some("Text."));
        assert_eq!(games[0].caption(), "ALPHA.EXE");
        let action = Filter::Genre("Action".into());
        assert!(zeta.matches("act", &Filter::All) && zeta.matches("", &action));
        assert!(!zeta.matches("alpha", &Filter::All) && !games[0].matches("", &action));
    }

    #[test]
    fn covers_decode_into_card_art_and_a_small_backdrop() {
        let dir = Directory::new();
        let path = dir.0.join("cover.png");
        image::RgbaImage::from_pixel(1600, 900, image::Rgba([200, 40, 40, 255]))
            .save(&path)
            .unwrap();
        let art = artwork(&path).unwrap();
        assert!(art.tint.r > 0.7 && art.tint.g < 0.2);
        std::fs::write(&path, b"not an image").unwrap();
        assert!(artwork(&path).is_err());
    }

    /// Visual check with the local profiles:
    /// `cargo test -p doslet --bin doslet library_screenshots -- --ignored`
    #[test]
    #[ignore = "requires a wgpu adapter and local ./games profiles; writes target/library-*.png"]
    fn library_screenshots() {
        use iced::{Font, Pixels, Point, Size};
        use iced_runtime::core::{Event, clipboard, mouse, renderer, window};
        use iced_runtime::user_interface::{Cache, UserInterface};
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directories = vec![root.join("games")];
        let mut library = Library {
            games: scan(&directories),
            directories,
            search: String::new(),
            filter: Filter::All,
            selected: None,
            notice: None,
            starting: None,
            global: GlobalSettings::default(),
            midi_choices: vec!["rust".into()],
            game_tab: GameTab::Overview,
            vfs_mode: VfsViewMode::AllFiles,
            vfs_dir: String::new(),
            confirm_reset_overlay: false,
            selected_save_file: None,
            new_action_name: String::new(),
            new_action_exe: String::new(),
        };
        assert!(!library.is_empty(), "no profiles in ./games");
        for game in &mut library.games {
            game.art = game.cover.as_deref().and_then(|path| artwork(path).ok());
        }
        let mut renderer = iced::futures::executor::block_on(
            <iced::Renderer as renderer::Headless>::new(Font::DEFAULT, Pixels(16.0), Some("wgpu")),
        )
        .expect("a wgpu adapter is required for this opt-in test");
        let theme = crate::ui::theme();
        let mut shoot = |library: &Library, name: &str, size: Size, cursor: Option<Point>| {
            let cursor = cursor.map_or(mouse::Cursor::Unavailable, mouse::Cursor::Available);
            let events = [
                Event::Mouse(mouse::Event::CursorMoved {
                    position: cursor.position().unwrap_or_default(),
                }),
                Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
            ];
            let (width, height) = (size.width as u32, size.height as u32);
            let mut cache = Cache::default();
            let mut pixels = Vec::new();
            // Images upload in the background; redraw until they are resident.
            for _ in 0..10 {
                let mut ui = UserInterface::build(library.view(), size, cache, &mut renderer);
                ui.update(
                    &events,
                    cursor,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut Vec::new(),
                );
                let style = renderer::Style {
                    text_color: crate::ui::FOREGROUND,
                };
                ui.draw(&mut renderer, &theme, &style, cursor);
                cache = ui.into_cache();
                pixels = renderer::Headless::screenshot(
                    &mut renderer,
                    Size::new(width, height),
                    1.0,
                    Color::BLACK,
                );
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            let path = root.join(format!("target/library-{name}.png"));
            image::RgbaImage::from_raw(width, height, pixels)
                .unwrap()
                .save(&path)
                .unwrap();
        };
        let wide = Size::new(1280.0, 820.0);
        shoot(&library, "grid", wide, None);
        shoot(&library, "hover", wide, Some(Point::new(420.0, 230.0)));
        shoot(&library, "narrow", Size::new(760.0, 560.0), None);
        let covered = library.games.iter().position(|g| g.art.is_some());
        let plain = library.games.iter().position(|g| g.art.is_none());
        for (name, index) in [("page", covered), ("page-plain", plain)] {
            library.selected = index;
            if index.is_some() {
                shoot(&library, name, wide, None);
            }
        }
        library.selected = covered;
        shoot(&library, "page-narrow", Size::new(760.0, 560.0), None);
        library.selected = None;
        // The first card's art spans about (283, 120)–(565, 278); its Play
        // button sits in the bottom-right corner of the hover layer.
        let mut click = |position: Point| {
            let cursor = mouse::Cursor::Available(position);
            let mut ui =
                UserInterface::build(library.view(), wide, Cache::default(), &mut renderer);
            let mut messages = Vec::new();
            for event in [
                mouse::Event::CursorMoved { position },
                mouse::Event::ButtonPressed(mouse::Button::Left),
                mouse::Event::ButtonReleased(mouse::Button::Left),
            ] {
                ui.update(
                    &[Event::Mouse(event)],
                    cursor,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
            }
            messages
        };
        assert!(matches!(
            click(Point::new(520.0, 255.0))[..],
            [Message::Launch(0)]
        ));
        assert!(matches!(
            click(Point::new(330.0, 150.0))[..],
            [Message::Open(0)]
        ));
        assert!(matches!(
            click(Point::new(330.0, 305.0))[..],
            [Message::Open(0)]
        ));
    }
}
