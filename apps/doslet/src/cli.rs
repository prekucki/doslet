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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DpmiMode {
    Off,
    Reflect,
    Direct,
}
impl DpmiMode {
    pub fn host(self) -> doslet_config::DpmiHost {
        match self {
            Self::Off => doslet_config::DpmiHost::Off,
            Self::Reflect => doslet_config::DpmiHost::Reflect,
            Self::Direct => doslet_config::DpmiHost::Direct,
        }
    }
}

/// Run a DOS game in the Doslet desktop window.
#[derive(Debug, Default, Parser)]
#[command(
    version,
    after_help = "Without a game source, opens the game library of the user's profile directory, or the color-bar demo when it holds none.\nGame support is experimental. Use --game EXE, --zip ZIP [--program EXE], or --config TOML [--program EXE] for a ZIP profile."
)]
pub struct Cli {
    /// Load a TOML launch profile; explicit CLI options override profile values
    #[arg(long, value_name = "TOML")]
    pub config: Option<PathBuf>,
    /// Open the built-in color-bar demo instead of the game library
    #[arg(long, conflicts_with_all = ["config", "game", "zip", "load_state"])]
    pub demo: bool,
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
    /// ZIP game installation mounted in memory as C:
    #[arg(long, value_name = "ZIP", conflicts_with = "game")]
    pub zip: Option<PathBuf>,
    /// EXE or COM path inside the ZIP (from --zip or a ZIP profile); optional
    /// with --zip when it contains one program
    #[arg(long, value_name = "EXE", conflicts_with = "game")]
    pub program: Option<String>,
    /// C: writes: ro denies, overlay keeps in memory (default), rw persists closed files
    #[arg(long, value_enum)]
    pub drive_mode: Option<DriveMode>,

    /// Run without opening an audio output device
    #[arg(long, conflicts_with = "midi_output")]
    pub no_audio: bool,

    /// Host audio adapter; PulseAudio uses a Rust protocol client on Linux
    #[arg(long, value_enum, default_value = "cpal")]
    pub audio_backend: crate::audio::Backend,

    /// Mixer; experimental system mixing selects PulseAudio on Linux
    #[arg(long, value_enum, default_value = "application")]
    pub audio_mixer: crate::audio::Mixer,

    /// Experimentally batch three Heroes timer polling loops; off by default
    #[arg(long)]
    pub experimental_heroes_timer_waits: bool,
    /// Adaptively yield known empty Heroes event loops; changes polling cadence
    #[arg(long, alias = "experimental-heroes-adaptive-idle")]
    pub experimental_heroes_menu_idle: bool,
    /// Observe recurring code and input queries without skipping execution
    #[arg(long)]
    pub observe_idle: bool,
    /// Experimentally budget empty input polls and countdown guards; enables observation
    #[arg(long, conflicts_with = "experimental_heroes_menu_idle")]
    pub adaptive_idle: bool,

    /// Trace bounded synchronization episodes without changing CPU throughput
    #[arg(long)]
    pub analyze_synchronization: bool,
    /// Experimental CPU budget from complete synchronization episodes
    #[arg(long, conflicts_with_all = ["adaptive_idle", "experimental_heroes_menu_idle", "experimental_heroes_timer_waits"])]
    pub adaptive_cpu: bool,

    /// Frozen execution labels with independently configurable CPU rates
    #[arg(long, value_name = "JSON", conflicts_with_all = ["adaptive_cpu", "adaptive_idle", "analyze_synchronization", "observe_idle", "experimental_heroes_menu_idle", "experimental_heroes_timer_waits"])]
    pub speed_tier_profile: Option<PathBuf>,
    /// Directory for profiles indexed by SHA-256 of the game EXE (default user data directory)
    #[arg(long, value_name = "DIR")]
    pub speed_tier_cache: Option<PathBuf>,
    /// Disable automatic activation of a previously learned game profile
    #[arg(long, conflicts_with = "speed_tier_profile")]
    pub no_speed_tiers: bool,
    /// Reclassify GREY >10% using paired one-second background state replays
    #[arg(long, requires = "speed_tier_profile")]
    pub learn_speed_tiers: bool,
    /// Cold candidate throughput for --speed-tier-profile (default 8 MHz)
    #[arg(long, value_name = "MHZ", requires = "speed_tier_profile", value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub cold_mhz: Option<u16>,
    /// HOT throughput; omit for MAX (zero instruction-budget cost)
    #[arg(long, value_name = "MHZ", requires = "speed_tier_profile", value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub hot_mhz: Option<u16>,
    /// GREY throughput (default 133 MHz), including IRQ code
    #[arg(long, value_name = "MHZ", requires = "speed_tier_profile", value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub grey_mhz: Option<u16>,
    /// Resume a Protobuf state (.pb or .pb.gz); use --config to resolve its media
    #[arg(long, value_name = "STATE", conflicts_with_all = ["cpu_mhz", "memory_mib", "no_x87", "no_sb16", "no_mpu401", "drive_mode"])]
    pub load_state: Option<PathBuf>,

    /// Use the experimental Cranelift JIT; slower than the interpreter on ROTT
    #[arg(long)]
    pub dynamic_core: bool,
    /// Accepted for older launch commands; the interpreter is now the default
    #[arg(long, hide = true, conflicts_with = "dynamic_core")]
    pub no_dynamic_core: bool,
    /// DPMI host [default: the profile's machine.dpmi, else direct]; direct
    /// dispatches eligible BIOS/DOS services from protected mode, reflect runs
    /// them in real mode, off leaves DOS extenders their own host
    #[arg(long, value_enum, alias = "experimental-dpmi")]
    pub dpmi: Option<DpmiMode>,

    /// Listen for a remote GDB client; attachment stops the guest
    #[arg(long, value_name = "IP:PORT")]
    pub gdb: Option<std::net::SocketAddr>,

    /// Enable guest screen/input tools over MCP on a loopback address
    #[arg(long, value_name = "IP:PORT")]
    pub mcp: Option<std::net::SocketAddr>,

    /// Load/save observed JIT blocks between runs (default: per-game file in the user cache)
    #[arg(long, value_name = "FILE")]
    pub core_profile: Option<PathBuf>,
    /// Neither load nor save a dynamic-core profile
    #[arg(long, conflicts_with = "core_profile")]
    pub no_core_profile: bool,
    /// Install this AOT procedure module at launch (.wasm with embedded metadata; legacy .json also accepted)
    #[arg(long, value_name = "WASM_OR_JSON")]
    pub wasm_procedures: Option<PathBuf>,
    /// Do not compile or install procedures for the live layout of the program
    #[arg(long, conflicts_with = "wasm_procedures")]
    pub no_aot: bool,
    /// Interpret marked procedures with the bundle's IRQ boundary policy
    #[arg(long, requires = "wasm_procedures")]
    pub interpret_procedures: bool,

    /// MIDI port index/name, timidity, rust, apple (macOS), or moont
    #[arg(long, value_name = "PORT")]
    pub midi_output: Option<String>,

    /// MIDI dialect produced by the game (default gm); Roland SysEx upgrades GM
    #[arg(long, value_enum)]
    pub midi_mode: Option<MidiMode>,

    /// Dialect of a host MIDI port (default gm; MT-32 hardware/Munt may use mt32)
    #[arg(long, value_enum)]
    pub midi_output_mode: Option<MidiMode>,

    /// Override automatic DOSBox ROM discovery for CM-32L / MT-32 audio
    #[arg(long, value_name = "DIR", conflicts_with = "no_audio")]
    pub mt32_rom_dir: Option<PathBuf>,

    /// List built-in outputs and host MIDI ports, then exit without a window
    #[arg(long)]
    pub list_midi_outputs: bool,

    /// Launch a named secondary action from `[game.actions]` (e.g. `setup`)
    #[arg(long, hide = true)]
    pub action: Option<String>,

    /// Path to save/restore automatic suspend state for library launches
    #[arg(long, hide = true)]
    pub autosave_path: Option<PathBuf>,

    #[arg(skip)]
    pub launch: crate::native_input::LaunchSettings,
    #[arg(skip)]
    pub profile_name: Option<String>,
    #[arg(skip)]
    pub global_settings: Option<crate::global_settings::GlobalSettings>,
}

impl Cli {
    /// No game source or debugging endpoint: show the game library instead.
    pub fn opens_library(&self) -> bool {
        self.config.is_none()
            && self.game.is_none()
            && self.zip.is_none()
            && self.program.is_none()
            && self.load_state.is_none()
            && self.gdb.is_none()
            && self.mcp.is_none()
            && !self.demo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_opens_only_without_a_game_or_debugger() {
        let parse = |args: &[&str]| Cli::try_parse_from([&["doslet"], args].concat()).unwrap();
        assert!(parse(&[]).opens_library());
        assert!(parse(&["--no-audio", "--experimental-dpmi", "off"]).opens_library());
        for args in [
            &["--demo"][..],
            &["--config", "games/ROTT.toml"],
            &["--game", "GAME.EXE"],
            &["--zip", "GAME.ZIP"],
            &["--mcp", "127.0.0.1:9000"],
        ] {
            assert!(!parse(args).opens_library(), "{args:?}");
        }
        assert!(Cli::try_parse_from(["doslet", "--demo", "--config", "a.toml"]).is_err());
    }

    #[test]
    fn explicit_procedure_modules_need_a_path() {
        // Speed tiers account the calls of a module, so neither excludes the other.
        let cli = Cli::try_parse_from(["doslet", "--wasm-procedures", "procedures.wasm"]).unwrap();
        assert_eq!(cli.wasm_procedures, Some(PathBuf::from("procedures.wasm")));
        assert!(!cli.interpret_procedures);
        assert!(Cli::try_parse_from(["doslet", "--wasm-procedures"]).is_err());
        assert!(Cli::try_parse_from(["doslet", "--interpret-procedures"]).is_err());
        let reference = Cli::try_parse_from([
            "doslet",
            "--wasm-procedures",
            "procedures.json",
            "--interpret-procedures",
        ])
        .unwrap();
        assert!(reference.interpret_procedures);
    }
    use clap::error::ErrorKind;
    #[test]
    fn application_mixing_is_default_and_system_is_explicit() {
        let cli = Cli::try_parse_from(["doslet"]).unwrap();
        assert_eq!(cli.audio_mixer, crate::audio::Mixer::Application);
        assert_eq!(cli.audio_backend, crate::audio::Backend::Cpal);
        let cli = Cli::try_parse_from([
            "doslet",
            "--audio-mixer",
            "system",
            "--audio-backend",
            "pulse",
        ])
        .unwrap();
        assert_eq!(cli.audio_mixer, crate::audio::Mixer::System);
        assert_eq!(cli.audio_backend, crate::audio::Backend::Pulse);
        assert!(Cli::try_parse_from(["doslet", "--audio-mixer", "unknown"]).is_err());
    }
    #[test]
    fn speed_tiers_and_snapshot_options_are_explicit() {
        let cli = Cli::try_parse_from([
            "doslet",
            "--load-state",
            "city.pb.gz",
            "--speed-tier-profile",
            "city.json",
            "--cold-mhz",
            "8",
        ])
        .unwrap();
        assert_eq!(cli.cold_mhz, Some(8));
        assert!(cli.load_state.is_some());
        for extra in [
            "--adaptive-cpu",
            "--adaptive-idle",
            "--analyze-synchronization",
        ] {
            assert!(
                Cli::try_parse_from(["doslet", "--speed-tier-profile", "city.json", extra])
                    .is_err()
            );
        }
        assert!(Cli::try_parse_from(["doslet", "--cold-mhz", "8"]).is_err());
        assert!(
            Cli::try_parse_from(["doslet", "--load-state", "city.pb.gz", "--cpu-mhz", "66"])
                .is_err()
        );
    }

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
            "--experimental-dpmi",
            "direct",
            "--core-profile",
            "target/game.core-profile",
        ])
        .unwrap()
        .resolve()
        .unwrap();
        assert!(cli.launch.no_dynamic_core);
        assert_eq!(
            cli.launch.dpmi,
            Some(doslet_runtime::session::dpmi::Mode::Direct)
        );
        assert_eq!(
            cli.launch.core_profile,
            Some(PathBuf::from("target/game.core-profile"))
        );
        // The JIT is slower than the interpreter on measured ROTT gameplay.
        let default = Cli::try_parse_from(["doslet"]).unwrap().resolve().unwrap();
        assert!(default.launch.no_dynamic_core);
        let jit = Cli::try_parse_from(["doslet", "--dynamic-core"])
            .unwrap()
            .resolve();
        if crate::cpu_config::JIT_AVAILABLE {
            assert!(!jit.unwrap().launch.no_dynamic_core);
        } else {
            assert!(jit.is_err_and(|e| e.to_string().contains("--features dynamic-core")));
        }
        assert!(Cli::try_parse_from(["doslet", "--dynamic-core", "--no-dynamic-core"]).is_err());
        assert!(
            Cli::try_parse_from(["doslet", "--no-aot", "--wasm-procedures", "a.wasm"]).is_err()
        );
    }

    #[test]
    fn dpmi_defaults_to_direct_and_supports_reflect_and_off() {
        use doslet_runtime::session::dpmi::Mode;
        let cli = Cli::try_parse_from(["doslet"]).unwrap().resolve().unwrap();
        assert_eq!(cli.launch.dpmi, Some(Mode::Direct));
        assert_eq!(
            Cli::default().resolve().unwrap().launch.dpmi,
            Some(Mode::Direct)
        );
        for (flag, argument, expected) in [
            ("--dpmi", "reflect", Some(Mode::Reflect)),
            ("--dpmi", "off", None),
            // Earlier launch commands keep working.
            ("--experimental-dpmi", "reflect", Some(Mode::Reflect)),
        ] {
            let cli = Cli::try_parse_from(["doslet", flag, argument])
                .unwrap()
                .resolve()
                .unwrap();
            assert_eq!(cli.launch.dpmi, expected);
        }
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
