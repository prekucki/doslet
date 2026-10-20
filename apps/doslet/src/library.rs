//! Game library shown when Doslet starts without a game. Each launch profile
//! becomes a card; the window shell starts the chosen game in its place.
use crate::machine_profile;
use doslet_config::{GameInfo, SourceKind};
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

pub struct Game {
    pub profile: PathBuf,
    pub name: String,
    pub info: GameInfo,
    pub program: String,
    pub drive: Option<String>,
    cover: Option<PathBuf>,
    pub art: Option<Artwork>,
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
        let mut game = Self {
            profile: std::path::absolute(path).unwrap_or_else(|_| path.to_owned()),
            name: stem,
            info: GameInfo::default(),
            program: String::new(),
            drive: None,
            cover: None,
            art: None,
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
            game.program = spec.game.executable;
            game.info = spec.info;
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
        }
        game.program = game
            .program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        Ok(game)
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
}
impl Filter {
    pub fn admits(&self, game: &Game) -> bool {
        match self {
            Self::All => true,
            Self::Genre(genre) => game.info.genres.iter().any(|g| g == genre),
        }
    }
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

#[derive(Clone, Debug)]
pub enum Message {
    Search(String),
    Filter(Filter),
    Open(usize),
    Back,
    Launch(usize),
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
        }
    }
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
    pub fn update(&mut self, message: Message) -> Task<Message> {
        use iced::keyboard::{Event, Key, key::Named};
        match message {
            Message::Search(text) => {
                self.search = text;
                self.selected = None;
            }
            Message::Filter(filter) => {
                self.filter = filter;
                self.selected = None;
            }
            Message::Open(index) => self.selected = Some(index).filter(|&i| i < self.games.len()),
            Message::Back => self.selected = None,
            // The shell owns the window and starts games itself.
            Message::Launch(_) => {}
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
                return self.load_art();
            }
            Message::Rescan => {}
            Message::Key(Event::KeyPressed { key, .. }) => match (key, self.selected) {
                (Key::Named(Named::Escape), Some(_)) => self.selected = None,
                (Key::Named(Named::Enter), Some(index)) => {
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
            "version=2\nname='Zeta'\n[info]\nreleased=1994\ngenres=['Action']\ndescription='Text.'\n[game]\nexecutable='dir/ZETA.EXE'\n[[drives]]\nletter='C'\nkind='zip'\nsource='zeta.zip'\n",
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
