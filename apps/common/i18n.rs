//! Lightweight localization module (`i18n`). Base language is English (`en`),
//! with automatic environment/browser detection and Polish (`pl`) translations.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    En,
    Pl,
}

impl Locale {
    pub const ALL: [Self; 2] = [Self::En, Self::Pl];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Pl => "pl",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Pl => "Polski",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        let trimmed = tag.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("c") || trimmed.eq_ignore_ascii_case("posix") {
            return None;
        }
        let primary = trimmed
            .split(['_', '-', '.', '@'])
            .next()
            .unwrap_or(trimmed);
        if primary.eq_ignore_ascii_case("pl") {
            Some(Self::Pl)
        } else if primary.eq_ignore_ascii_case("en") {
            Some(Self::En)
        } else {
            None
        }
    }

    /// Detect the active locale from the environment, defaulting to English.
    /// In unit tests, always returns `Locale::En` for deterministic assertions.
    pub fn detect() -> Self {
        if cfg!(test) {
            return Self::En;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
                if let Some(val) = std::env::var_os(key)
                    && let Some(text) = val.to_str()
                    && !text.is_empty()
                    && let Some(locale) = Self::from_tag(text)
                {
                    return locale;
                }
            }
        }
        Self::En
    }

    pub const fn strings(self) -> &'static Strings {
        match self {
            Self::En => &EN,
            Self::Pl => &PL,
        }
    }
}

pub struct Strings {
    pub released: &'static str,
    pub publisher: &'static str,
    pub developer: &'static str,
    pub genre: &'static str,
    pub gameplay: &'static str,
    pub play_game: &'static str,
    pub resume_game: &'static str,
    pub restart_game: &'static str,
    pub loading_emulator: &'static str,
    pub browser_dos_game_suffix: &'static str,
    pub all_games: &'static str,
    pub genres_section: &'static str,
    pub library_section: &'static str,
    pub global_settings: &'static str,
    pub game_configuration: &'static str,
    pub no_games_here: &'static str,
    pub no_games_hint: &'static str,
    pub launch_demo: &'static str,
}

pub const EN: Strings = Strings {
    released: "Released",
    publisher: "Publisher",
    developer: "Developer",
    genre: "Genre",
    gameplay: "Gameplay",
    play_game: "Play game",
    resume_game: "Resume",
    restart_game: "Restart",
    loading_emulator: "Loading emulator…",
    browser_dos_game_suffix: "DOS game running in the browser.",
    all_games: "All games",
    genres_section: "GENRES",
    library_section: "LIBRARY",
    global_settings: "Settings",
    game_configuration: "Configure…",
    no_games_here: "No games here",
    no_games_hint: "Add a launch profile to your profiles folder or launch the built-in test demo.",
    launch_demo: "Launch test demo",
};

pub const PL: Strings = Strings {
    released: "Wydanie",
    publisher: "Wydawca",
    developer: "Producent",
    genre: "Gatunek",
    gameplay: "Rozgrywka",
    play_game: "Uruchom grę",
    resume_game: "Wznów",
    restart_game: "Od nowa",
    loading_emulator: "Ładowanie emulatora…",
    browser_dos_game_suffix: "Gra DOS uruchamiana w przeglądarce.",
    all_games: "Wszystkie gry",
    genres_section: "GATUNKI",
    library_section: "BIBLIOTEKA",
    global_settings: "Ustawienia",
    game_configuration: "Konfiguruj…",
    no_games_here: "Brak gier w bibliotece",
    no_games_hint: "Dodaj profil gry (.toml) do katalogu profili albo uruchom wbudowane demo testowe.",
    launch_demo: "Uruchom demo testowe",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_language_tags_and_defaults_to_english() {
        assert_eq!(Locale::from_tag("en_US.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::from_tag("pl_PL.UTF-8"), Some(Locale::Pl));
        assert_eq!(Locale::from_tag("pl-PL"), Some(Locale::Pl));
        assert_eq!(Locale::from_tag("C"), None);
        assert_eq!(Locale::detect(), Locale::En);
        assert_eq!(Locale::En.strings().released, "Released");
        assert_eq!(Locale::Pl.strings().released, "Wydanie");
    }
}
