use clap::{Parser, ValueEnum};
use doslet_midi_routing::Mode;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum MidiMode {
    #[default]
    Gm,
    Mt32,
}
impl From<MidiMode> for Mode {
    fn from(mode: MidiMode) -> Self {
        match mode {
            MidiMode::Gm => Self::Gm,
            MidiMode::Mt32 => Self::Mt32,
        }
    }
}
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DriveMode {
    Ro,
    Overlay,
    Rw,
}

/// Run a DOS game in the Doslet desktop window.
#[derive(Debug, Default, Parser)]
#[command(
    version,
    after_help = "Without --game, opens the built-in color-bar demo.\nGame support is experimental. Keyboard/mouse input and in-memory saves are available; persistent saves are pending."
)]
pub struct Cli {
    /// Load a TOML launch profile; explicit CLI options override profile values
    #[arg(long, value_name = "TOML")]
    pub config: Option<PathBuf>,
    /// Nominal CPU clock in MHz (1..=1000, default 33); not cycle-accurate
    #[arg(long, value_name = "MHZ")]
    pub cpu_mhz: Option<u16>,
    /// RAM size in MiB (4, 8, 16 or 32)
    #[arg(long)]
    pub memory_mib: Option<u8>,
    /// Disable the emulated x87
    #[arg(long)]
    pub no_x87: bool,
    /// Disable emulated Sound Blaster 16 hardware
    #[arg(long)]
    pub no_sb16: bool,
    /// Disable emulated MPU-401 hardware
    #[arg(long)]
    pub no_mpu401: bool,
    /// DOS executable; its containing directory is mounted as C:
    #[arg(long, value_name = "EXE")]
    pub game: Option<PathBuf>,
    /// C: writes: ro denies, overlay keeps in memory (default), rw persists closed files
    #[arg(long, value_enum)]
    pub drive_mode: Option<DriveMode>,

    /// Run without opening an audio output device
    #[arg(long, conflicts_with = "midi_output")]
    pub no_audio: bool,

    /// Experimentally batch three Heroes timer polling loops; off by default
    #[arg(long)]
    pub experimental_heroes_timer_waits: bool,

    /// Use the interpreter instead of the native dynamic core
    #[arg(long)]
    pub no_dynamic_core: bool,

    /// Load/save observed JIT blocks between runs (local profile, outside game data)
    #[arg(long, value_name = "FILE")]
    pub core_profile: Option<PathBuf>,

    /// MIDI port index/name, rust, apple (macOS), or moont
    #[arg(long, value_name = "PORT")]
    pub midi_output: Option<String>,

    /// MIDI dialect produced by the game (default gm); Roland SysEx upgrades GM
    #[arg(long, value_enum)]
    pub midi_mode: Option<MidiMode>,

    /// Dialect of a host MIDI port (default gm; MT-32 hardware/Munt may use mt32)
    #[arg(long, value_enum)]
    pub midi_output_mode: Option<MidiMode>,

    /// Load CM32L_CONTROL.ROM and CM32L_PCM.ROM for MT-32-family audio
    #[arg(long, value_name = "DIR", conflicts_with = "no_audio")]
    pub mt32_rom_dir: Option<PathBuf>,

    /// List built-in outputs and host MIDI ports, then exit without a window
    #[arg(long)]
    pub list_midi_outputs: bool,

    #[arg(skip)]
    pub launch: crate::native_input::LaunchSettings,
    #[arg(skip)]
    pub profile_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn midi_modes_and_moont_port_selection_parse_explicitly() {
        let cli = Cli::try_parse_from(["doslet"]).unwrap();
        assert_eq!(cli.midi_mode, None);
        assert_eq!(cli.midi_output_mode, None);
        let cli = Cli::try_parse_from([
            "doslet",
            "--midi-mode",
            "mt32",
            "--midi-output-mode",
            "mt32",
            "--midi-output",
            "moont",
            "--mt32-rom-dir",
            "/local/roms",
        ])
        .unwrap();
        assert_eq!(cli.midi_mode, Some(MidiMode::Mt32));
        assert_eq!(cli.midi_output_mode, Some(MidiMode::Mt32));
        assert_eq!(cli.midi_output.as_deref(), Some("moont"));
        assert!(Cli::try_parse_from(["doslet", "--midi-mode", "auto"]).is_err());
    }
    #[test]
    fn game_options_and_demo_default() {
        let cli =
            Cli::try_parse_from(["doslet", "--game", "my game/PRINCE.EXE", "--no-audio"]).unwrap();
        assert_eq!(cli.game.unwrap(), PathBuf::from("my game/PRINCE.EXE"));
        assert!(cli.no_audio);
        assert!(Cli::try_parse_from(["doslet"]).unwrap().game.is_none());
    }

    #[test]
    fn native_core_options_survive_launch_resolution() {
        let cli = Cli::try_parse_from([
            "doslet",
            "--no-dynamic-core",
            "--core-profile",
            "target/game.core-profile",
        ])
        .unwrap()
        .resolve()
        .unwrap();
        assert!(cli.launch.no_dynamic_core);
        assert_eq!(
            cli.launch.core_profile,
            Some(PathBuf::from("target/game.core-profile"))
        );
        assert!(
            !Cli::try_parse_from(["doslet"])
                .unwrap()
                .resolve()
                .unwrap()
                .launch
                .no_dynamic_core
        );
    }

    #[test]
    fn experimental_timer_waits_are_explicit_and_survive_launch_resolution() {
        let cli = Cli::try_parse_from(["doslet"]).unwrap().resolve().unwrap();
        assert!(!cli.launch.heroes_timer_waits);
        let cli = Cli::try_parse_from(["doslet", "--experimental-heroes-timer-waits"])
            .unwrap()
            .resolve()
            .unwrap();
        assert!(cli.launch.heroes_timer_waits);
    }

    #[test]
    fn invalid_arguments_and_help_are_handled_before_gui_startup() {
        for args in [vec!["doslet", "--game"], vec!["doslet", "--unknown"]] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert_eq!(
            Cli::try_parse_from(["doslet", "--help"])
                .unwrap_err()
                .kind(),
            ErrorKind::DisplayHelp
        );
    }
    #[test]
    fn host_midi_selection_and_listing_are_explicit() {
        let cli = Cli::try_parse_from(["doslet", "--midi-output", "0"]).unwrap();
        assert_eq!(cli.midi_output.as_deref(), Some("0"));
        assert!(
            Cli::try_parse_from(["doslet", "--list-midi-outputs"])
                .unwrap()
                .list_midi_outputs
        );
        assert!(Cli::try_parse_from(["doslet", "--no-audio", "--midi-output", "0"]).is_err());
    }
}
