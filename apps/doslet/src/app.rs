use crate::native_audio::{Config as AudioConfig, Engine as AudioEngine, Source as AudioSource};
use crate::native_synth::APPLE_LABEL;
#[cfg(all(test, target_os = "macos"))]
use crate::native_synth::Timeline;
use crate::presentation::{DisplayFrame, DisplaySizing, SizingMessage};
use doslet_midi_routing::{Mode, Routing, SNAPSHOT_CAPACITY};
use doslet_runtime::{RuntimeError, Session, SessionStatus};
use iced::Task;
use std::time::{Duration, Instant};
#[path = "app_cpu.rs"]
mod cpu;

const MAX_STEPS_PER_TICK: u32 = 400_000;

/// Guest relative reads must continue this long before Auto mode locks.
const RELATIVE_LOCK_NANOS: u64 = 300_000_000;
/// Auto mode unlocks only after this long without relative reads.
const RELATIVE_UNLOCK_NANOS: u64 = 3_000_000_000;
/// A longer gap between reads starts a new streak towards locking.
const RELATIVE_STREAK_GAP_NANOS: u64 = 1_000_000_000;

/// Hysteresis for the Auto mouse mode, measured in guest time so a pause
/// changes nothing. A game that polls relative motion unevenly (menus versus
/// play) no longer flips between a captured and a free pointer.
#[derive(Default)]
struct RelativeLatch {
    relative: bool,
    /// First and latest read of the current streak while unlocked.
    streak: Option<(u64, u64)>,
}

impl RelativeLatch {
    /// Updates from the latest guest read; returns true when it locks.
    fn observe(&mut self, last_read: Option<u64>, now: u64) -> bool {
        let Some(last) = last_read else {
            *self = Self::default();
            return false;
        };
        let idle = now.saturating_sub(last);
        if self.relative {
            if idle >= RELATIVE_UNLOCK_NANOS {
                *self = Self::default();
            }
            return false;
        }
        if idle >= RELATIVE_STREAK_GAP_NANOS {
            self.streak = None;
            return false;
        }
        let start = match self.streak {
            Some((start, previous))
                if last.saturating_sub(previous) < RELATIVE_STREAK_GAP_NANOS =>
            {
                start
            }
            _ => last,
        };
        self.streak = Some((start, last));
        if last.saturating_sub(start) >= RELATIVE_LOCK_NANOS {
            *self = Self {
                relative: true,
                streak: None,
            };
            return true;
        }
        false
    }
}
const CHUNK: u32 = 10_000;
const MAX_WORK_PER_TICK: Duration = Duration::from_millis(8);
struct PreparedOutput {
    deferred: bool,
    audio: Option<crate::audio::Output>,
    host_midi: Option<crate::native_midi::Output>,
    rom_dir: Option<std::path::PathBuf>,
    audio_engine: AudioEngine,
    midi_mode: Mode,
    output_mode: Mode,
    choice: String,
    choices: Vec<String>,
    routing: Routing,
}
struct PreparedMachine {
    session: Session,
    source: Option<std::path::PathBuf>,
    launch: crate::native_input::LaunchSettings,
    spec: doslet_config::MachineSpec,
}
pub(crate) struct MachinePreparation {
    old_storage: Option<crate::native_storage::NativeStorage>,
    result: Result<(Box<PreparedMachine>, crate::native_storage::NativeStorage), String>,
}
impl core::fmt::Debug for MachinePreparation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MachinePreparation")
    }
}
type PreparationSlot = std::sync::Arc<std::sync::Mutex<Option<MachinePreparation>>>;
pub struct Doslet {
    pub session: doslet_debugger::DebugSession,
    debugger: Option<doslet_debugger::Debugger>,
    computer: Option<crate::native_computer::Computer>,
    adaptive_meter: crate::adaptive_meter::Meter,
    cpu_meter: crate::cpu_meter::Meter,
    speed_tiers: Option<crate::native_speed_tiers::Controller>,
    restore_state: Option<std::path::PathBuf>,
    pub(crate) autosave_path: Option<std::path::PathBuf>,
    host_mouse_buttons: u8,
    agent_mouse_buttons: u8,
    host_modifiers: u8,
    agent_modifiers: u8,
    startup_keys_epoch: Option<u64>,
    startup_keys_index: usize,
    pub workspace: crate::ui::Workspace,
    pub configurator: crate::configurator::ConfiguratorState,
    storage: Option<crate::native_storage::NativeStorage>,
    /// Finds where a DOS extender loaded the program in each session.
    #[cfg(not(target_arch = "wasm32"))]
    layout: Option<crate::le_layout::Watcher>,
    /// The layout found in the session of this epoch.
    #[cfg(not(target_arch = "wasm32"))]
    live_layout: Option<(u64, crate::le_layout::Layout)>,
    #[cfg(all(not(target_arch = "wasm32"), feature = "wasm-procedures"))]
    aot: Option<crate::native_aot::Aot>,
    gamepads: Option<crate::native_gamepad::NativeGamepads>,
    pad_axes: [i16; 2],
    pad_buttons: [bool; 2],
    pad_generation: Option<u64>,
    pad_sequence: u64,
    pending_output: Option<Box<PreparedOutput>>,
    preparation_was_running: bool,
    pub display: DisplayFrame,
    pub display_sizing: DisplaySizing,
    pub name: String,
    pub notice: Option<String>,
    logged_fault: Option<RuntimeError>,
    logged_notice: Option<String>,
    #[cfg(feature = "wasm-procedures")]
    wasm_reported_epoch: Option<u64>,
    dynamic_error_reported: bool,
    jit: crate::native_jit::Worker,
    pub midi_choices: Vec<String>,
    pub midi_choice: String,
    host_midi: Option<crate::native_midi::Output>,
    routing: Routing,
    pub midi_mode: Mode,
    pub midi_output_mode: Mode,
    pub midi_activity: crate::midi_activity::MidiActivity,
    #[cfg(test)]
    pub archive: Option<crate::archive::GameArchive>,
    #[cfg(test)]
    pub program: Option<String>,
    #[cfg(test)]
    active_archive: Option<(crate::archive::GameArchive, String)>,
    pub(crate) worker_owned: bool,
    pub(crate) presentation_revision: u64,
    anchor: Option<(Instant, u64)>,
    audio_engine: AudioEngine,
    audio_deferred: bool,
    mt32_rom_dir: Option<std::path::PathBuf>,
    audio_backend: crate::audio::Backend,
    audio_mixer: crate::audio::Mixer,
    audio_volume: u8,
    audio_fallback_used: bool,
    midi_bytes: [u8; 1024],
    pcm: [doslet_pcm::Frame; 128],
    mouse_capture: Option<iced::window::Id>,
    mouse_capture_pending: bool,
    mouse_capture_generation: u64,
    relative_latch: RelativeLatch,
    /// Whether the last update ended with a relative (capturing) mouse mode.
    was_relative: bool,
    /// The mode switched to capturing while a dialog covered the picture.
    capture_on_switch: bool,
    /// The host pointer is over the game picture (and the window has focus).
    pointer_inside: bool,
    #[cfg(not(target_arch = "wasm32"))]
    source: Option<std::path::PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    launch: crate::native_input::LaunchSettings,
    #[cfg(not(target_arch = "wasm32"))]
    audio: Option<crate::audio::Output>,
}
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum Message {
    CpuProfile(crate::cpu_config::ProfileEdit),
    Cpu(crate::cpu_config::Edit),
    Ui(crate::ui::Action),
    Config(crate::configurator::Edit),
    MachinePrepared(Result<PreparationSlot, String>),
    DisplaySizing(SizingMessage),
    PointerMoved(u16, u16),
    PointerButton(u8, bool),
    PointerLeft,
    CaptureWindow(iced::window::Id, u64),
    MouseCapture(iced::window::Id, u64, iced::window::MouseCaptureEvent),
    Keyboard(iced::keyboard::Event),
    KeyboardCaptured(iced::keyboard::Event),
    RunPause,
    Step,
    GdbToggle,
    McpToggle,
    CpuMhz(u16),
    SpeedTiersToggle,
    SynchronizationMode {
        enabled: bool,
        adaptive: bool,
    },
    HeroesMenuIdleToggle,
    IdleObservationToggle,
    AdaptiveIdleToggle,
    IdleLoopsToggle(bool),
    VgaWaitsToggle(bool),
    V86RamWaitsToggle(bool),
    V86VgaPollingToggle(bool),
    V86MpuPollingToggle(bool),
    V86InputPollingToggle(bool),
    Reset,
    Tick,
    GamepadsReady,
    FocusLost,
    CloseRequested(iced::window::Id),
    ExitRequested,
    /// Leave a game started from the library once its saves are settled.
    ReturnToLibrary,
    /// Release a captured mouse, e.g. before the window leaves full screen.
    ReleaseMouse,
    /// Like switching the PC off: return to the library without keeping a
    /// suspended state, so the next launch starts the game afresh.
    ShutDown,
    /// The machine is paused for the library; the window shell drops it.
    LibraryReady,
    RefreshMidi,
    AudioVolume(u8),
    MidiSelected(String),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    AudioMixerSelected(crate::audio::Mixer),
    MidiModeSelected(Mode),
    MidiOutputModeSelected(Mode),
    #[cfg(test)]
    ArchiveLoaded(Result<Option<std::sync::Arc<Vec<u8>>>, String>),
    #[cfg(test)]
    ProgramSelected(String),
    #[cfg(test)]
    StartArchive,
}
impl Doslet {
    #[inline(never)]
    pub fn new() -> Self {
        Self {
            session: Session::demo().into(),
            debugger: None,
            computer: None,
            adaptive_meter: crate::adaptive_meter::Meter::default(),
            cpu_meter: crate::cpu_meter::Meter::default(),
            speed_tiers: None,
            restore_state: None,
            autosave_path: None,
            host_mouse_buttons: 0,
            agent_mouse_buttons: 0,
            host_modifiers: 0,
            agent_modifiers: 0,
            startup_keys_epoch: None,
            startup_keys_index: 0,
            workspace: crate::ui::Workspace::default(),
            configurator: crate::configurator::ConfiguratorState::default(),
            storage: None,
            #[cfg(not(target_arch = "wasm32"))]
            layout: None,
            #[cfg(not(target_arch = "wasm32"))]
            live_layout: None,
            #[cfg(all(not(target_arch = "wasm32"), feature = "wasm-procedures"))]
            aot: None,
            gamepads: None,
            pad_axes: [0; 2],
            pad_buttons: [false; 2],
            pad_generation: None,
            pad_sequence: 0,
            pending_output: None,
            preparation_was_running: false,
            display: DisplayFrame::default(),
            display_sizing: DisplaySizing::default(),
            name: "Color bars".into(),
            notice: None,
            logged_fault: None,
            logged_notice: None,
            #[cfg(feature = "wasm-procedures")]
            wasm_reported_epoch: None,
            dynamic_error_reported: false,
            jit: crate::native_jit::Worker::default(),
            midi_choices: crate::native_synth::choices(),
            midi_choice: "Rust synth".into(),
            host_midi: None,
            routing: Routing::new(Mode::Gm),
            midi_mode: Mode::Gm,
            midi_output_mode: Mode::Gm,
            midi_activity: crate::midi_activity::MidiActivity::default(),
            #[cfg(test)]
            archive: None,
            #[cfg(test)]
            program: None,
            #[cfg(test)]
            active_archive: None,
            worker_owned: false,
            presentation_revision: 0,
            anchor: None,
            audio_engine: AudioEngine::new(),
            audio_deferred: false,
            mt32_rom_dir: None,
            audio_backend: crate::audio::Backend::default(),
            audio_mixer: crate::audio::Mixer::default(),
            audio_volume: 100,
            audio_fallback_used: false,
            midi_bytes: [0; 1024],
            pcm: [doslet_pcm::EMPTY; 128],
            mouse_capture: None,
            mouse_capture_pending: false,
            mouse_capture_generation: 0,
            relative_latch: RelativeLatch::default(),
            was_relative: false,
            capture_on_switch: false,
            pointer_inside: false,
            #[cfg(not(target_arch = "wasm32"))]
            source: None,
            #[cfg(not(target_arch = "wasm32"))]
            launch: crate::native_input::LaunchSettings::default(),
            #[cfg(not(target_arch = "wasm32"))]
            audio: None,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(never)]
    fn boxed_new() -> Box<Self> {
        Box::new(Self::new())
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(never)]
    fn init_demo_session(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut session = Session::demo_configured(self.launch.hardware)?;
        crate::native_input::install_v86(&mut session, &self.launch)?;
        session.set_dynamic_core(!self.launch.no_dynamic_core);
        if let Some(path) = &self.launch.core_profile {
            crate::native_input::load_core_profile(&mut session, path);
        }
        self.session = session.into();
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(never)]
    fn init_snapshot_session(
        &mut self,
        path: std::path::PathBuf,
        game: Option<std::path::PathBuf>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(game) = game {
            // Mount resources before resolving the snapshot's logical references.
            let persist = self.launch.persist;
            self.launch.persist = false;
            let mut media_settings = self.launch.clone();
            media_settings.wasm_procedures = None;
            media_settings.interpret_procedures = false;
            self.session =
                crate::native_zip::load_game_with_settings(&game, &media_settings)?.into();
            self.source = Some(game);
            self.launch.persist = persist;
        }
        self.install_snapshot(path)?;
        if self.autosave_path.is_some() {
            self.storage = Some(crate::native_storage::NativeStorage::prepare_resumed_mut(
                &mut self.session,
                &self.launch,
            )?);
        }
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(never)]
    fn init_game_session(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut session = crate::native_zip::load_game_with_settings(&path, &self.launch)
            .map_err(|error| format!("Cannot load {}: {error}", path.display()))?;
        self.storage = Some(crate::native_storage::NativeStorage::prepare_mut(
            &mut session,
            &self.launch,
        )?);
        self.name = path.file_name().unwrap().to_string_lossy().into_owned();
        self.source = Some(path);
        session.start();
        self.session = session.into();
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(never)]
    pub fn from_cli(cli: crate::cli::Cli) -> Result<Box<Self>, Box<dyn std::error::Error>> {
        let cli = cli.resolve()?;
        if cli.learn_speed_tiers && cli.game.is_none() {
            return Err(
                "--learn-speed-tiers requires a game executable (--game, --zip or --config)".into(),
            );
        }
        let mut app = Self::boxed_new();
        app.audio_backend = cli.audio_backend;
        app.audio_mixer = cli.audio_mixer;
        app.audio_deferred = !cli.no_audio;
        if let Some(global) = &cli.global_settings {
            app.audio_volume = global.audio_volume.min(100);
            app.display_sizing.apply_settings(global);
        }
        app.gamepads = Some(crate::native_gamepad::NativeGamepads::new());
        app.midi_mode = cli.midi_mode.unwrap_or_default().into();
        app.midi_output_mode = cli.midi_output_mode.unwrap_or_default().into();
        app.autosave_path = cli.autosave_path.clone();
        app.launch = cli.launch;
        let automatic_tiers = if cli.speed_tier_profile.is_none()
            && !cli.no_speed_tiers
            && !app.launch.adaptive_cpu
            && !app.launch.adaptive_idle
            && !app.launch.analyze_synchronization
            && !app.launch.observe_idle
            && !app.launch.heroes_menu_idle
            && !app.launch.heroes_timer_waits
            && let Some(exe) = &cli.game
        {
            match crate::native_zip::cached_game(exe, &app.launch, cli.speed_tier_cache.as_deref())
            {
                Ok(tiers) => tiers,
                Err(error) => {
                    app.notice = Some(format!("Cannot load learned game profile: {error}"));
                    None
                }
            }
        } else {
            None
        };
        app.launch.preload_local_media = cli.learn_speed_tiers
            || cli.load_state.is_some()
            || cli.autosave_path.is_some()
            || automatic_tiers.as_ref().is_some_and(|t| t.stats.learning);
        let mut machine_spec = app.launch.machine_spec.clone().unwrap_or_default();
        machine_spec.hardware = app.launch.hardware;
        machine_spec.name = cli.profile_name.clone().unwrap_or_default();
        machine_spec.game.executable = cli
            .game
            .as_ref()
            .map_or(String::new(), |p| p.to_string_lossy().into_owned());
        machine_spec.output.audio = !cli.no_audio;
        machine_spec.output.game_midi = app.midi_mode.as_str().into();
        machine_spec.output.host_midi = app.midi_output_mode.as_str().into();
        machine_spec.output.midi_output = cli.midi_output.clone().unwrap_or_else(|| "rust".into());
        // The settings show the host in effect, including a --dpmi choice.
        machine_spec.dpmi = Some(crate::native_input::dpmi_host(app.launch.dpmi));
        app.display_sizing.gpu = crate::presentation::hardware_gpu();
        app.display_sizing.set_output(&machine_spec.output);
        app.configurator.set_active(machine_spec);
        if cli.game.is_none() && cli.load_state.is_none() {
            app.init_demo_session()?;
        }
        app.routing.set_mode(app.midi_mode);
        if cli.game.is_some() || cli.load_state.is_some() {
            if let Some(path) = cli.load_state {
                app.init_snapshot_session(path, cli.game)?;
            } else {
                app.init_game_session(cli.game.unwrap())?;
            }
            if let Some(name) = cli.profile_name {
                app.name = name;
            }
            // Procedures bind live addresses, so AOT also waits for the layout.
            let aot = !cli.no_aot
                && app.launch.wasm_procedures.is_none()
                && !cli.learn_speed_tiers
                && app.launch.no_dynamic_core;
            app.watch_layout(aot);
        }

        if let Some(directory) = cli.mt32_rom_dir {
            app.mt32_rom_dir = Some(directory);
            app.midi_choices.push("CM-32L / MT-32".into());
        }
        if !cli.no_audio
            && let Ok(ports) = crate::native_midi::ports()
        {
            for name in ports.names {
                if !app.midi_choices.contains(&name) {
                    app.midi_choices.push(name);
                }
            }
        }
        if let Some(selection) = cli.midi_output {
            let selection = match selection.as_str() {
                "rust" => "Rust synth",
                "apple" => APPLE_LABEL,
                "moont" => "CM-32L / MT-32",
                other => other,
            };
            app.select_midi(selection)?;
        } else if app.mt32_rom_dir.is_some() {
            app.select_midi("CM-32L / MT-32")?;
        }
        app.session
            .set_synchronization_analysis(app.launch.analyze_synchronization);
        if app.launch.adaptive_cpu {
            app.session.set_adaptive_cpu(true);
        }
        if cli.speed_tier_profile.is_some() || cli.learn_speed_tiers {
            let mut tiers = if let Some(path) = &cli.speed_tier_profile {
                crate::native_speed_tiers::Controller::load(
                    path,
                    cli.cold_mhz.unwrap_or(8),
                    cli.hot_mhz,
                    cli.grey_mhz.unwrap_or(133),
                )?
            } else {
                let mut tiers = automatic_tiers
                    .unwrap_or_else(crate::native_speed_tiers::Controller::untrained);
                tiers.configure(
                    cli.hot_mhz.or(tiers.stats.hot_mhz),
                    cli.grey_mhz.unwrap_or(tiers.stats.grey_mhz),
                    cli.cold_mhz.unwrap_or(tiers.stats.cold_mhz),
                    false,
                );
                tiers
            };
            if let Some(exe) = &app.source {
                crate::native_zip::bind_game(
                    &mut tiers,
                    exe,
                    &app.launch,
                    cli.speed_tier_cache.as_deref(),
                )?;
            }
            if cli.learn_speed_tiers {
                tiers.set_learning(true);
            }
            tiers.enable(&mut app.session);
            eprintln!(
                "[INFO doslet] speed tiers: hot {}, cold {} MHz, grey {} MHz; profile {}",
                tiers
                    .stats
                    .hot_mhz
                    .map_or_else(|| "MAX".into(), |m| format!("{m} MHz")),
                tiers.stats.cold_mhz,
                tiers.stats.grey_mhz,
                cli.speed_tier_profile.as_ref().map_or_else(
                    || "game recording".into(),
                    |path| path.display().to_string(),
                )
            );
            app.speed_tiers = Some(tiers);
        } else if let Some(mut tiers) = automatic_tiers {
            tiers.enable(&mut app.session);
            app.speed_tiers = Some(tiers);
        }
        if let Some(address) = cli.gdb {
            app.enable_gdb(address)?;
        }
        if let Some(address) = cli.mcp {
            app.enable_mcp(address)?;
        }
        app.log_errors();
        Ok(app)
    }
    pub(crate) fn enable_gdb(
        &mut self,
        address: std::net::SocketAddr,
    ) -> Result<(), std::io::Error> {
        self.debugger = Some(doslet_debugger::Debugger::listen(address)?);
        Ok(())
    }
    pub(crate) fn enable_mcp(
        &mut self,
        address: std::net::SocketAddr,
    ) -> Result<(), std::io::Error> {
        self.computer = Some(crate::native_computer::Computer::listen(address)?);
        Ok(())
    }
    pub(crate) fn audio_underruns(&self) -> Option<u64> {
        self.audio.as_ref().map(|a| a.stats().1)
    }
    fn install_snapshot(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut session = crate::native_state::load(
            &path,
            !self.launch.no_dynamic_core,
            self.session.state_resources()?,
        )?;
        crate::native_input::install_wasm_procedures(&mut session, &self.launch)?;
        crate::native_input::install_v86(&mut session, &self.launch)?;
        self.launch.persist = false;
        self.launch.hardware = session.hardware_config();
        self.configurator.active.hardware = session.hardware_config();
        self.configurator.draft.hardware = session.hardware_config();
        self.name = format!(
            "Snapshot · {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        self.restore_state = Some(path);
        self.session = (*session).into();
        Ok(())
    }
    pub(crate) fn speed_tier_stats(&self) -> Option<crate::native_speed_tiers::Stats> {
        self.speed_tiers.as_ref().map(|t| t.stats)
    }
    pub(crate) fn code_tags(
        &mut self,
        start: u32,
        end: u32,
        bits: u8,
        tier: Option<doslet_mcp::CodeTier>,
    ) -> Result<serde_json::Value, String> {
        if tier.is_some() && (self.gdb_connected() || self.configurator.pending) {
            return Err("Tagging requires no GDB connection or pending machine preparation".into());
        }
        self.speed_tiers
            .as_mut()
            .ok_or("No speed-tier profile is loaded")?
            .code_tags(
                &mut self.session,
                start,
                end,
                bits,
                tier.map(|t| match t {
                    doslet_mcp::CodeTier::Hot => crate::speed_tiers::Tier::Fast,
                    doslet_mcp::CodeTier::Cold => crate::speed_tiers::Tier::Slow,
                    doslet_mcp::CodeTier::Grey => crate::speed_tiers::Tier::Grey,
                }),
            )
    }
    fn disable_speed_tiers(&mut self) {
        if let Some(tiers) = &mut self.speed_tiers
            && tiers.stats.enabled
        {
            tiers.disable(&mut self.session);
        }
    }
    pub(crate) fn gdb_connected(&self) -> bool {
        self.debugger.as_ref().is_some_and(|gdb| gdb.connected())
    }
    fn host_pointer_button(&mut self, button: u8, pressed: bool) {
        if button >= 3 {
            return;
        }
        let bit = 1 << button;
        if pressed {
            self.host_mouse_buttons |= bit;
        } else {
            self.host_mouse_buttons &= !bit;
        }
        self.session.pointer_button(
            button,
            (self.host_mouse_buttons | self.agent_mouse_buttons) & bit != 0,
        );
    }
    pub(crate) fn agent_pointer_button(&mut self, button: u8, pressed: bool) {
        if button >= 3 {
            return;
        }
        let bit = 1 << button;
        if (self.agent_mouse_buttons & bit != 0) == pressed {
            return;
        }
        if pressed {
            self.agent_mouse_buttons |= bit;
        } else {
            self.agent_mouse_buttons &= !bit;
        }
        self.session.pointer_button(
            button,
            (self.host_mouse_buttons | self.agent_mouse_buttons) & bit != 0,
        );
    }
    pub(crate) fn agent_modifiers(&mut self, flags: u8) -> Result<(), RuntimeError> {
        self.agent_modifiers = flags;
        self.session
            .set_key_modifiers(self.host_modifiers | self.agent_modifiers)
    }
    pub(crate) fn forget_agent_input(&mut self) {
        self.agent_mouse_buttons = 0;
        self.agent_modifiers = 0;
    }
    #[cfg(test)]
    fn load_mt32(
        directory: &std::path::Path,
        rate: u32,
        mode: Mode,
    ) -> Result<doslet_mt32::Timeline, Box<dyn std::error::Error>> {
        let control = std::fs::read(directory.join("CM32L_CONTROL.ROM"))?;
        let pcm = std::fs::read(directory.join("CM32L_PCM.ROM"))?;
        Ok(doslet_mt32::Timeline::with_gm(
            &control,
            &pcm,
            rate,
            mode == Mode::Gm,
        )?)
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        if self.debugger.as_ref().is_some_and(|gdb| gdb.connected())
            && matches!(
                message,
                Message::RunPause
                    | Message::CpuMhz(_)
                    | Message::SynchronizationMode { .. }
                    | Message::SpeedTiersToggle
                    | Message::HeroesMenuIdleToggle
                    | Message::AdaptiveIdleToggle
                    | Message::IdleLoopsToggle(_)
                    | Message::VgaWaitsToggle(_)
                    | Message::V86RamWaitsToggle(_)
                    | Message::Step
                    | Message::Reset
                    | Message::Config(
                        crate::configurator::Edit::Apply
                            | crate::configurator::Edit::ImportSaves
                            | crate::configurator::Edit::Keyboard(_)
                    )
            )
        {
            self.notice =
                Some("GDB controls execution; detach before changing the machine.".into());
            self.log_errors();
            return Task::none();
        }
        let mouse_task = if matches!(message, Message::Reset) {
            self.release_mouse()
        } else {
            Task::none()
        };
        if matches!(
            message,
            Message::CpuMhz(_)
                | Message::SynchronizationMode { .. }
                | Message::AdaptiveIdleToggle
                | Message::HeroesMenuIdleToggle
                | Message::IdleObservationToggle
        ) {
            self.disable_speed_tiers();
        }
        match message {
            Message::CpuProfile(edit) => self.edit_cpu_profile(edit),
            Message::Cpu(edit) => self.edit_cpu(edit),
            Message::SpeedTiersToggle => {
                if !self.configurator.pending
                    && let Some(tiers) = &mut self.speed_tiers
                {
                    if tiers.stats.enabled {
                        tiers.disable(&mut self.session);
                    } else {
                        tiers.enable(&mut self.session);
                    }
                    self.anchor = None;
                }
            }
            Message::SynchronizationMode { enabled, adaptive } => {
                if !self.configurator.pending {
                    self.launch.adaptive_cpu = enabled && adaptive;
                    self.launch.analyze_synchronization = enabled;
                    self.launch.adaptive_idle = false;
                    self.launch.heroes_menu_idle = false;
                    self.launch.heroes_timer_waits = false;
                    self.launch.observe_idle = false;
                    self.session.set_adaptive_idle(false);
                    self.session.set_idle_observation(false);
                    self.session.set_synchronization_analysis(enabled);
                    if enabled && adaptive {
                        self.session.set_adaptive_cpu(true);
                    }
                }
            }
            Message::AdaptiveIdleToggle => {
                if !self.gdb_connected() && !self.configurator.pending {
                    self.launch.adaptive_idle = !self.session.adaptive_idle_enabled();
                    self.launch.heroes_menu_idle = false;
                    self.launch.observe_idle = true;
                    self.session.set_adaptive_idle(self.launch.adaptive_idle);
                }
            }
            Message::IdleObservationToggle => {
                if !self.configurator.pending {
                    self.launch.observe_idle = !self.session.idle_observation().enabled;
                    self.session.set_idle_observation(self.launch.observe_idle);
                    if !self.launch.observe_idle {
                        self.launch.adaptive_idle = false;
                    }
                }
            }
            Message::HeroesMenuIdleToggle => {
                if !self.gdb_connected() && !self.configurator.pending {
                    self.launch.heroes_menu_idle =
                        self.session.adaptive_idle_enabled() || !self.session.menu_idle_enabled();
                    self.launch.adaptive_idle = false;
                    crate::native_input::set_heroes_menu_idle(
                        &mut self.session,
                        self.launch.heroes_menu_idle,
                    );
                }
            }
            Message::IdleLoopsToggle(enabled) => {
                if !self.configurator.pending {
                    self.session.set_idle_loops(enabled);
                }
            }
            Message::VgaWaitsToggle(enabled) => {
                if !self.configurator.pending {
                    self.session.set_vga_wait_acceleration(enabled);
                }
            }
            Message::V86RamWaitsToggle(enabled) => {
                if !self.configurator.pending {
                    self.session.set_v86_ram_waits(enabled);
                    self.anchor = None;
                }
            }
            Message::V86VgaPollingToggle(enabled) => {
                if !self.configurator.pending {
                    self.session.set_v86_vga_polling(enabled);
                }
            }
            Message::V86MpuPollingToggle(enabled) => {
                if !self.configurator.pending {
                    self.session.set_v86_mpu_polling(enabled);
                }
            }
            Message::V86InputPollingToggle(enabled) => {
                if !self.configurator.pending {
                    self.session.set_v86_input_polling(enabled);
                }
            }
            Message::CpuMhz(mhz) => {
                if self.configurator.pending {
                    return Task::none();
                }
                match self.session.set_cpu_mhz(mhz) {
                    Ok(()) => {
                        self.configurator.active.hardware.cpu_mhz = mhz;
                        self.configurator.draft.hardware.cpu_mhz = mhz;
                        self.launch.hardware.cpu_mhz = mhz;
                        if let Some(spec) = &mut self.launch.machine_spec {
                            spec.hardware.cpu_mhz = mhz;
                        }
                    }
                    Err(error) => self.notice = Some(error.to_string()),
                }
            }
            Message::Ui(action) => match action {
                crate::ui::Action::Open(panel) => {
                    if panel == crate::ui::Panel::Cpu {
                        self.workspace.cpu = Some(self.cpu_settings());
                    }
                    self.workspace.panel = Some(panel);
                    self.configurator.open = false;
                    return self.release_mouse();
                }
                crate::ui::Action::Close => {
                    self.workspace.panel = None;
                    self.configurator.open = false;
                }
                crate::ui::Action::DismissNotice => self.notice = None,
                crate::ui::Action::AudioPage(page) => self.workspace.audio_page = page,
                crate::ui::Action::StatusBar(mode) => self.workspace.status_bar = mode,
            },
            Message::Config(edit) => {
                use crate::configurator::Edit;
                let keep_panel = match self.workspace.panel {
                    Some(crate::ui::Panel::Audio) => edit.audio_panel_edit(),
                    Some(crate::ui::Panel::Drives) => edit.drives_panel_edit(),
                    Some(crate::ui::Panel::Input) => edit.input_panel_edit(),
                    Some(crate::ui::Panel::Memory) => matches!(edit, Edit::Memory(_)),
                    _ => false,
                };
                if !keep_panel {
                    self.workspace.panel = None;
                }
                if self.configurator.pending && !matches!(edit, Edit::Toggle) {
                    return Task::none();
                }
                self.configurator.edit(&edit);
                match edit {
                    Edit::Keyboard(at) => {
                        let mode = if at {
                            doslet_config::KeyboardMode::At
                        } else {
                            doslet_config::KeyboardMode::Bios
                        };
                        if let Err(error) = self.session.set_keyboard_mode(mode) {
                            self.configurator.draft.hardware.keyboard =
                                self.configurator.active.hardware.keyboard;
                            self.notice = Some(error.to_string());
                        } else {
                            self.configurator.active.hardware.keyboard = mode;
                            self.launch.hardware.keyboard = mode;
                            if let Some(spec) = &mut self.launch.machine_spec {
                                spec.hardware.keyboard = mode;
                            }
                            self.agent_modifiers = 0;
                        }
                        return self.release_mouse();
                    }
                    Edit::HideCursor(hidden) => {
                        self.configurator.active.input.hide_cursor = hidden;
                        if let Some(spec) = &mut self.launch.machine_spec {
                            spec.input.hide_cursor = hidden;
                        }
                    }
                    Edit::MouseMode(mode) => {
                        self.configurator.active.input.mouse_mode = mode;
                        if let Some(spec) = &mut self.launch.machine_spec {
                            spec.input.mouse_mode = mode;
                        }
                        let release = self.release_mouse();
                        return Task::batch([release, self.follow_mouse_mode()]);
                    }
                    Edit::ApplyAudio => {
                        if self.configurator.audio_only_changes() {
                            let audio = self.configurator.draft.hardware.audio();
                            match self.session.configure_unused_audio(audio) {
                                Ok(()) => {
                                    self.configurator.active.hardware =
                                        self.configurator.active.hardware.with_audio(audio);
                                    self.launch.hardware = self.launch.hardware.with_audio(audio);
                                    if let Some(spec) = &mut self.launch.machine_spec {
                                        spec.hardware = spec.hardware.with_audio(audio);
                                    }
                                    self.configurator.message =
                                        Some("Audio hardware updated without restarting.".into());
                                }
                                Err(error) => self.configurator.message = Some(error.to_string()),
                            }
                        }
                    }
                    Edit::Apply => return self.prepare_machine(self.configurator.draft.clone()),
                    Edit::LoadProfileFile => {
                        use clap::Parser;
                        let path = self.configurator.profile_path.clone();
                        match crate::cli::Cli::try_parse_from(["doslet", "--config", &path])
                            .map_err(|e| e.to_string())
                            .and_then(|cli| cli.resolve().map_err(|e| e.to_string()))
                        {
                            Ok(cli) => {
                                self.configurator.draft =
                                    cli.launch.machine_spec.unwrap_or_default();
                                self.configurator.edit(&Edit::ExportProfile);
                            }
                            Err(e) => self.configurator.message = Some(e),
                        }
                    }
                    Edit::SaveProfileFile => {
                        let text = crate::machine_profile::export_machine_profile(
                            &self.configurator.draft,
                        );
                        let path = std::path::Path::new(&self.configurator.profile_path);
                        self.configurator.message = Some(
                            match path
                                .parent()
                                .filter(|parent| !parent.as_os_str().is_empty())
                                .map_or(Ok(()), std::fs::create_dir_all)
                                .and_then(|()| std::fs::write(path, text))
                            {
                                Ok(()) => "Profile saved.".into(),
                                Err(e) => e.to_string(),
                            },
                        );
                    }
                    Edit::ImportSaves => {
                        let path = std::path::PathBuf::from(&self.configurator.save_path);
                        return self.prepare_machine_with_import(
                            self.configurator.active.clone(),
                            Some(path),
                        );
                    }
                    Edit::ExportSaves => {
                        let path = std::path::PathBuf::from(&self.configurator.save_path);
                        self.configurator.message =
                            Some(if let Some(storage) = &mut self.storage {
                                storage.export(&mut self.session, &path).map_or_else(
                                    |e| e,
                                    |_| format!("Save data exported: {}", path.display()),
                                )
                            } else {
                                "Start a configured machine first.".into()
                            });
                    }
                    _ => {}
                }
                if self.configurator.open {
                    return self.release_mouse();
                }
            }
            Message::MachinePrepared(result) => self.finish_preparation(result),
            Message::DisplaySizing(message) => {
                let leaving = matches!(message, SizingMessage::Fullscreen(false));
                let task = self
                    .display_sizing
                    .update(message, &self.display)
                    .map(Message::DisplaySizing);
                return if leaving {
                    Task::batch([task, self.release_mouse()])
                } else {
                    task
                };
            }
            Message::ReleaseMouse => return self.release_mouse(),
            Message::PointerMoved(x, y) => {
                self.pointer_inside = true;
                if self.mouse_capture.is_none()
                    && !self.mouse_capture_pending
                    && !self.relative_mouse()
                    && self.session.status() == SessionStatus::Running
                    && !self.configurator.open
                    && self.workspace.panel.is_none()
                {
                    self.session.pointer_move(x, y);
                }
            }
            Message::PointerButton(button, pressed) => {
                if pressed
                    && self.session.status() == SessionStatus::Running
                    && self.mouse_capture.is_none()
                    && !self.mouse_capture_pending
                    && self.relative_mouse()
                    && !self.configurator.open
                    && self.workspace.panel.is_none()
                {
                    return self.request_capture();
                }
                if self.mouse_capture_pending {
                    return Task::none();
                }
                if (self.session.status() == SessionStatus::Running
                    && !self.configurator.open
                    && self.workspace.panel.is_none())
                    || !pressed
                {
                    self.host_pointer_button(button, pressed);
                }
            }
            Message::PointerLeft => {
                self.pointer_inside = false;
                if self.mouse_capture.is_none()
                    && !self.mouse_capture_pending
                    && !(self.debugger.as_ref().is_some_and(|gdb| gdb.connected())
                        && self.session.status() == SessionStatus::Paused)
                {
                    for button in 0..3 {
                        self.host_pointer_button(button, false);
                    }
                }
            }
            Message::CaptureWindow(id, generation) => {
                if generation == self.mouse_capture_generation
                    && self.mouse_capture_pending
                    && self.session.status() == SessionStatus::Running
                    && self.relative_mouse()
                {
                    self.mouse_capture = Some(id);
                    return iced::window::capture_mouse(id)
                        .map(move |event| Message::MouseCapture(id, generation, event));
                }
            }
            Message::MouseCapture(id, generation, event) => {
                if generation != self.mouse_capture_generation {
                    return Task::none();
                }
                use iced::window::MouseCaptureEvent;
                match event {
                    MouseCaptureEvent::Acquired => {
                        if !self.mouse_capture_pending
                            || self.session.status() != SessionStatus::Running
                            || !self.relative_mouse()
                        {
                            return iced::window::release_mouse(id).map(|()| Message::PointerLeft);
                        }
                        self.mouse_capture_pending = false;
                        self.mouse_capture = Some(id);
                    }
                    MouseCaptureEvent::Motion { dx, dy } => {
                        if self.mouse_capture == Some(id)
                            && self.session.status() == SessionStatus::Running
                            && self.relative_mouse()
                        {
                            self.session.pointer_relative(dx, dy);
                        }
                    }
                    MouseCaptureEvent::Button { button, pressed } => {
                        if self.mouse_capture == Some(id) && (!pressed || self.relative_mouse()) {
                            self.host_pointer_button(button, pressed);
                        }
                    }
                    MouseCaptureEvent::Released => {
                        if self.mouse_capture == Some(id) || self.mouse_capture_pending {
                            self.mouse_capture = None;
                            self.mouse_capture_pending = false;
                            return self.release_mouse();
                        }
                    }
                    MouseCaptureEvent::Failed(error) => {
                        self.notice = Some(format!("Mouse capture unavailable: {error}"));
                        return self.release_mouse();
                    }
                }
            }
            Message::Keyboard(event) => {
                if (self.configurator.open || self.workspace.panel.is_some())
                    && matches!(
                        &event,
                        iced::keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                            ..
                        }
                    )
                {
                    self.configurator.open = false;
                    self.workspace.panel = None;
                    return self.release_mouse();
                }
                if (self.mouse_capture.is_some() || self.mouse_capture_pending)
                    && matches!(
                        &event,
                        iced::keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                            modifiers,
                            ..
                        } if modifiers.control()
                    )
                {
                    return self.release_mouse();
                }
                if self.configurator.active.hardware.keyboard == doslet_config::KeyboardMode::At {
                    self.track_physical_key(
                        &event,
                        self.session.status() == SessionStatus::Running
                            && !self.configurator.open
                            && self.workspace.panel.is_none()
                            && !self.configurator.pending,
                    );
                    return Task::none();
                }
                if self.session.status() == SessionStatus::Running
                    && !self.configurator.open
                    && self.workspace.panel.is_none()
                {
                    let (modifiers, key) = crate::keyboard::translate(&event);
                    self.host_modifiers = modifiers;
                    if let Err(error) = self
                        .session
                        .set_key_modifiers(modifiers | self.agent_modifiers)
                        .and_then(|()| {
                            if let Some((scan, ascii)) = key {
                                self.session.push_key(scan, ascii).map(|_| ())
                            } else {
                                Ok(())
                            }
                        })
                    {
                        self.notice = Some(error.to_string());
                    }
                }
            }
            Message::KeyboardCaptured(event) => {
                if self.configurator.active.hardware.keyboard == doslet_config::KeyboardMode::At {
                    self.track_physical_key(&event, false);
                }
            }
            Message::RefreshMidi => match crate::native_midi::ports() {
                Ok(ports) => {
                    self.midi_choices = crate::native_synth::choices();
                    if self.mt32_rom_dir.is_some() {
                        self.midi_choices.push("CM-32L / MT-32".into());
                    }
                    self.midi_choices.extend(ports.names);
                }
                Err(error) => self.notice = Some(error),
            },
            Message::MidiModeSelected(mode) => {
                let previous = self.midi_mode;
                self.midi_mode = mode;
                self.routing.set_mode(mode);
                let selection = self.midi_choice.clone();
                if let Err(error) = self.reselect_midi(&selection) {
                    self.midi_mode = previous;
                    self.routing.set_mode(previous);
                    self.notice = Some(error);
                }
                self.configurator.remember_midi(
                    self.midi_mode.as_str(),
                    self.midi_output_mode.as_str(),
                    &self.midi_choice,
                );
            }
            Message::MidiOutputModeSelected(mode) => {
                let previous = self.midi_output_mode;
                self.midi_output_mode = mode;
                if self.host_midi.is_some() {
                    let selection = self.midi_choice.clone();
                    if let Err(error) = self.reselect_midi(&selection) {
                        self.midi_output_mode = previous;
                        self.notice = Some(error);
                    }
                }
                self.configurator.remember_midi(
                    self.midi_mode.as_str(),
                    self.midi_output_mode.as_str(),
                    &self.midi_choice,
                );
            }
            Message::MidiSelected(selection) => {
                if let Err(error) = self.select_midi(&selection) {
                    self.notice = Some(error);
                }
                self.configurator.remember_midi(
                    self.midi_mode.as_str(),
                    self.midi_output_mode.as_str(),
                    &self.midi_choice,
                );
            }
            Message::AudioVolume(volume) => {
                if self.audio_mixer == crate::audio::Mixer::Application {
                    self.audio_volume = volume.min(100);
                    if let Some(audio) = &self.audio {
                        audio.set_volume(self.audio_volume);
                    }
                }
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Message::AudioMixerSelected(mixer) => {
                if let Err(error) = self.select_audio_mixer(mixer) {
                    self.notice = Some(format!("Audio mixer: {error}"));
                }
            }
            #[cfg(test)]
            Message::ArchiveLoaded(result) => match result {
                Ok(Some(bytes)) => match crate::archive::GameArchive::new(bytes) {
                    Ok(archive) => {
                        self.program =
                            (archive.programs.len() == 1).then(|| archive.programs[0].clone());
                        self.archive = Some(archive);
                        self.notice = None;
                    }
                    Err(error) => self.notice = Some(error),
                },
                Ok(None) => {}
                Err(error) => self.notice = Some(error),
            },
            #[cfg(test)]
            Message::ProgramSelected(program) => self.program = Some(program),
            #[cfg(test)]
            Message::StartArchive => {
                if let (Some(archive), Some(program)) = (&self.archive, &self.program) {
                    match archive.load(program) {
                        Ok(mut session) => {
                            session.start();
                            self.session = session.into();
                            self.name = program.rsplit('/').next().unwrap().into();
                            self.active_archive = Some((archive.clone(), program.clone()));
                            self.clear_presentation();
                        }
                        Err(error) => self.notice = Some(error),
                    }
                }
            }
            Message::McpToggle => {
                if let Some(mut computer) = self.computer.take() {
                    computer.cancel(self, "MCP control disabled");
                } else if let Err(error) = self.enable_mcp("127.0.0.1:9002".parse().unwrap()) {
                    self.notice = Some(format!("Cannot enable MCP: {error}"));
                }
            }
            Message::GdbToggle => {
                if let Some(mut debugger) = self.debugger.take() {
                    debugger.disable(&mut self.session);
                    self.anchor = None;
                } else {
                    match doslet_debugger::Debugger::listen("127.0.0.1:9001".parse().unwrap()) {
                        Ok(debugger) => self.debugger = Some(debugger),
                        Err(error) => self.notice = Some(format!("Cannot enable GDB: {error}")),
                    }
                }
            }
            Message::RunPause => {
                if self.configurator.pending {
                    return Task::none();
                }
                self.save_core_profile();
                if self.session.status() == SessionStatus::Running {
                    self.session.pause();
                } else {
                    self.session.start();
                }
                self.anchor = None;
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(audio) = &self.audio {
                    audio.set_active(self.session.status() == SessionStatus::Running);
                }
            }
            Message::Step => {
                if self.configurator.pending {
                    return Task::none();
                }
                self.session.capture_audio_sources(
                    self.audio
                        .as_ref()
                        .is_some_and(|a| a.mixer() == crate::audio::Mixer::System)
                        || (self.audio_deferred && self.audio_mixer == crate::audio::Mixer::System),
                );
                match self.audio_engine.ready() {
                    Ok(true) => {
                        self.session.single_step();
                        if let Err(error) = self.process_audio() {
                            self.notice = Some(error);
                        }
                    }
                    Ok(false) => {
                        self.notice = Some(
                            "Audio queue full; resume playback before stepping further".into(),
                        );
                        self.session.pause();
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        self.session.pause();
                    }
                }
            }
            Message::Reset => {
                if self.configurator.pending {
                    return Task::none();
                }
                if self
                    .configurator
                    .active
                    .mounts
                    .iter()
                    .any(|m| m.kind == doslet_config::SourceKind::HttpsIso)
                {
                    return self.prepare_machine(self.configurator.active.clone());
                }
                self.reset();
            }
            Message::FocusLost => {
                self.pointer_inside = false;
                let _ = self.session.release_keys();
                return self.release_mouse();
            }
            Message::CloseRequested(id) => {
                self.save_core_profile();
                self.save_autosave();
                return iced::window::close(id);
            }
            Message::ExitRequested => {
                self.session.pause();
                self.presentation_revision = self.presentation_revision.wrapping_add(1);
                self.save_core_profile();
                self.save_autosave();
                return iced::exit();
            }
            Message::ReturnToLibrary => return self.return_to_library(true),
            Message::ShutDown => return self.return_to_library(false),
            Message::LibraryReady => {}
            Message::Tick => self.tick(),
            Message::GamepadsReady => self.poll_gamepads(),
        }
        self.poll_audio_error();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            audio.set_active(self.session.status() == SessionStatus::Running);
        }
        if let Some(output) = &mut self.host_midi {
            match output.set_active(self.session.status() == SessionStatus::Running) {
                Ok(true) => {
                    let mut replay = Vec::with_capacity(SNAPSHOT_CAPACITY);
                    self.routing.replay(self.midi_output_mode, &mut |message| {
                        replay.extend_from_slice(message)
                    });
                    let count = replay.len();
                    if let Err(error) = output.push(self.session.emulated_nanos(), &replay[..count])
                    {
                        self.notice = Some(error);
                        self.session.pause();
                        let _ = output.set_active(false);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    self.notice = Some(error);
                    self.session.pause();
                }
            }
        }
        self.log_errors();
        self.midi_activity.observe(
            self.session.emulated_nanos(),
            self.session.midi_stats().1,
            self.routing.input_note_events(),
        );
        if !self.worker_owned {
            self.display.refresh(&self.session);
        }
        // Auto mode follows the guest with hysteresis. A switch to capturing
        // grabs the pointer at once when it is over the game picture, instead
        // of waiting for a click; a pointer the player released stays free
        // until the mode switches again.
        self.relative_latch.observe(
            self.session.pointer_relative_last_read(),
            self.session.emulated_nanos(),
        );
        let mouse_task = Task::batch([mouse_task, self.follow_mouse_mode()]);
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.session.status() != SessionStatus::Running {
                let release = if self.debugger.as_ref().is_some_and(|gdb| gdb.connected()) {
                    self.release_capture()
                } else {
                    self.release_mouse()
                };
                Task::batch([mouse_task, release])
            } else if (self.mouse_capture.is_some() || self.mouse_capture_pending)
                && !self.relative_mouse()
            {
                for button in 0..3 {
                    self.host_pointer_button(button, false);
                }
                let release = self.release_capture();
                Task::batch([mouse_task, release])
            } else {
                mouse_task
            }
        }
    }
    fn prepare_output(
        spec: &doslet_config::MachineSpec,
        backend: crate::audio::Backend,
        mixer: crate::audio::Mixer,
    ) -> Result<PreparedOutput, String> {
        let mut output = Self::new();
        output.audio_backend = backend;
        output.audio_mixer = mixer;
        output.audio_deferred = spec.output.audio;
        output.midi_mode = if spec.output.game_midi == "mt32" {
            Mode::Mt32
        } else {
            Mode::Gm
        };
        output.midi_output_mode = if spec.output.host_midi == "mt32" {
            Mode::Mt32
        } else {
            Mode::Gm
        };
        output.routing.set_mode(output.midi_mode);
        output.mt32_rom_dir = spec
            .output
            .rom_binding
            .as_ref()
            .map(|id| std::path::PathBuf::from(spec.binding(id)))
            .or_else(|| spec.output.audio.then(crate::native_roms::detect).flatten());
        if output.mt32_rom_dir.is_some() {
            output.midi_choices.push("CM-32L / MT-32".into());
        }
        if spec.output.audio {
            let selection = match spec.output.midi_output.as_str() {
                "rust" => "Rust synth",
                "apple" => APPLE_LABEL,
                "moont" => "CM-32L / MT-32",
                other => other,
            };
            output.select_midi(selection)?;
        }
        Ok(PreparedOutput {
            deferred: output.audio_deferred,
            audio: output.audio,
            host_midi: output.host_midi,
            rom_dir: output.mt32_rom_dir,
            audio_engine: output.audio_engine,
            midi_mode: output.midi_mode,
            output_mode: output.midi_output_mode,
            choice: output.midi_choice,
            choices: output.midi_choices,
            routing: output.routing,
        })
    }
    fn prepare_machine(&mut self, spec: doslet_config::MachineSpec) -> Task<Message> {
        self.prepare_machine_with_import(spec, None)
    }
    fn prepare_machine_with_import(
        &mut self,
        spec: doslet_config::MachineSpec,
        import_path: Option<std::path::PathBuf>,
    ) -> Task<Message> {
        if self.configurator.pending {
            return Task::none();
        }
        let plan = match spec.resolve() {
            Ok(plan) => plan,
            Err(e) => {
                self.configurator.message = Some(e.to_string());
                return Task::none();
            }
        };
        if let Some(storage) = &mut self.storage
            && let Err(e) = storage.service(&mut self.session)
        {
            self.configurator.message = Some(e);
            return Task::none();
        }
        if self.session.storage_waiting() || self.session.pending_commit() {
            self.configurator.message =
                Some("Wait for pending storage operations before restarting.".into());
            return Task::none();
        }
        if self.session.dirty_overlay()
            && !self.launch.persist
            && !self.configurator.discard_changes
        {
            self.configurator.message = Some(
                "Export saves or select Discard unsaved temporary changes before restarting."
                    .into(),
            );
            return Task::none();
        }
        if let Some(path) = &self.source
            && let Err(e) = crate::native_input::flush_writes(&mut self.session, path, &self.launch)
        {
            self.configurator.message = Some(e.to_string());
            return Task::none();
        }
        let output = match Self::prepare_output(&spec, self.audio_backend, self.audio_mixer) {
            Ok(output) => output,
            Err(e) => {
                self.configurator.message = Some(e);
                return Task::none();
            }
        };
        self.save_core_profile();
        let mut settings = self.launch.clone();
        if spec.game.executable.is_empty() && !spec.game.patches.is_empty() {
            self.configurator.message = Some("game.patches requires an executable".into());
            return Task::none();
        }
        settings.hardware = plan.hardware;
        if let Some(host) = spec.dpmi {
            settings.dpmi = crate::native_input::dpmi_mode(host);
        }
        settings.machine_spec = Some(spec.clone());
        settings.command_tail = plan.command_tail;
        settings.patches = spec
            .game
            .patches
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        settings.working_directory = spec.game.working_directory.as_bytes().to_vec();
        settings.root = None;
        settings.cdrom = None;
        for mount in &spec.mounts {
            match mount.kind {
                doslet_config::SourceKind::BrowserZip => {
                    settings.writable = mount.policy != doslet_config::WritePolicy::ReadOnly;
                    settings.persist = mount.policy == doslet_config::WritePolicy::LegacyWriteback;
                }
                doslet_config::SourceKind::NativeDirectory => {
                    settings.root = Some(std::path::PathBuf::from(spec.binding(&mount.source)));
                    settings.writable = mount.policy != doslet_config::WritePolicy::ReadOnly;
                    settings.persist = mount.policy == doslet_config::WritePolicy::LegacyWriteback;
                }
                doslet_config::SourceKind::LocalIso => {
                    settings.cdrom = Some((
                        mount.letter,
                        std::path::PathBuf::from(spec.binding(&mount.source)),
                    ))
                }
                doslet_config::SourceKind::HttpsIso => {}
            }
        }
        self.pending_output = Some(Box::new(output));
        self.preparation_was_running = self.session.status() == SessionStatus::Running;
        self.session.pause();
        self.anchor = None;
        if let Some(audio) = &self.audio {
            audio.set_active(false);
        }
        if let Some(output) = &mut self.host_midi {
            let _ = output.set_active(false);
        }
        self.configurator.pending = true;
        self.configurator.message = Some("Preparing machine and sources…".into());
        let old_storage = self.storage.take();
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = sender.send(Self::prepare_candidate(
                spec,
                settings,
                old_storage,
                import_path,
            ));
        });
        let task = Task::perform(
            async move {
                receiver
                    .await
                    .map(|r| std::sync::Arc::new(std::sync::Mutex::new(Some(r))))
                    .map_err(|_| "Machine preparation worker stopped.".into())
            },
            Message::MachinePrepared,
        );
        Task::batch([self.release_mouse(), task])
    }
    fn prepare_candidate(
        spec: doslet_config::MachineSpec,
        settings: crate::native_input::LaunchSettings,
        mut old_storage: Option<crate::native_storage::NativeStorage>,
        import_path: Option<std::path::PathBuf>,
    ) -> MachinePreparation {
        let mut settings = settings;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_, String> {
                let path = (!spec.game.executable.is_empty())
                    .then(|| std::path::PathBuf::from(&spec.game.executable));
                // A cached profile belongs to one executable; never reuse the last game's.
                if settings.core_profile_cache.is_some() {
                    settings.core_profile = None;
                    if let Some(path) = &path {
                        crate::profile::select_core_profile(&mut settings, path);
                    }
                }
                let mut session = match &path {
                    Some(path) => crate::native_zip::load_game_with_settings(path, &settings)
                        .map_err(|e| e.to_string())?,
                    None => {
                        let mut session = Session::demo_configured(settings.hardware)
                            .map_err(|e| e.to_string())?;
                        session.set_dynamic_core(!settings.no_dynamic_core);
                        if let Some(path) = &settings.core_profile {
                            crate::native_input::load_core_profile(&mut session, path);
                        }
                        session
                    }
                };
                let mut storage = if let Some(old) = &mut old_storage {
                    old.prepare_replacement_mut(&mut session, &settings)
                } else {
                    crate::native_storage::NativeStorage::prepare_mut(&mut session, &settings)
                }
                .map_err(|e| e.to_string())?;
                if let Some(path) = import_path {
                    storage.import(&mut session, &path)?;
                }
                Ok((
                    Box::new(PreparedMachine {
                        session,
                        source: path,
                        launch: settings,
                        spec,
                    }),
                    storage,
                ))
            }))
            .unwrap_or_else(|_| Err("Machine preparation worker failed.".into()));
        MachinePreparation {
            old_storage,
            result,
        }
    }
    fn finish_preparation(&mut self, result: Result<PreparationSlot, String>) {
        self.configurator.pending = false;
        let prepared = result.and_then(|slot| {
            slot.lock()
                .map_err(|_| "Machine preparation result unavailable.".to_string())
                .and_then(|mut slot| {
                    slot.take()
                        .ok_or_else(|| "Machine preparation result already consumed.".into())
                })
        });
        match prepared {
            Ok(prepared) => {
                self.storage = prepared.old_storage;
                match prepared.result {
                    Ok((candidate, storage)) => {
                        let mut candidate = *candidate;
                        let Some(output) = self.pending_output.take() else {
                            self.configurator.message =
                                Some("Prepared host output unavailable.".into());
                            return;
                        };
                        let output = *output;
                        if candidate.spec.input.pad_id == self.configurator.active.input.pad_id
                            && candidate.spec.input.pad_index
                                == self.configurator.active.input.pad_index
                        {
                            self.seed_pad(&mut candidate.session);
                        } else {
                            self.pad_generation = None;
                            self.pad_axes = [0; 2];
                            self.pad_buttons = [false; 2];
                        }
                        let _ = candidate.session.release_keys();
                        candidate.session.start();
                        self.session = candidate.session.into();
                        self.speed_tiers = None;
                        self.restore_state = None;
                        self.dynamic_error_reported = false;
                        self.source = candidate.source;
                        self.launch = candidate.launch;
                        self.storage = Some(storage);
                        self.audio_deferred = output.deferred;
                        self.audio_engine = output.audio_engine;
                        self.audio = output.audio;
                        self.apply_audio_volume();
                        self.host_midi = output.host_midi;
                        self.mt32_rom_dir = output.rom_dir;
                        self.midi_mode = output.midi_mode;
                        self.midi_output_mode = output.output_mode;
                        self.midi_choice = output.choice;
                        self.midi_choices = output.choices;
                        self.routing = output.routing;
                        self.midi_activity = crate::midi_activity::MidiActivity::default();
                        self.display = DisplayFrame::default();
                        self.anchor = None;
                        self.notice = None;
                        self.name = if candidate.spec.name.is_empty() {
                            "Configured machine".into()
                        } else {
                            candidate.spec.name.clone()
                        };
                        self.configurator.set_active(candidate.spec);
                        self.display_sizing
                            .set_output(&self.configurator.active.output);
                        self.configurator.open = false;
                        self.workspace.panel = None;
                        self.configurator.message = Some("Machine applied and restarted.".into());
                    }
                    Err(error) => {
                        self.pending_output = None;
                        if self.preparation_was_running {
                            self.session.start();
                        }
                        self.anchor = None;
                        self.notice = Some(error.clone());
                        self.configurator.message = Some(error);
                    }
                }
            }
            Err(error) => {
                self.pending_output = None;
                if self.preparation_was_running {
                    self.session.start();
                }
                self.anchor = None;
                self.notice = Some(error.clone());
                self.configurator.message = Some(error);
            }
        }
    }
    fn release_mouse(&mut self) -> Task<Message> {
        if self.computer.is_some() {
            let _ = self
                .session
                .release_input_source(doslet_input::InputSourceId(0));
            let _ = self
                .session
                .release_input_source(doslet_input::InputSourceId(1));
        } else {
            let _ = self.session.release_keys();
        }
        self.host_modifiers = 0;
        if self.agent_modifiers != 0 {
            let _ = self.session.set_key_modifiers(self.agent_modifiers);
        }
        for button in 0..3 {
            self.host_pointer_button(button, false);
        }
        self.release_capture()
    }
    fn release_capture(&mut self) -> Task<Message> {
        if self.mouse_capture.is_some() || self.mouse_capture_pending {
            self.mouse_capture_generation = self.mouse_capture_generation.wrapping_add(1);
        }
        self.mouse_capture_pending = false;
        self.mouse_capture.take().map_or_else(Task::none, |id| {
            iced::window::release_mouse(id).map(|()| Message::Tick)
        })
    }
    fn log_errors(&mut self) {
        #[cfg(feature = "wasm-procedures")]
        if let Some(stats) = self.session.wasm_procedure_stats()
            && stats.calls != 0
            && self.wasm_reported_epoch != Some(self.session.epoch())
        {
            if stats.interpreted_calls != 0 {
                eprintln!(
                    "[INFO doslet] Interpreter procedure boundaries executing: {} calls, {} instructions",
                    stats.interpreted_calls, stats.interpreted_instructions
                );
            } else {
                eprintln!(
                    "[INFO doslet] AOT-WASM executing: {} calls, {} translated instructions; shared native RAM",
                    stats.calls, stats.translated_instructions
                );
            }
            self.wasm_reported_epoch = Some(self.session.epoch());
        }
        let fault = self.session.fault();
        if fault != self.logged_fault {
            self.presentation_revision = self.presentation_revision.wrapping_add(1);
            if let Some(error) = fault {
                if let Some(detail) = self.session.v86_last_error() {
                    eprintln!("[ERROR doslet] v86: {detail}");
                }
                #[cfg(feature = "wasm-procedures")]
                if let Some(detail) = self.session.wasm_procedure_last_error() {
                    eprintln!("[ERROR doslet] AOT-WASM: {detail}");
                }
                eprintln!("[ERROR doslet] {}: {error}", self.name);
            }
            self.logged_fault = fault;
        }
        if self.notice != self.logged_notice {
            self.presentation_revision = self.presentation_revision.wrapping_add(1);
            if let Some(error) = &self.notice {
                eprintln!("[ERROR doslet] {}: {error}", self.name);
            }
            self.logged_notice.clone_from(&self.notice);
        }
    }
    #[inline(never)]
    fn reset(&mut self) {
        self.save_core_profile();
        self.notice = None;
        if self.session.dirty_overlay()
            && !self.launch.persist
            && !self.configurator.discard_changes
        {
            self.configurator.open = true;
            self.configurator.message = Some(
                "Export saves or select Discard unsaved temporary changes before Reset.".into(),
            );
            return;
        }
        if let Some(storage) = &mut self.storage
            && let Err(error) = storage.service(&mut self.session)
        {
            self.notice = Some(error);
            return;
        }
        if self.session.storage_waiting() {
            self.notice = Some(
                "Save commit is pending; reset will be available after acknowledgement.".into(),
            );
            return;
        }
        if let Some(path) = &self.source
            && let Err(error) =
                crate::native_input::flush_writes(&mut self.session, path, &self.launch)
        {
            self.notice = Some(error.to_string());
            return;
        }
        #[cfg(test)]
        if self.active_archive.is_some() {
            self.reset_active_archive();
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // A snapshot selects the initial position, not the reset recipe.
            // Prefer restarting the configured executable when one is available.
            if self.source.is_some() {
                if !self.reset_source_game() {
                    return;
                }
            } else if let Some(path) = self.restore_state.clone() {
                if let Err(error) = self.install_snapshot(path) {
                    self.notice = Some(format!("Cannot restore snapshot: {error}"));
                    return;
                }
            } else {
                if !self.reset_demo_session() {
                    return;
                }
            }
        }
        if let Some(generation) = self.pad_generation {
            let _ = Self::route_pad(
                &mut self.session,
                generation,
                self.pad_sequence,
                self.pad_axes,
                self.pad_buttons,
            );
        }
        let _ = self.session.release_keys();
        self.clear_presentation();
    }
    #[cfg(test)]
    #[inline(never)]
    fn reset_active_archive(&mut self) {
        if let Some((archive, program)) = &self.active_archive {
            match archive.load(program) {
                Ok(mut session) => {
                    session.start();
                    self.seed_pad(&mut session);
                    let _ = session.release_keys();
                    self.session = session.into();
                    self.name = program.rsplit('/').next().unwrap().into();
                    self.clear_presentation();
                }
                Err(error) => self.notice = Some(error),
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(never)]
    fn reset_source_game(&mut self) -> bool {
        let Some(path) = &self.source else {
            return true;
        };
        match crate::native_zip::load_game_with_settings(path, &self.launch) {
            Ok(mut session) => {
                match if let Some(storage) = &mut self.storage {
                    storage.prepare_replacement_mut(&mut session, &self.launch)
                } else {
                    crate::native_storage::NativeStorage::prepare_mut(&mut session, &self.launch)
                } {
                    Ok(storage) => {
                        session.start();
                        self.session = session.into();
                        self.storage = Some(storage);
                        self.restore_state = None;
                        self.name = if self.configurator.active.name.is_empty() {
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        } else {
                            self.configurator.active.name.clone()
                        };
                        true
                    }
                    Err(error) => {
                        self.notice = Some(format!("Cannot reload saves: {error}"));
                        false
                    }
                }
            }
            Err(error) => {
                self.notice = Some(format!("Cannot reload game: {error}"));
                false
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[inline(never)]
    fn reset_demo_session(&mut self) -> bool {
        if let Err(error) = self.init_demo_session() {
            self.notice = Some(format!("Cannot restart demo: {error}"));
            return false;
        }
        self.session
            .set_synchronization_analysis(self.launch.analyze_synchronization);
        if self.launch.adaptive_cpu {
            self.session.set_adaptive_cpu(true);
        }
        true
    }
    /// Leaving drops the machine, so it needs Reset's guarantees: committed
    /// saves and no silently discarded temporary changes.
    fn return_to_library(&mut self, keep_state: bool) -> Task<Message> {
        if let Some(storage) = &mut self.storage
            && let Err(error) = storage.service(&mut self.session)
        {
            self.notice = Some(error);
            return Task::none();
        }
        if self.session.storage_waiting() || self.session.pending_commit() {
            self.notice = Some("Saving; return to the library after the commit finishes.".into());
            return Task::none();
        }
        // Without a suspended state, temporary disk changes would be lost.
        if (self.autosave_path.is_none() || !keep_state)
            && self.session.dirty_overlay()
            && !self.launch.persist
            && !self.configurator.discard_changes
        {
            self.configurator.open = true;
            self.configurator.message = Some(
                "Export saves or select Discard unsaved temporary changes before returning to the library."
                    .into(),
            );
            return Task::none();
        }
        self.session.pause();
        self.presentation_revision = self.presentation_revision.wrapping_add(1);
        self.save_core_profile();
        if keep_state {
            self.save_autosave();
        } else if let Some(path) = self.autosave_path.take() {
            // Taking the path also stops the worker from saving on exit.
            let _ = std::fs::remove_file(path);
        }
        Task::batch([self.release_mouse(), Task::done(Message::LibraryReady)])
    }
    pub(crate) fn save_core_profile(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = &self.launch.core_profile {
            crate::native_input::save_core_profile(&self.session, path);
        }
    }
    pub(crate) fn save_autosave(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = self.autosave_path.take() {
            if matches!(
                self.session.status(),
                SessionStatus::Exited(_) | SessionStatus::Faulted | SessionStatus::Halted
            ) {
                let _ = std::fs::remove_file(&path);
                return;
            }
            self.session.pause();
            if let Some(storage) = &mut self.storage {
                let deadline = Instant::now() + Duration::from_secs(2);
                while (self.session.storage_waiting() || self.session.pending_commit())
                    && Instant::now() < deadline
                {
                    let _ = storage.service(&mut self.session);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            if let Err(error) = crate::native_state::save(&path, &mut self.session) {
                eprintln!(
                    "[WARN doslet] Cannot save suspended state {}: {error}",
                    path.display()
                );
            }
        }
    }
    fn clear_presentation(&mut self) {
        if let Some(tiers) = &mut self.speed_tiers
            && tiers.stats.enabled
        {
            tiers.enable(&mut self.session);
        }
        self.dynamic_error_reported = false;
        self.notice = None;
        self.midi_mode = if self.configurator.active.output.game_midi == "mt32" {
            Mode::Mt32
        } else {
            Mode::Gm
        };
        self.routing = Routing::new(self.midi_mode);
        // Reset is a control barrier: the audio owner discards old batches and
        // queued PCM only after the replacement synth has been constructed.
        let config = self.audio_config(&self.midi_choice, self.audio_engine.rate(), false);
        if let Err(error) = config.and_then(|config| {
            self.audio_engine.configure(
                config,
                self.audio
                    .as_ref()
                    .map(|a| a.writer_with_reserve(self.session.v86_host_clock_enabled())),
            )
        }) {
            self.notice = Some(format!("Audio reset: {error}"));
            self.session.pause();
        }
        if let Some(audio) = &self.audio {
            audio.set_active(self.session.status() == SessionStatus::Running);
        }
        self.midi_activity = crate::midi_activity::MidiActivity::default();
        if let Some(output) = &mut self.host_midi
            && let Err(error) = output.reset(self.midi_output_mode)
        {
            self.notice = Some(error);
        }
        self.display = DisplayFrame::default();
        self.anchor = None;
    }
    fn track_physical_key(&mut self, event: &iced::keyboard::Event, enabled: bool) {
        if let Some((key, pressed)) = crate::keyboard::physical(event)
            && let Err(e) = self
                .session
                .key_event(doslet_input::PhysicalKey(key), pressed)
        {
            self.notice = Some(e.to_string());
        }
        if !enabled {
            let _ = self.session.release_keys();
        }
    }
    fn route_pad(
        session: &mut Session,
        generation: u64,
        sequence: u64,
        axes: [i16; 2],
        buttons: [bool; 2],
    ) -> Result<(), RuntimeError> {
        use doslet_input::{InputAction, InputEvent, InputSourceId};
        session.connect_input_joystick(InputSourceId(1), generation)?;
        let epoch = session.input_router_mut().epoch();
        for (index, action) in [
            InputAction::Axis {
                axis: 0,
                value: axes[0],
            },
            InputAction::Axis {
                axis: 1,
                value: axes[1],
            },
            InputAction::Button {
                button: 0,
                pressed: buttons[0],
            },
            InputAction::Button {
                button: 1,
                pressed: buttons[1],
            },
        ]
        .into_iter()
        .enumerate()
        {
            session.route_input(InputEvent {
                session_epoch: epoch,
                source: InputSourceId(1),
                connection_generation: generation,
                sequence: sequence + index as u64,
                action,
            })?;
        }
        Ok(())
    }
    fn seed_pad(&self, session: &mut Session) {
        if let Some(generation) = self.pad_generation {
            let _ = Self::route_pad(
                session,
                generation,
                self.pad_sequence,
                self.pad_axes,
                self.pad_buttons,
            );
            let _ = session.release_input_source(doslet_input::InputSourceId(1));
        }
    }
    fn poll_gamepads(&mut self) {
        use crate::native_gamepad::{GamepadUpdate, uuid_string};
        let Some(gamepads) = &mut self.gamepads else {
            return;
        };
        gamepads.acknowledge();
        for _ in 0..256 {
            let Some(update) = gamepads.poll() else {
                break;
            };
            self.presentation_revision = self.presentation_revision.wrapping_add(1);
            match update {
                GamepadUpdate::State(state) => {
                    let settings = &self.configurator.active.input;
                    if let Some(id) = &settings.pad_id {
                        if *id != uuid_string(state.uuid) {
                            continue;
                        }
                        if state.ambiguous {
                            let _ = self
                                .session
                                .release_input_source(doslet_input::InputSourceId(1));
                            self.notice=Some("Controller identity is ambiguous; choose a controller index explicitly.".into());
                            continue;
                        }
                    } else if usize::from(settings.pad_index) != state.id {
                        continue;
                    }
                    let axis = |index: u8| {
                        let value = state.axes.get(usize::from(index)).copied().unwrap_or(0);
                        if i32::from(value).abs() * 100
                            < i32::from(settings.deadzone_percent) * 32767
                        {
                            0
                        } else {
                            value
                        }
                    };
                    let button = |index: u8| index < 32 && state.buttons & (1u32 << index) != 0;
                    let axes = [axis(settings.axis_x), axis(settings.axis_y)];
                    let buttons = [button(settings.button_1), button(settings.button_2)];
                    match Self::route_pad(
                        &mut self.session,
                        state.generation,
                        state.sequence,
                        axes,
                        buttons,
                    ) {
                        Ok(()) => {}
                        Err(RuntimeError::Input(doslet_input::InputError::Stale)) => continue,
                        Err(error) => {
                            self.notice = Some(error.to_string());
                            continue;
                        }
                    }
                    self.pad_axes = axes;
                    self.pad_buttons = buttons;
                    self.pad_generation = Some(state.generation);
                    self.pad_sequence = state.sequence;
                    if self.session.status() != SessionStatus::Running
                        || self.configurator.open
                        || self.workspace.panel.is_some()
                        || state.resync
                    {
                        let _ = self
                            .session
                            .release_input_source(doslet_input::InputSourceId(1));
                    }
                }
                GamepadUpdate::Disconnected {
                    id,
                    uuid,
                    generation,
                    sequence,
                } => {
                    if self.configurator.active.input.pad_id.as_ref().map_or(
                        id == usize::from(self.configurator.active.input.pad_index),
                        |selected| *selected == uuid,
                    ) && self.pad_generation == Some(generation)
                    {
                        let epoch = self.session.input_router_mut().epoch();
                        let _ = self.session.route_input(doslet_input::InputEvent {
                            session_epoch: epoch,
                            source: doslet_input::InputSourceId(1),
                            connection_generation: generation,
                            sequence,
                            action: doslet_input::InputAction::ReleaseSource,
                        });
                        self.pad_axes = [0; 2];
                        self.pad_buttons = [false; 2];
                    }
                }
                GamepadUpdate::ReleaseAll => {
                    let _ = self
                        .session
                        .release_input_source(doslet_input::InputSourceId(1));
                }
                GamepadUpdate::Error(error) => {
                    self.notice = Some(format!("Controller unavailable: {error}"))
                }
                GamepadUpdate::Connected {
                    id,
                    name,
                    uuid,
                    generation,
                } => {
                    if self.configurator.active.input.pad_id.as_ref().map_or(
                        id == usize::from(self.configurator.active.input.pad_index),
                        |selected| *selected == uuid,
                    ) {
                        let _ = self
                            .session
                            .connect_input_joystick(doslet_input::InputSourceId(1), generation);
                        self.pad_generation = Some(generation);
                        self.pad_sequence = 0;
                    }
                    self.configurator
                        .controllers
                        .retain(|label| !label.starts_with(&format!("{id}:")));
                    self.configurator
                        .controllers
                        .push(format!("{id}: {name} · {uuid}"));
                }
            }
        }
    }
    fn tick(&mut self) {
        let now = Instant::now();
        self.tick_while(now, || now.elapsed() < MAX_WORK_PER_TICK);
    }
    #[cfg(test)]
    fn tick_at(&mut self, now: Instant) {
        self.tick_while(now, || true);
    }
    fn tick_while(&mut self, now: Instant, can_continue: impl FnMut() -> bool) {
        let mut debugger = self.debugger.take();
        if let Some(gdb) = &mut debugger {
            let before = (gdb.revision(), self.session.steps(), self.session.status());
            if !self.configurator.pending {
                let before = self.session.status();
                if let Err(error) = gdb.poll(&mut self.session) {
                    self.notice = Some(error.to_string());
                }
                if before != self.session.status() {
                    self.anchor = None;
                }
            }
            if gdb.connected() {
                self.tick_mode(now, can_continue, gdb);
                if self.session.status() != SessionStatus::Running {
                    self.anchor = None;
                }
            } else {
                self.tick_mode(now, can_continue, &mut doslet_debugger::Normal);
            }
            if before != (gdb.revision(), self.session.steps(), self.session.status()) {
                self.presentation_revision = self.presentation_revision.wrapping_add(1);
            }
            self.debugger = debugger;
            return;
        }
        self.tick_mode(now, can_continue, &mut doslet_debugger::Normal);
        self.debugger = debugger;
    }
    fn tick_mode<M: doslet_debugger::Execution>(
        &mut self,
        now: Instant,
        mut can_continue: impl FnMut() -> bool,
        execution: &mut M,
    ) {
        self.poll_gamepads();
        self.poll_audio_error();
        if self.configurator.pending {
            return;
        }
        // Completed host I/O can finalize guest registers. Keep those effects
        // queued while GDB is stopped; apply them after its resume command.
        if M::DEBUG && self.session.status() != SessionStatus::Running {
            return;
        }
        if let Some(storage) = &mut self.storage
            && let Err(error) = storage.service(&mut self.session)
        {
            self.notice = Some(error);
            self.session.pause();
        }
        if self.session.storage_waiting() {
            self.anchor = None;
            return;
        }

        if self.session.status() != SessionStatus::Running {
            return;
        }
        if self.anchor.is_none()
            && let Some(output) = &self.host_midi
        {
            output.start_clock(self.session.emulated_nanos(), now);
        }
        let (start, guest) = *self
            .anchor
            .get_or_insert((now, self.session.emulated_nanos()));
        let mut target = guest.saturating_add(
            now.duration_since(start)
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
        );
        let wall_target = target;
        if self.host_midi.is_some() {
            target = target.saturating_add(crate::native_midi::LOOKAHEAD.as_nanos() as u64);
        }
        let host_clock = self.session.v86_host_clock_enabled();
        let mut budget = if host_clock {
            4_000_000
        } else {
            MAX_STEPS_PER_TICK
        };
        // Two watermarks maintain a PCM reserve; never run over 80 ms ahead.
        // Only the opt-in experiment changes the existing pacing policy.
        if !M::DEBUG
            && (self.session.menu_idle_enabled()
                || self.session.synchronization_analysis().controller_enabled
                || self.speed_tiers.as_ref().is_some_and(|t| t.stats.enabled))
            && let Some(audio) = &self.audio
        {
            let refill = audio
                .refill_nanos()
                .saturating_sub(self.audio_engine.pending_nanos());
            if refill != 0 {
                target = target.max(
                    self.session
                        .emulated_nanos()
                        .saturating_add(refill)
                        .min(wall_target.saturating_add(80_000_000)),
                );
            }
        }
        let previous_memory_wait_steps = self.session.memory_wait_steps();
        let previous_v86_ram_wait_parks =
            self.session.v86_ram_waits().map_or(0, |(_, parks)| parks);
        let previous_v86_vga_throttles =
            self.session.v86_vga_polling().map_or(0, |(_, reads)| reads);
        let previous_v86_mpu_throttles =
            self.session.v86_mpu_polling().map_or(0, |(_, reads)| reads);
        let mut prepared = false;
        while budget != 0
            && self.session.status() == SessionStatus::Running
            && (host_clock || self.session.emulated_nanos() < target)
            && can_continue()
        {
            let epoch = self.session.epoch();
            if self.startup_keys_epoch != Some(epoch) {
                self.startup_keys_epoch = Some(epoch);
                self.startup_keys_index = 0;
            }
            if self.session.waiting_for_key() {
                while let Some(&ascii) = self
                    .configurator
                    .active
                    .game
                    .startup_keys
                    .as_bytes()
                    .get(self.startup_keys_index)
                {
                    let Some((scan, ascii)) = doslet_config::bios_key_for_ascii(ascii) else {
                        self.notice = Some("Invalid configured startup character".into());
                        self.session.pause();
                        return;
                    };
                    match self.session.push_key(scan, ascii) {
                        Ok(true) => self.startup_keys_index += 1,
                        Ok(false) => break,
                        Err(error) => {
                            self.notice = Some(format!("Startup keyboard input failed: {error}"));
                            self.session.pause();
                            return;
                        }
                    }
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            if self
                .audio
                .as_ref()
                .is_some_and(|audio| audio.available() < 512)
            {
                break;
            }
            self.session.capture_audio_sources(
                self.audio
                    .as_ref()
                    .is_some_and(|a| a.mixer() == crate::audio::Mixer::System)
                    || (self.audio_deferred && self.audio_mixer == crate::audio::Mixer::System),
            );
            match self.audio_engine.ready() {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    self.notice = Some(error);
                    self.session.pause();
                    break;
                }
            }
            if !M::DEBUG && !prepared {
                self.jit.maintain(&mut self.session);
                if self.maintain_tier_learning() {
                    // Profiling wall time is a pause, never guest catch-up time.
                    return;
                }
                prepared = true;
            }
            if !self.dynamic_error_reported
                && let Some(error) = self.session.dynamic_error()
            {
                self.dynamic_error_reported = true;
                self.notice = Some(format!(
                    "JIT preparation stopped: {error}. Interpreter fallback remains active."
                ));
            }
            if host_clock {
                let host_target = guest
                    .saturating_add(start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
                if host_target.saturating_sub(self.session.emulated_nanos()) > 1_000_000
                    && self.session.debug_cpu().accepts_maskable_interrupt()
                    && !self.session.debug_irq_pending()
                {
                    // Catch up idle device time, but first let the CPU service
                    // a queued IRQ: advancing several more PIT periods would
                    // merge their edges in the PIC and slow the guest clock.
                    if let Err(error) = self.session.advance_v86_host_time(host_target) {
                        self.notice = Some(error.to_string());
                        self.session.pause();
                        break;
                    }
                    if let Err(error) = self.process_audio() {
                        self.notice = Some(error);
                        self.session.pause();
                        break;
                    }
                    budget = budget.saturating_sub(1);
                    continue;
                }
            }
            let completed = match if !M::DEBUG
                && let Some(tiers) = &mut self.speed_tiers
                && (tiers.stats.enabled || tiers.has_pending_time())
            {
                if tiers.stats.enabled {
                    tiers
                        .advance(&mut self.session, budget.min(CHUNK), target)
                        .map_err(Into::into)
                } else {
                    tiers
                        .settle(&mut self.session)
                        .map(|()| 0)
                        .map_err(Into::into)
                }
            } else {
                execution.advance(
                    &mut self.session,
                    budget.min(if host_clock { 65_536 } else { CHUNK }),
                )
            } {
                Ok(completed) => completed,
                Err(error) => {
                    self.notice = Some(error.to_string());
                    self.session.debug_suspend();
                    break;
                }
            };
            budget = budget.saturating_sub(completed);
            if host_clock {
                let host_target = guest
                    .saturating_add(start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
                if let Err(error) = self.session.advance_v86_host_time(host_target) {
                    self.notice = Some(error.to_string());
                    self.session.pause();
                    break;
                }
            }
            if let Err(error) = self.process_audio() {
                self.notice = Some(error);
                self.session.pause();
                break;
            }
            if completed == 0 || self.session.timeslice_yielded() {
                break;
            }
        }
        if previous_memory_wait_steps == 0 && self.session.memory_wait_steps() != 0 {
            eprintln!(
                "[INFO doslet] {}: experimental timer waits active; batched {} logical steps",
                self.name,
                self.session.memory_wait_steps()
            );
        }
        if previous_v86_ram_wait_parks == 0
            && self
                .session
                .v86_ram_waits()
                .is_some_and(|(_, parks)| parks != 0)
        {
            eprintln!(
                "[INFO doslet] {}: v86 RAM wait detected at {:x}; host-clock parking active",
                self.name,
                self.session.debug_pc()
            );
        }
        if previous_v86_vga_throttles == 0
            && self
                .session
                .v86_vga_polling()
                .is_some_and(|(_, reads)| reads != 0)
        {
            eprintln!(
                "[INFO doslet] {}: v86 VGA status polling detected; host-clock pacing active",
                self.name
            );
        }
        if previous_v86_mpu_throttles == 0
            && self
                .session
                .v86_mpu_polling()
                .is_some_and(|(_, reads)| reads != 0)
        {
            eprintln!(
                "[INFO doslet] {}: v86 MPU-401 status polling detected; host-clock pacing active",
                self.name
            );
        }
    }
    fn process_audio(&mut self) -> Result<(), String> {
        if self.audio_deferred {
            let status = self.session.audio_status();
            if status.configured_format.is_some()
                || status.opl_started
                || self.session.midi_stats().1 != 0
            {
                // Start at the previous batch boundary, retaining this batch's
                // queued PCM and MIDI. A failed open needs an explicit retry.
                let start = match &self.audio_engine {
                    AudioEngine::Inline(renderer) => renderer.nanos,
                    AudioEngine::Threaded(_) => self.session.emulated_nanos(),
                };
                self.audio_deferred = false;
                let selection = self.midi_choice.clone();
                self.reselect_midi_at(&selection, Some(start))?;
            }
        }
        if !self.audio_engine.ready()? {
            return Err("Audio input queue full; resume playback before stepping further".into());
        }
        loop {
            let count = self.session.drain_pcm_timed(&mut self.pcm);
            if count == 0 {
                break;
            }
            if self
                .audio
                .as_ref()
                .is_none_or(|a| a.mixer() != crate::audio::Mixer::System)
            {
                self.audio_engine.receive_pcm(&self.pcm[..count])?;
            }
        }
        if self
            .audio
            .as_ref()
            .is_some_and(|a| a.mixer() == crate::audio::Mixer::System)
        {
            let mut frames = [doslet_pcm::SourceFrame::default(); 128];
            for fm in [false, true] {
                loop {
                    let count = self.session.drain_audio_source(fm, &mut frames);
                    if count == 0 {
                        break;
                    }
                    self.audio_engine.receive_source(fm, &frames[..count])?;
                }
            }
        }
        self.audio_engine
            .render_until(self.session.emulated_nanos(), self.host_midi.is_some())?;
        let count = self.session.drain_midi(&mut self.midi_bytes);
        let dialect = self.output_dialect();
        let mut bytes = [0u8; 16384];
        let mut used = 0;
        let mut overflow = false;
        for &byte in &self.midi_bytes[..count] {
            self.routing
                .push(byte, dialect, &mut |message| {
                    if used + message.len() > bytes.len() {
                        overflow = true;
                        return;
                    }
                    bytes[used..used + message.len()].copy_from_slice(message);
                    used += message.len();
                })
                .map_err(|error| format!("MIDI wire error: {error:?}"))?;
        }
        if overflow {
            return Err("Translated MIDI batch capacity exceeded".into());
        }
        if self.routing.mode() != self.midi_mode {
            self.midi_mode = self.routing.mode();
            if self.audio_engine.is_mt32() {
                let selection = self.midi_choice.clone();
                self.reselect_midi(&selection)?;
                used = 0; // Replay already includes this batch's device state and notes.
            }
        }
        if let Some(output) = &mut self.host_midi {
            output.push(self.session.emulated_nanos(), &bytes[..used])?;
        } else {
            self.audio_engine
                .push_midi(self.session.emulated_nanos(), &bytes[..used])?;
        }
        self.audio_engine.submit()?;
        if let Some(source) = &self.source {
            crate::native_input::flush_writes(&mut self.session, source, &self.launch)
                .map_err(|error| format!("C: RW writeback failed: {error}"))?;
        }
        Ok(())
    }
    pub fn audio_label(&self) -> String {
        let stats = self.audio_engine.stats();
        if let Some(output) = &self.host_midi {
            return format!("{} · SB {} nonzero frames", output.label(), stats.nonzero);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            let (frames, _, peak) = audio.stats();
            if self.audio_engine.is_mt32() {
                return format!(
                    "CM-32L · {} notes · {} SysEx · peak {:.2} · {} frames · SB {} nonzero frames",
                    stats.notes, stats.sysex, peak, frames, stats.nonzero
                );
            }
            return format!(
                "{} · {} Hz · {} MIDI notes · peak {:.2} · {} played frames · SB {} nonzero frames",
                self.midi_choice, audio.rate, stats.notes, peak, frames, stats.nonzero
            );
        }
        "Audio output inactive".into()
    }
    pub fn storage_label(&self) -> String {
        if self.configurator.pending {
            "Preparing machine and sources…".into()
        } else if self.session.pending_commit() {
            "Saving changes…".into()
        } else if self.session.storage_waiting() {
            "Waiting for storage…".into()
        } else if self.session.dirty_overlay() {
            "Unsaved changes".into()
        } else if let Some(revision) = self.session.committed_revision() {
            format!("Saved · revision {revision}")
        } else {
            "No unsaved changes".into()
        }
    }
    fn output_dialect(&self) -> Mode {
        if self.host_midi.is_some() {
            self.midi_output_mode
        } else if self.audio_engine.is_mt32() {
            self.midi_mode
        } else {
            Mode::Gm
        }
    }
    fn audio_config(
        &self,
        selection: &str,
        rate: u32,
        restore: bool,
    ) -> Result<AudioConfig, String> {
        let source = match selection {
            "Rust synth" => AudioSource::Rust,
            APPLE_LABEL => AudioSource::Apple,
            "CM-32L / MT-32" => AudioSource::Mt32 {
                directory: self
                    .mt32_rom_dir
                    .clone()
                    .ok_or("Use --mt32-rom-dir to load CM-32L ROMs")?,
                mode: self.midi_mode,
            },
            _ => AudioSource::External,
        };
        let mut replay = Vec::new();
        if restore && self.session.midi_stats().1 != 0 {
            replay.reserve(SNAPSHOT_CAPACITY);
            let dialect = if matches!(source, AudioSource::Mt32 { .. }) {
                self.midi_mode
            } else {
                Mode::Gm
            };
            self.routing
                .replay(dialect, &mut |message| replay.extend_from_slice(message));
        }
        Ok(AudioConfig {
            source,
            rate,
            nanos: self.session.emulated_nanos(),
            replay,
        })
    }
    fn apply_audio_volume(&self) {
        if self.audio_mixer == crate::audio::Mixer::Application
            && let Some(audio) = &self.audio
        {
            audio.set_volume(self.audio_volume);
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn select_audio_mixer(&mut self, mixer: crate::audio::Mixer) -> Result<(), String> {
        if self.audio_deferred {
            self.audio_mixer = mixer;
            return Ok(());
        }
        if self.audio_mixer == mixer && self.audio.as_ref().is_some_and(|a| a.check().is_ok()) {
            return Ok(());
        }
        self.session.pause();
        self.anchor = None;
        if let Some(audio) = &self.audio {
            audio.set_active(false);
        }
        // Prepare the complete replacement before discarding the old output.
        // A failed connection leaves the previous mixer selected and paused.
        let audio = crate::audio::Output::new(self.audio_backend, mixer)?;
        audio.set_active(false);
        let config = self.audio_config(&self.midi_choice, audio.rate, true)?;
        let mut engine = AudioEngine::new();
        engine.configure(
            config,
            Some(audio.writer_with_reserve(self.session.v86_host_clock_enabled())),
        )?;
        self.audio_engine = engine;
        self.audio = Some(audio);
        self.audio_mixer = mixer;
        self.apply_audio_volume();
        self.audio_fallback_used = false;
        self.notice = Some(format!("{mixer} mixer selected; resume to continue."));
        Ok(())
    }
    fn poll_audio_error(&mut self) {
        let output_result = self.audio.as_ref().map_or(Ok(()), |a| a.check());
        let output_failed = output_result.is_err();
        let result = self.audio_engine.check().and(output_result);
        if let Err(error) = result {
            self.session.pause();
            if let Some(audio) = &self.audio {
                audio.set_active(false);
            }
            if self.audio_backend == crate::audio::Backend::Auto
                && output_failed
                && !self.audio_fallback_used
                && self.audio.as_ref().is_some_and(|audio| audio.is_pulse())
            {
                self.audio_fallback_used = true;
                let replacement = crate::audio::Output::new(
                    crate::audio::Backend::Cpal,
                    crate::audio::Mixer::Application,
                )
                .and_then(|audio| {
                    audio.set_active(false);
                    let config = self.audio_config(&self.midi_choice, audio.rate, true)?;
                    let mut engine = AudioEngine::new();
                    engine.configure(
                        config,
                        Some(audio.writer_with_reserve(self.session.v86_host_clock_enabled())),
                    )?;
                    Ok((audio, engine))
                });
                match replacement {
                    Ok((audio, engine)) => {
                        self.audio_mixer = crate::audio::Mixer::Application;
                        self.audio_engine = engine;
                        self.audio = Some(audio);
                        self.apply_audio_volume();
                        self.notice =
                            Some(format!("{error}. Switched to CPAL; resume to continue."));
                    }
                    Err(fallback) => {
                        self.notice = Some(format!("{error}; CPAL fallback: {fallback}"))
                    }
                }
            } else {
                self.notice = Some(error);
            }
        }
    }
    #[cfg(test)]
    fn restored_mt32(&self, rate: u32) -> Result<doslet_mt32::Timeline, String> {
        let directory = self
            .mt32_rom_dir
            .as_ref()
            .ok_or("Use --mt32-rom-dir to load CM-32L ROMs")?;
        let mut engine =
            Self::load_mt32(directory, rate, self.midi_mode).map_err(|error| error.to_string())?;
        engine.rebase(self.session.emulated_nanos());
        let mut replay_error = None;
        self.routing.replay(self.midi_mode, &mut |message| {
            if replay_error.is_some() {
                return;
            }
            for &byte in message {
                if let Err(error) = engine.restore_byte(byte) {
                    replay_error = Some(error.to_string());
                    break;
                }
            }
        });
        if let Some(error) = replay_error {
            return Err(error);
        }
        Ok(engine)
    }
    fn reselect_midi(&mut self, selection: &str) -> Result<(), String> {
        self.reselect_midi_at(selection, None)
    }
    fn reselect_midi_at(&mut self, selection: &str, start: Option<u64>) -> Result<(), String> {
        if selection == APPLE_LABEL && !cfg!(target_os = "macos") {
            return Err("Apple MIDI output is available only on native macOS".into());
        }
        if self.audio_deferred {
            if !matches!(selection, "CM-32L / MT-32" | "Rust synth" | APPLE_LABEL) {
                if self.midi_mode == Mode::Gm && self.midi_output_mode == Mode::Mt32 {
                    return Err("GM to an MT-32 host port is unsupported".into());
                }
                let ports = crate::native_midi::ports()?;
                if ports.selection_name(selection).is_none() {
                    return Err(format!("MIDI output not found: {selection}"));
                }
            }
            let config = self.audio_config(selection, self.audio_engine.rate(), false)?;
            self.audio_engine.configure(config, None)?;
            self.midi_choice = selection.into();
            return Ok(());
        }
        if matches!(selection, "CM-32L / MT-32" | "Rust synth" | APPLE_LABEL) {
            // A MIDI dialect/synth change does not require reopening CPAL.
            // Keep the current stream until the replacement synth is ready.
            let new_audio = if self.audio.is_none() {
                Some(crate::audio::Output::new(
                    self.audio_backend,
                    self.audio_mixer,
                )?)
            } else {
                None
            };
            let rate = self.audio.as_ref().or(new_audio.as_ref()).unwrap().rate;
            let mut config = self.audio_config(selection, rate, true)?;
            if let Some(nanos) = start {
                config.nanos = nanos;
            }
            self.audio_engine.configure(
                config,
                self.audio
                    .as_ref()
                    .or(new_audio.as_ref())
                    .map(|a| a.writer_with_reserve(self.session.v86_host_clock_enabled())),
            )?;
            // Dropping the old sender completes its final silence before reuse.
            drop(self.host_midi.take());
            if let Some(audio) = new_audio {
                self.audio = Some(audio);
            }
            let audio = self.audio.as_ref().unwrap();
            audio.set_active(self.session.status() == SessionStatus::Running);
        } else {
            if self.midi_mode == Mode::Gm && self.midi_output_mode == Mode::Mt32 {
                return Err("GM to an MT-32 host port is unsupported; select MT-32 game mode. GM on moont uses GmDevice.".into());
            }
            let ports = crate::native_midi::ports()?;
            self.midi_choices = crate::native_synth::choices();
            if self.mt32_rom_dir.is_some() {
                self.midi_choices.push("CM-32L / MT-32".into());
            }
            self.midi_choices.extend(ports.names.iter().cloned());
            let same_port = self
                .host_midi
                .as_ref()
                .is_some_and(|old| ports.selection_name(selection) == Some(old.name.as_str()));
            let mut output = if same_port && self.host_midi.as_ref().unwrap().check().is_ok() {
                // Virtual reconnect: cancel/reset on the same sender, without
                // opening a second physical connection to an exclusive port.
                self.host_midi.take().unwrap()
            } else {
                if same_port {
                    drop(self.host_midi.take());
                }
                ports.connect(selection)?
            };
            // Disconnect a different old receiver before initializing the new one.
            drop(self.host_midi.take());
            output.reset(self.midi_output_mode)?;
            output.set_active(self.session.status() == SessionStatus::Running)?;
            let mut replay = Vec::with_capacity(SNAPSHOT_CAPACITY);
            self.routing.replay(self.midi_output_mode, &mut |message| {
                replay.extend_from_slice(message)
            });
            output.push(self.session.emulated_nanos(), &replay)?;
            if self.audio.is_none() {
                match crate::audio::Output::new(self.audio_backend, self.audio_mixer) {
                    Ok(audio) => self.audio = Some(audio),
                    Err(error) => {
                        eprintln!("[WARN doslet] Sound Blaster output unavailable: {error}")
                    }
                }
            }
            let rate = self
                .audio
                .as_ref()
                .map_or(self.audio_engine.rate(), |audio| audio.rate);
            let mut config = self.audio_config(selection, rate, false)?;
            if let Some(nanos) = start {
                config.nanos = nanos;
            }
            self.audio_engine.configure(
                config,
                self.audio
                    .as_ref()
                    .map(|a| a.writer_with_reserve(self.session.v86_host_clock_enabled())),
            )?;
            if let Some(audio) = &self.audio {
                audio.set_active(self.session.status() == SessionStatus::Running);
            }
            self.host_midi = Some(output);
        }
        self.apply_audio_volume();
        self.midi_choice = self
            .host_midi
            .as_ref()
            .map_or_else(|| selection.into(), |output| output.name.clone());
        self.anchor = None;
        self.notice = None;
        Ok(())
    }
    fn select_midi(&mut self, selection: &str) -> Result<(), String> {
        let ready = match selection {
            "Rust synth" | APPLE_LABEL => self.audio.is_some() && !self.audio_engine.is_mt32(),
            "CM-32L / MT-32" => self.audio.is_some() && self.audio_engine.is_mt32(),
            _ => self.host_midi.as_ref().is_some_and(|o| o.check().is_ok()),
        };
        if selection == self.midi_choice && ready && self.notice.is_none() {
            return Ok(());
        }
        self.reselect_midi(selection)
    }
    #[cfg(all(test, target_os = "macos"))]
    fn restored_synth(&self, rate: u32, selection: &str) -> Result<Timeline, String> {
        let mut engine = if selection == APPLE_LABEL {
            Timeline::apple(rate)?
        } else {
            Timeline::new(rate)?
        };
        engine.skip_until(self.session.emulated_nanos())?;
        let mut replay_error = None;
        self.routing.replay(Mode::Gm, &mut |message| {
            if replay_error.is_some() {
                return;
            }
            for &byte in message {
                if let Err(error) = engine.restore_byte(byte) {
                    replay_error = Some(error);
                    break;
                }
            }
        });
        if let Some(error) = replay_error {
            return Err(error);
        }
        Ok(engine)
    }
    pub fn midi_mode_label(&self) -> String {
        let route = if self.midi_mode == Mode::Mt32 && self.output_dialect() == Mode::Gm {
            "MT-32 → GM (approximation)"
        } else if self.audio_engine.is_mt32() && self.midi_mode == Mode::Gm {
            "GM → CM-32L (approximation, 8 melodic parts)"
        } else {
            ""
        };
        let stats = self.routing.translator().stats();
        let lcd = String::from_utf8_lossy(self.routing.translator().display());
        format!(
            "{route}{} · {} SysEx · {} unmapped notes · {lcd}",
            if self.routing.upgraded() {
                " · Roland detected: upgraded to MT-32"
            } else {
                ""
            },
            stats.sysex,
            stats.unmapped_notes
        )
    }
    pub fn audio_spectrum(&self) -> [f32; doslet_pcm::spectrum::BANDS] {
        if self.session.status() != SessionStatus::Running {
            return [0.0; doslet_pcm::spectrum::BANDS];
        }
        self.audio
            .as_ref()
            .map_or([0.0; doslet_pcm::spectrum::BANDS], |audio| audio.spectrum())
    }
    pub fn pointer_dimensions(&self) -> (u16, u16) {
        self.session.pointer_dimensions()
    }
    pub fn guest_audio_description(&self) -> String {
        let status = self.session.audio_status();
        let (bits, channels) = status.configured_format.map_or((0, 0), |format| {
            (
                if format.word { 16 } else { 8 },
                if format.stereo { 2 } else { 1 },
            )
        });
        crate::ui::guest_pcm_description(status.rate, bits, channels)
    }
    fn relative_mouse(&self) -> bool {
        self.configurator
            .active
            .input
            .mouse_mode
            .relative(self.relative_latch.relative)
    }
    /// Grabs the pointer when the mouse mode switches to capturing while the
    /// pointer is over the game picture. A dialog covering the picture defers
    /// the grab until it closes; a pointer outside the picture is left alone.
    fn follow_mouse_mode(&mut self) -> Task<Message> {
        let relative = self.relative_mouse();
        self.capture_on_switch = relative && (self.capture_on_switch || !self.was_relative);
        self.was_relative = relative;
        if !self.capture_on_switch || self.configurator.open || self.workspace.panel.is_some() {
            return Task::none();
        }
        self.capture_on_switch = false;
        if self.pointer_inside
            && self.session.status() == SessionStatus::Running
            && self.mouse_capture.is_none()
            && !self.mouse_capture_pending
        {
            self.request_capture()
        } else {
            Task::none()
        }
    }
    /// Starts capturing the host pointer for relative guest motion.
    fn request_capture(&mut self) -> Task<Message> {
        self.mouse_capture_pending = true;
        self.mouse_capture_generation = self.mouse_capture_generation.wrapping_add(1);
        let generation = self.mouse_capture_generation;
        iced::window::latest()
            .and_then(move |id| Task::done(Message::CaptureWindow(id, generation)))
    }
    pub fn has_mouse_capture(&self) -> bool {
        self.mouse_capture.is_some() && !self.mouse_capture_pending
    }
    pub fn hide_host_cursor(&self) -> bool {
        self.has_mouse_capture()
            || self.configurator.active.input.hide_cursor
                && !self.relative_mouse()
                && self.session.pointer_in_use()
                && self.session.status() == SessionStatus::Running
                && !self.configurator.open
                && self.workspace.panel.is_none()
                && !self.configurator.pending
    }
    fn maintain_tier_learning(&mut self) -> bool {
        let Some(tiers) = &mut self.speed_tiers else {
            return false;
        };
        let mut output_error = None;
        let ran = tiers.maintain_learning_with_pause(&mut self.session, || {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(audio) = &self.audio {
                audio.set_active(false);
            }
            if let Some(output) = &mut self.host_midi {
                output_error = output.set_active(false).err();
            }
        });
        if let Some(error) = tiers.take_persistence_error() {
            self.notice = Some(error);
        }
        if ran {
            self.anchor = None;
            self.presentation_revision = self.presentation_revision.wrapping_add(1);
            // update() restores playback from the unchanged guest position.
            if let Some(error) = output_error {
                self.notice = Some(error);
                self.session.pause();
            }
        }
        ran
    }
    fn poll_paused_tier_persistence(&mut self) {
        if self.session.status() != SessionStatus::Paused
            || self.configurator.pending
            || self.debugger.as_ref().is_some_and(|gdb| gdb.connected())
        {
            return;
        }
        if let Some(tiers) = &mut self.speed_tiers {
            let progress = |s: crate::native_speed_tiers::Stats| {
                (
                    s.learning_running,
                    s.learning_pending,
                    s.learning_jobs,
                    s.learning_changes,
                    s.saved_updates,
                    s.save_pending,
                )
            };
            let before = progress(tiers.stats);
            tiers.maintain_learning(&mut self.session);
            let error = tiers.take_persistence_error();
            if before != progress(tiers.stats) || error.is_some() {
                self.presentation_revision = self.presentation_revision.wrapping_add(1);
            }
            if let Some(error) = error {
                self.notice = Some(error);
            }
        }
    }
    /// Watch for the live layout of a 32-bit LE or DJGPP COFF program:
    /// learned tiers, relocatable patches and AOT procedures follow it.
    #[cfg(not(target_arch = "wasm32"))]
    fn watch_layout(&mut self, aot: bool) {
        if self.session.v86_enabled() {
            return;
        }
        let Some(path) = &self.source else {
            return;
        };
        let exe: std::sync::Arc<[u8]> = match crate::native_zip::program_bytes(path, &self.launch) {
            Ok(bytes) => bytes.into(),
            Err(error) => {
                eprintln!("[WARN doslet] Cannot read the program for its layout: {error}");
                return;
            }
        };
        let program = path.file_name().unwrap_or_default().to_string_lossy();
        self.layout = crate::le_layout::Watcher::new(exe.clone());
        if self.layout.is_none() {
            if aot {
                eprintln!(
                    "[INFO doslet] AOT: {program} is neither a 32-bit LE nor a DJGPP COFF program; using the interpreter"
                );
            }
            return;
        }
        #[cfg(feature = "wasm-procedures")]
        if aot {
            use sha2::{Digest, Sha256};
            match crate::host_dirs::HostDirs::detect() {
                Ok(dirs) => {
                    let directory = dirs
                        .aot_modules()
                        .join(format!("{:x}", Sha256::digest(&exe)));
                    self.aot = Some(crate::native_aot::Aot::new(
                        program.into_owned(),
                        exe,
                        directory,
                        self.launch.compiled_cache.clone(),
                    ));
                }
                Err(error) => eprintln!(
                    "[INFO doslet] AOT: no user cache directory for modules: {error}; using the interpreter"
                ),
            }
        }
        #[cfg(not(feature = "wasm-procedures"))]
        let _ = aot;
    }
    /// Speed tiers installed after the layout was found follow it as well.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn follow_layout_with_tiers(&mut self) {
        let (Some(watcher), Some((epoch, layout))) = (&self.layout, &self.live_layout) else {
            return;
        };
        if *epoch == self.session.epoch() {
            crate::native_layout::follow(
                &mut self.session,
                &[],
                self.speed_tiers.as_mut(),
                watcher.exe(),
                layout,
            );
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn track_layout(&mut self) {
        let Some(watcher) = &mut self.layout else {
            return;
        };
        if let Some(layout) = watcher.poll(&self.session) {
            let objects = layout
                .objects
                .iter()
                .map(|o| format!("{}:{:#x}", o.number, o.base))
                .collect::<Vec<_>>()
                .join(", ");
            if layout.text.is_some() {
                eprintln!(
                    "[INFO doslet] {}: COFF sections at {objects} (code segment base {:#x}, layout {})",
                    self.name,
                    layout.code.base,
                    layout.key()
                );
            } else {
                eprintln!("[INFO doslet] {}: LE objects at {objects}", self.name);
            }
            crate::native_layout::follow(
                &mut self.session,
                &self.launch.patches,
                self.speed_tiers.as_mut(),
                watcher.exe(),
                &layout,
            );
            #[cfg(feature = "wasm-procedures")]
            if let Some(aot) = &mut self.aot {
                aot.layout_found(&mut self.session, &layout);
            }
            self.live_layout = Some((self.session.epoch(), layout));
        }
        #[cfg(feature = "wasm-procedures")]
        if let Some(aot) = &mut self.aot {
            aot.poll(&mut self.session);
        }
    }
    pub(crate) fn worker_tick(&mut self) -> Task<Message> {
        #[cfg(not(target_arch = "wasm32"))]
        self.track_layout();
        let computer_effects = if let Some(mut computer) = self.computer.take() {
            let effects = computer.poll(self);
            let closing = computer.is_closing();
            self.computer = Some(computer);
            if closing {
                return effects;
            }
            effects
        } else {
            Task::none()
        };
        self.poll_paused_tier_persistence();
        let effects = if self.session.status() == SessionStatus::Running
            || self.session.storage_waiting()
            || self.debugger.is_some()
        {
            self.update(Message::Tick)
        } else {
            self.poll_gamepads();
            self.poll_audio_error();
            self.log_errors();
            Task::none()
        };
        Task::batch([computer_effects, effects])
    }
    pub(crate) fn worker_delay(&self, progressed: bool) -> Duration {
        if self.session.status() != SessionStatus::Running && !self.session.storage_waiting() {
            return Duration::from_millis(16);
        }
        if self.session.v86_ram_wait_parked() && !self.session.debug_irq_pending() {
            // A recognized wait has no guest work until an IRQ or memory change.
            // Input commands wake the channel sooner. If no IRQ deadline is
            // known, keep servicing autonomous audio and presentation;
            // otherwise sleep to the device deadline and catch up its clock.
            return self.session.debug_service_horizon().map_or_else(
                || Duration::from_millis(16),
                |until_event| {
                    Duration::from_nanos(until_event.saturating_sub(self.host_lag_nanos()))
                },
            );
        }
        if self.session.v86_input_poll_parked() && !self.session.debug_irq_pending() {
            return self.session.debug_service_horizon().map_or_else(
                || Duration::from_millis(16),
                |until_event| {
                    Duration::from_nanos(until_event.saturating_sub(self.host_lag_nanos()))
                        .min(Duration::from_millis(16))
                },
            );
        }
        if let Some(nanos) = self.session.v86_vga_poll_wait_nanos()
            && !self.session.debug_irq_pending()
        {
            // The vertical-retrace pulse is only about 63 us wide in mode 13h.
            // Sleep toward it, accounting for existing host lag, then poll
            // without sleeping for the last 250 us so the scheduler cannot
            // skip the pulse and stall a title animation for another frame.
            return Duration::from_nanos(
                nanos
                    .saturating_sub(self.host_lag_nanos())
                    .saturating_sub(250_000),
            );
        }
        if let Some(nanos) = self.session.v86_mpu_poll_wait_nanos()
            && nanos != 0
            && !self.session.debug_irq_pending()
        {
            return Duration::from_nanos(nanos.max(50_000));
        }
        if self.session.v86_host_clock_enabled()
            && progressed
            && !self.session.debug_cpu().is_halted()
        {
            return Duration::ZERO;
        }
        if !progressed || self.host_lag_nanos() == 0 {
            Duration::from_millis(1)
        } else {
            // Behind the host clock: check commands and continue immediately.
            Duration::ZERO
        }
    }
    pub(crate) fn host_lag_nanos(&self) -> u64 {
        let Some((start, guest)) = self.anchor else {
            return 0;
        };
        let target =
            guest.saturating_add(start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
        target.saturating_sub(self.session.emulated_nanos())
    }
    pub(crate) fn snapshot(&mut self, include_config: bool) -> crate::desktop::Snapshot {
        use crate::desktop::{Registers, SessionSnapshot, Snapshot};
        use doslet_x86::Gpr16;
        self.display.refresh(&self.session);
        let adaptive_idle_percent = self.adaptive_meter.sample(
            self.session.menu_idle_enabled() && self.session.status() == SessionStatus::Running,
            self.session.emulated_nanos(),
            self.session.menu_idle_stats().nanos,
        );
        let tiers = self.speed_tier_stats();
        let idle = self.session.idle_loop_stats();
        let cpu_activity = self.cpu_meter.sample(
            self.session.status() == SessionStatus::Running
                && !self.session.storage_waiting()
                && !self.debugger.as_ref().is_some_and(|gdb| gdb.connected()),
            crate::cpu_meter::Counters {
                epoch: self.session.epoch(),
                guest: self.session.emulated_nanos(),
                idle: idle
                    .skipped_nanos
                    .saturating_add(self.session.menu_idle_stats().nanos),
                executed: self.session.executed_tier_counts(),
                grey_mhz: tiers.filter(|s| s.enabled).map(|s| s.grey_mhz),
            },
        );
        let registers = self.session.registers();
        Snapshot {
            cpu_settings: self.cpu_settings(),
            v86_host_clock: self.session.v86_host_clock_enabled(),
            v86_ram_waits: self.session.v86_ram_waits(),
            v86_vga_polling: self.session.v86_vga_polling(),
            v86_mpu_polling: self.session.v86_mpu_polling(),
            v86_input_polling: self.session.v86_input_polling(),
            idle_loops: self.session.idle_loops_enabled(),
            vga_waits: self.session.vga_wait_acceleration_enabled(),
            cpu_profile: self.speed_tiers.as_ref().map(|t| t.profile_info()),
            cpu_learning_available: self.session.check_working_state().is_ok(),
            cpu_activity,
            disk_reads: (self.session.epoch(), self.session.disk_read_bytes()),
            video_mode_label: self.session.video_mode_label().to_string(),
            video_refresh_mhz: self.session.video_refresh_millihertz(),
            audio_volume: self.audio_volume,
            audio_use: self.session.audio_use(),
            memory_usage: self.session.memory_usage(),
            speed_tiers: self.speed_tier_stats(),
            idle_observation: self.session.idle_observation(),
            synchronization: self.session.synchronization_stats(),
            analysis: self.session.synchronization_analysis(),
            host_audio_underruns: self.audio_underruns(),
            adaptive_idle_percent,
            learned_idle: self.session.adaptive_idle_enabled(),
            menu_idle: (
                self.session.menu_idle_enabled(),
                self.session.menu_idle_stats().nanos,
                self.session.menu_idle_stats().interval_nanos,
            ),
            mcp: self.computer.as_ref().map(|computer| computer.address()),
            gdb: self
                .debugger
                .as_ref()
                .and_then(|gdb| gdb.address().ok().map(|address| (address, gdb.connected()))),
            session: SessionSnapshot {
                status: self.session.status(),
                steps: self.session.steps(),
                registers: Registers {
                    cs: registers.cs,
                    ip: registers.ip,
                    values: [
                        Gpr16::Ax,
                        Gpr16::Cx,
                        Gpr16::Dx,
                        Gpr16::Bx,
                        Gpr16::Sp,
                        Gpr16::Bp,
                        Gpr16::Si,
                        Gpr16::Di,
                    ]
                    .map(|r| registers.get(r)),
                },
                fault: self.session.fault(),
                v86_error: self.session.v86_last_error().map(str::to_owned),
                pending_commit: self.session.pending_commit(),
                storage_waiting: self.session.storage_waiting(),
                dirty: self.session.dirty_overlay(),
            },
            workspace: self.workspace.clone(),
            configurator: include_config.then(|| self.configurator.presentation_copy()),
            display: self.display.clone(),
            name: self.name.clone(),
            notice: self.notice.clone(),
            midi_choices: self.midi_choices.clone(),
            midi_choice: self.midi_choice.clone(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            audio_mixer: self.audio_mixer,
            midi_mode: self.midi_mode,
            midi_output_mode: self.midi_output_mode,
            midi_activity: self.midi_activity.clone(),
            midi_channels: self.routing.channels(self.output_dialect()),
            spectrum: self.audio_spectrum(),
            pointer_dimensions: self.pointer_dimensions(),
            captured: self.has_mouse_capture(),
            hide_cursor: self.hide_host_cursor(),
            audio_label: self.audio_label(),
            storage_label: self.storage_label(),
            midi_label: self.midi_mode_label(),
            guest_audio: self.guest_audio_description(),
            lag_nanos: if self.session.status() == SessionStatus::Running {
                self.host_lag_nanos()
            } else {
                0
            },
            #[cfg(test)]
            guest_nanos: self.session.emulated_nanos(),
            #[cfg(test)]
            heroes_movie: self
                .session
                .ram_view(0x68EFC..0x68EFC + 0x76C)
                .and_then(|movie| {
                    let dword =
                        |offset| u32::from_le_bytes(movie[offset..offset + 4].try_into().unwrap());
                    ((dword(4), dword(8), dword(0xC)) == (640, 480, 161))
                        .then(|| (dword(0x678), dword(0x768)))
                }),
        }
    }
}

#[cfg(all(test, feature = "wasm-procedures", not(target_arch = "wasm32")))]
#[path = "wasm_presentation_tests.rs"]
mod wasm_presentation_tests;

#[cfg(all(test, feature = "wasm-procedures", not(target_arch = "wasm32")))]
#[path = "app_quake_tests.rs"]
mod quake_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    include!("../../../tests/fixtures/midi.rs");
    use super::*;

    /// The guest has polled relative motion long enough for Auto to lock.
    fn lock_relative(app: &mut Doslet) {
        app.relative_latch = RelativeLatch {
            relative: true,
            streak: None,
        };
        app.was_relative = true;
    }

    #[test]
    fn auto_mouse_mode_locks_after_steady_reads_and_unlocks_after_a_quiet_spell() {
        const MS: u64 = 1_000_000;
        let mut latch = RelativeLatch::default();
        assert!(!latch.observe(None, 0));
        // A single read, e.g. a menu probing the driver, never locks.
        for now in [0, 200 * MS, 900 * MS] {
            assert!(!latch.observe(Some(0), now));
        }
        assert!(!latch.relative);
        // A gap longer than a second starts a new streak.
        assert!(!latch.observe(Some(2_000 * MS), 2_000 * MS));
        assert!(!latch.observe(Some(2_250 * MS), 2_250 * MS));
        assert!(
            latch.observe(Some(2_300 * MS), 2_300 * MS),
            "steady reads lock"
        );
        assert!(latch.relative);
        // Pauses in polling shorter than the quiet spell keep it locked.
        assert!(!latch.observe(Some(2_300 * MS), 4_000 * MS));
        assert!(!latch.observe(Some(2_300 * MS), 5_299 * MS));
        assert!(latch.relative);
        assert!(!latch.observe(Some(2_300 * MS), 5_300 * MS));
        assert!(!latch.relative, "three quiet seconds unlock");
    }

    #[test]
    fn a_dialog_over_the_picture_defers_the_grab_until_it_closes() {
        use doslet_config::MouseMode;
        let mut app = Doslet::new();
        app.launch.machine_spec = Some(doslet_config::MachineSpec::default());
        app.session = Session::new(&[0xEB, 0xFE]).unwrap().into();
        app.session.start();
        let _ = app.update(Message::Config(crate::configurator::Edit::MouseMode(
            MouseMode::NoCapture,
        )));
        let _ = app.update(Message::PointerMoved(10, 10));
        let _ = app.update(Message::Ui(crate::ui::Action::Open(
            crate::ui::Panel::Input,
        )));
        let _ = app.update(Message::Config(crate::configurator::Edit::MouseMode(
            MouseMode::Capture,
        )));
        assert!(
            !app.mouse_capture_pending,
            "the input panel still covers it"
        );
        let _ = app.update(Message::Ui(crate::ui::Action::Close));
        assert!(app.mouse_capture_pending);
    }

    #[test]
    fn switching_to_capture_grabs_a_pointer_over_the_picture_but_not_outside() {
        use doslet_config::MouseMode;
        for inside in [true, false] {
            let mut app = Doslet::new();
            app.launch.machine_spec = Some(doslet_config::MachineSpec::default());
            app.session = Session::new(&[0xEB, 0xFE]).unwrap().into();
            app.session.start();
            let _ = app.update(Message::Config(crate::configurator::Edit::MouseMode(
                MouseMode::NoCapture,
            )));
            let _ = app.update(Message::PointerMoved(10, 10));
            if !inside {
                let _ = app.update(Message::PointerLeft);
            }
            assert!(!app.mouse_capture_pending);
            let _ = app.update(Message::Config(crate::configurator::Edit::MouseMode(
                MouseMode::Capture,
            )));
            assert_eq!(app.mouse_capture_pending, inside, "inside={inside}");
            // Staying in a capturing mode does not grab again after release.
            let _ = app.update(Message::ReleaseMouse);
            let _ = app.update(Message::PointerMoved(11, 11));
            assert!(!app.mouse_capture_pending);
        }
    }
    #[test]
    fn blaster_changes_require_restart_and_preserve_active_settings() {
        let mut app = Doslet::new();
        app.session.start();
        app.session.advance(20);
        let before = (
            app.session.epoch(),
            app.session.steps(),
            app.session.emulated_nanos(),
            app.session.debug_pc(),
        );
        app.configurator.draft.hardware.sb16 = false;
        app.configurator.draft.hardware.mpu401 = false;
        let _ = app.update(Message::Config(crate::configurator::Edit::ApplyAudio));
        assert_eq!(
            (
                app.session.epoch(),
                app.session.steps(),
                app.session.emulated_nanos(),
                app.session.debug_pc()
            ),
            before
        );
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert!(app.session.hardware_config().sb16);
        assert!(app.launch.hardware.sb16);
        assert_ne!(app.configurator.draft, app.configurator.active);
        assert!(
            app.configurator
                .message
                .as_deref()
                .unwrap()
                .contains("restart")
        );
    }
    #[test]
    fn deferred_audio_selection_and_silent_batches_do_not_open_host_outputs_or_roms() {
        let mut app = Doslet::new();
        app.audio_deferred = true;
        app.mt32_rom_dir = Some("/missing/doslet-dormant-roms".into());
        app.select_midi("CM-32L / MT-32").unwrap();
        assert!(app.audio.is_none());
        assert!(app.audio_engine.is_mt32());
        app.session = Session::new(&[0xeb, 0xfe]).unwrap().into();
        app.session.start();
        app.session.advance(10_000);
        app.process_audio().unwrap();
        assert!(app.audio_deferred);
        assert!(app.audio.is_none());
        assert!(app.host_midi.is_none());
        assert!(app.audio_engine.inline().midi.is_none());
        assert!(app.audio_engine.inline().mt32.is_none());
        app.select_midi("Rust synth").unwrap();
        assert!(!app.audio_engine.is_mt32());
        assert!(app.audio.is_none());
        assert!(app.audio_engine.inline().midi.is_none());
    }
    #[test]
    fn input_panel_applies_live_and_preserves_other_drafts_and_restart_settings() {
        use crate::{configurator::Edit, ui::Panel};
        use doslet_config::{KeyboardMode, MouseMode};
        let mut app = Doslet::new();
        app.session = Session::new(&[0x40, 0xeb, 0xfd]).unwrap().into();
        app.session.start();
        app.session.advance(100);
        app.launch.machine_spec = Some(app.configurator.active.clone());
        app.configurator.draft.hardware.memory_mib = 32;
        app.workspace.panel = Some(Panel::Input);
        let before = (
            app.session.debug_pc(),
            app.session.steps(),
            app.session.emulated_nanos(),
            app.session.epoch(),
        );
        for edit in [
            Edit::MouseMode(MouseMode::NoCapture),
            Edit::HideCursor(true),
            Edit::Keyboard(true),
        ] {
            let _ = app.update(Message::Config(edit));
            assert_eq!(app.workspace.panel, Some(Panel::Input));
            assert!(!app.configurator.pending && !app.configurator.open);
            assert_eq!(app.session.status(), SessionStatus::Running);
            assert_eq!(
                (
                    app.session.debug_pc(),
                    app.session.steps(),
                    app.session.emulated_nanos(),
                    app.session.epoch()
                ),
                before
            );
            assert_eq!(app.configurator.draft.hardware.memory_mib, 32);
            assert_eq!(app.configurator.active.hardware.memory_mib, 16);
        }
        assert_eq!(app.session.hardware_config().keyboard, KeyboardMode::At);
        assert_eq!(app.configurator.active.hardware.keyboard, KeyboardMode::At);
        assert_eq!(
            app.launch.machine_spec.as_ref().unwrap().hardware.keyboard,
            KeyboardMode::At
        );
        assert!(app.launch.machine_spec.as_ref().unwrap().input.hide_cursor);
        assert_eq!(
            app.launch.machine_spec.as_ref().unwrap().input.mouse_mode,
            MouseMode::NoCapture
        );
        app.reset();
        assert_eq!(app.session.hardware_config().keyboard, KeyboardMode::At);
        assert_eq!(
            app.configurator.active.input.mouse_mode,
            MouseMode::NoCapture
        );
        assert!(app.configurator.active.input.hide_cursor);
    }

    #[test]
    fn audio_hardware_edits_remain_in_panel_and_leave_running_machine_unchanged() {
        use crate::{configurator::Edit, ui::Panel};
        let mut app = Doslet::new();
        app.session.start();
        app.workspace.panel = Some(Panel::Audio);
        let steps = app.session.steps();
        let _ = app.update(Message::Config(Edit::Sb16(false)));
        assert_eq!(app.workspace.panel, Some(Panel::Audio));
        assert!(!app.configurator.draft.hardware.sb16);
        assert!(app.configurator.active.hardware.sb16);
        assert!(app.session.hardware_config().sb16);
        assert_eq!(app.session.steps(), steps);
        let _ = app.update(Message::Config(Edit::Discard));
        assert!(app.configurator.draft.hardware.sb16);
        assert_eq!(app.workspace.panel, Some(Panel::Audio));
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires PULSE_SINK=doslet-audio-probe-output, a dedicated null sink"]
    fn mixer_selection_prepares_both_outputs_and_rolls_back_failed_switch() {
        use crate::audio::{Backend, Mixer};
        assert_eq!(
            std::env::var("PULSE_SINK").unwrap(),
            "doslet-audio-probe-output"
        );
        let mut app = Doslet::new();
        app.audio_backend = Backend::Pulse;
        app.session.start();
        app.select_audio_mixer(Mixer::System).unwrap();
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert_eq!(app.audio_mixer, Mixer::System);
        assert_eq!(app.audio.as_ref().unwrap().mixer(), Mixer::System);
        app.audio.as_ref().unwrap().check().unwrap();

        app.midi_choice = "CM-32L / MT-32".into();
        assert!(app.select_audio_mixer(Mixer::Application).is_err());
        assert_eq!(app.audio_mixer, Mixer::System);
        assert_eq!(app.audio.as_ref().unwrap().mixer(), Mixer::System);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        app.audio_engine.check().unwrap();

        app.midi_choice = "Rust synth".into();
        app.select_audio_mixer(Mixer::Application).unwrap();
        assert_eq!(app.audio_mixer, Mixer::Application);
        assert_eq!(app.audio.as_ref().unwrap().mixer(), Mixer::Application);
        assert_eq!(app.session.status(), SessionStatus::Paused);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_mixer_selection_prepares_outputs_and_rolls_back_failed_switch() {
        use crate::audio::{Backend, Mixer};
        let mut app = Doslet::new();
        app.audio_backend = Backend::Cpal;
        app.session.start();
        app.select_audio_mixer(Mixer::System).unwrap();
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert_eq!(app.audio_mixer, Mixer::System);
        assert_eq!(app.audio.as_ref().unwrap().mixer(), Mixer::System);
        app.audio.as_ref().unwrap().check().unwrap();

        app.midi_choice = "CM-32L / MT-32".into();
        assert!(app.select_audio_mixer(Mixer::Application).is_err());
        assert_eq!(app.audio_mixer, Mixer::System);
        assert_eq!(app.audio.as_ref().unwrap().mixer(), Mixer::System);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        app.audio_engine.check().unwrap();

        app.midi_choice = "Rust synth".into();
        app.select_audio_mixer(Mixer::Application).unwrap();
        assert_eq!(app.audio_mixer, Mixer::Application);
        assert_eq!(app.audio.as_ref().unwrap().mixer(), Mixer::Application);
        assert_eq!(app.session.status(), SessionStatus::Paused);
    }

    #[test]
    fn learned_profiles_activate_by_executable_with_saved_rates_and_can_be_disabled() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let fixture = crate::profile::tests::Fixture::new(
                    r#"
version = 1
[game]
executable = "../TINY.EXE"
[audio]
enabled = false
"#,
                );
                let seed = fixture.root.join("seed.json");
                let cache = fixture.root.join("tier-cache");
                std::fs::write(
                    &seed,
                    serde_json::to_vec(&serde_json::json!({
                        "unknown":"grey", "irq_policy":"static-grey", "nodes":[{
                            "pc":0,"bits":16,"bytes":vec![0x90;15],"tier":"fast"
                        }]
                    }))
                    .unwrap(),
                )
                .unwrap();
                {
                    let app = Doslet::from_cli(fixture.cli(&[
                        "--speed-tier-profile",
                        seed.to_str().unwrap(),
                        "--learn-speed-tiers",
                        "--hot-mhz",
                        "500",
                        "--grey-mhz",
                        "100",
                        "--speed-tier-cache",
                        cache.to_str().unwrap(),
                    ]))
                    .unwrap();
                    assert!(app.speed_tiers.as_ref().unwrap().stats.learning);
                }
                let mut app =
                    Doslet::from_cli(fixture.cli(&["--speed-tier-cache", cache.to_str().unwrap()]))
                        .unwrap();
                let stats = app.speed_tiers.as_ref().unwrap().stats;
                assert!(stats.enabled && !stats.learning);
                assert_eq!(
                    (stats.hot_mhz, stats.cold_mhz, stats.grey_mhz),
                    (Some(500), 8, 100)
                );
                assert_eq!(stats.restored_labels, 1);
                assert!(!app.launch.preload_local_media);
                app.reset();
                assert_eq!(app.speed_tiers.as_ref().unwrap().stats.restored_labels, 1);
                drop(app);
                let app = Doslet::from_cli(fixture.cli(&[
                    "--speed-tier-cache",
                    cache.to_str().unwrap(),
                    "--no-speed-tiers",
                ]))
                .unwrap();
                assert!(app.speed_tiers.is_none());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn zip_recording_activates_passively_on_next_launch_and_tracks_executable_bytes() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                use clap::Parser;
                let fixture = crate::profile::tests::Fixture::new("version=2");
                let zip = |fixture: &crate::profile::tests::Fixture, files: &[(&str, &[u8])]| {
                    let path = fixture.root.join("game.zip");
                    std::fs::write(
                        &path,
                        crate::archive::tests::pack(files, zip::CompressionMethod::Deflated)
                            .as_slice(),
                    )
                    .unwrap();
                    path
                };
                let path = zip(&fixture, &[("GAME/PLAY.COM", &[0xb8, 7, 0x4c, 0xcd, 0x21])]);
                let cache = fixture.root.join("tier-cache");
                let launch = |extra: &[&str]| {
                    let mut arguments = vec![
                        "doslet",
                        "--zip",
                        path.to_str().unwrap(),
                        "--no-audio",
                        "--speed-tier-cache",
                        cache.to_str().unwrap(),
                    ];
                    arguments.extend_from_slice(extra);
                    Doslet::from_cli(crate::cli::Cli::parse_from(arguments)).unwrap()
                };
                {
                    let app = launch(&[
                        "--learn-speed-tiers",
                        "--hot-mhz",
                        "450",
                        "--grey-mhz",
                        "95",
                        "--cold-mhz",
                        "9",
                    ]);
                    assert!(app.speed_tiers.as_ref().unwrap().stats.learning);
                }
                {
                    let app = launch(&[]);
                    let tiers = app.speed_tiers.as_ref().unwrap();
                    assert!(tiers.stats.enabled);
                    assert!(!tiers.stats.learning);
                    assert_eq!(
                        (
                            tiers.stats.hot_mhz,
                            tiers.stats.grey_mhz,
                            tiers.stats.cold_mhz
                        ),
                        (Some(450), 95, 9),
                    );
                    assert!(!app.launch.preload_local_media);
                }
                assert!(launch(&["--no-speed-tiers"]).speed_tiers.is_none());
                zip(&fixture, &[("GAME/PLAY.COM", &[0x90, 0xcd, 0x20])]);
                assert!(launch(&[]).speed_tiers.is_none());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn recording_starts_without_a_seed_and_persists_game_cache() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                for mode in ["fixed", "adaptive"] {
                    let fixture = crate::profile::tests::Fixture::new(&format!(
                        r#"
version = 2
[game]
executable = "../TINY.EXE"
[machine]
cpu_mode = "{mode}"
[output]
audio = false
"#
                    ));
                    let cache = fixture.root.join("tier-cache");
                    {
                        let app = Doslet::from_cli(fixture.cli(&[
                            "--learn-speed-tiers",
                            "--speed-tier-cache",
                            cache.to_str().unwrap(),
                            "--hot-mhz",
                            "400",
                            "--grey-mhz",
                            "90",
                            "--cold-mhz",
                            "7",
                        ]))
                        .unwrap();
                        let stats = app.speed_tiers.as_ref().unwrap().stats;
                        assert!(stats.enabled && stats.learning);
                        assert_eq!(
                            (stats.hot_mhz, stats.grey_mhz, stats.cold_mhz),
                            (Some(400), 90, 7)
                        );
                        assert!(app.launch.preload_local_media);
                        assert!(!app.launch.adaptive_cpu);
                    }
                    let app = Doslet::from_cli(fixture.cli(&[
                        "--learn-speed-tiers",
                        "--speed-tier-cache",
                        cache.to_str().unwrap(),
                    ]))
                    .unwrap();
                    let stats = app.speed_tiers.as_ref().unwrap().stats;
                    assert!(stats.enabled && stats.learning);
                    assert_eq!(
                        (stats.hot_mhz, stats.grey_mhz, stats.cold_mhz),
                        (Some(400), 90, 7)
                    );
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn recording_requires_an_executable_identity() {
        use clap::Parser;
        let error = Doslet::from_cli(crate::cli::Cli::parse_from([
            "doslet",
            "--demo",
            "--no-audio",
            "--learn-speed-tiers",
        ]))
        .err()
        .unwrap();
        assert!(error.to_string().contains("requires a game executable"));
    }

    #[test]
    fn portable_snapshot_launch_and_reset_get_fresh_host_epochs() {
        use clap::Parser;
        let fixture = crate::profile::tests::Fixture::new("");
        let path = fixture.root.join("live.pb");
        let original = Session::new(&[0x40, 0xeb, 0xfd]).unwrap();
        std::fs::write(&path, original.save_state().unwrap()).unwrap();
        let mut app = Doslet::from_cli(crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--load-state".into(),
            path.into_os_string(),
            "--no-audio".into(),
        ]))
        .unwrap();
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert!(app.source.is_none() && app.storage.is_none());
        let epoch = app.session.epoch();
        let pc = app.session.debug_pc();
        app.session.advance(11);
        app.reset();
        assert_ne!(app.session.epoch(), epoch);
        assert_eq!(app.session.debug_pc(), pc);
        assert_eq!(app.session.emulated_nanos(), 0);
    }

    #[test]
    fn snapshot_with_game_config_resets_to_the_executable() {
        // Use a bounded setup stack, as in the native snapshot loader.
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(snapshot_with_game_config_reset_check)
            .unwrap()
            .join()
            .unwrap();
    }

    fn snapshot_with_game_config_reset_check() {
        let fixture = crate::profile::tests::Fixture::new(
            r#"
version = 1
name = "Reset game"
[game]
executable = "../TINY.EXE"
arguments = ["/reset"]
[audio]
enabled = false
"#,
        );
        let path = fixture.root.join("live.pb");
        let mut snapshot = Session::new(&[0x40, 0xeb, 0xfd]).unwrap();
        snapshot.start();
        snapshot.advance(20);
        std::fs::write(&path, snapshot.save_state().unwrap()).unwrap();
        let mut app =
            Doslet::from_cli(fixture.cli(&["--load-state", path.to_str().unwrap()])).unwrap();
        assert_eq!(app.session.emulated_nanos(), snapshot.emulated_nanos());
        assert!(app.restore_state.is_some());
        for _ in 0..2 {
            let epoch = app.session.epoch();
            app.reset();
            assert!(app.notice.is_none(), "{:?}", app.notice);
            assert!(app.restore_state.is_none());
            assert_ne!(app.session.epoch(), epoch);
            assert_eq!(app.session.emulated_nanos(), 0);
            assert_eq!(app.session.status(), SessionStatus::Running);
            assert_eq!(app.name, "Reset game");
            assert_eq!(app.session.ram_view(0x10081..0x10088).unwrap(), b" /reset");
            app.session.advance(100);
            assert_eq!(app.session.status(), SessionStatus::Exited(7));
            assert_eq!(app.session.console(), b"Relocated MZ");
        }
    }

    include!("../../../tests/fixtures/iso9660.rs");

    #[test]
    fn autosave_with_cdrom_iso_preloads_local_media_and_resumes_snapshot() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let fixture = crate::profile::tests::Fixture::new(
                    r#"
version = 2
name = "CD-ROM Game"
[game]
executable = "../TINY.EXE"
[[drives]]
letter = "C"
source = ".."
kind = "directory"
[[drives]]
letter = "D"
source = "../disc.gog"
kind = "iso"
[output]
audio = false
"#,
                );
                std::fs::write(fixture.root.join("disc.gog"), test_iso()).unwrap();
                let autosave = fixture.root.join("autosave.state.gz");
                let mut app =
                    Doslet::from_cli(fixture.cli(&["--autosave-path", autosave.to_str().unwrap()]))
                        .unwrap();
                assert!(app.launch.preload_local_media);
                app.session.advance(2);
                assert_eq!(app.session.status(), SessionStatus::Running);
                app.save_autosave();
                drop(app);
                assert!(
                    autosave.is_file(),
                    "expected autosave.state.gz to be created"
                );

                let mut resumed = Doslet::from_cli(fixture.cli(&[
                    "--load-state",
                    autosave.to_str().unwrap(),
                    "--autosave-path",
                    autosave.to_str().unwrap(),
                ]))
                .unwrap();
                assert_eq!(resumed.session.status(), SessionStatus::Running);
                resumed.session.advance(100);
                assert_eq!(resumed.session.status(), SessionStatus::Exited(7));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[ignore = "requires the local portable city fixture and trained three-group profile"]
    fn local_speed_tiers_engine_comparison() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut results = Vec::new();
        for (run, jit) in [true, false, false, true, true, false]
            .into_iter()
            .enumerate()
        {
            let mut args = vec![
                std::ffi::OsString::from("doslet"),
                "--load-state".into(),
                root.join("target/heroes-states/city.pb.gz")
                    .into_os_string(),
                "--speed-tier-profile".into(),
                root.join("target/speed-tiers/city-static-grey.json")
                    .into_os_string(),
                "--no-audio".into(),
            ];
            if !jit {
                args.push("--no-dynamic-core".into());
            }
            let mut app = Doslet::from_cli(crate::cli::Cli::parse_from(args)).unwrap();
            let now = Instant::now();
            app.tick_at(now);
            for i in 1..=400 {
                app.tick_at(now + Duration::from_millis(i * 5));
            }
            let before = app.session.emulated_nanos();
            let start = Instant::now();
            for i in 401..=1000 {
                app.tick_at(now + Duration::from_millis(i * 5));
            }
            let wall = start.elapsed().as_secs_f64();
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
            let guest = app.session.emulated_nanos() - before;
            assert!(guest >= 2_950_000_000);
            let result = serde_json::json!({"run":run, "jit":jit, "guest_ns":guest,
                "host_seconds":wall, "host_seconds_per_guest_second":wall/(guest as f64/1e9),
                "dynamic_invalidations":app.session.dynamic_stats().invalidations});
            println!("{result}");
            results.push(result);
        }
        std::fs::write(
            root.join("target/speed-tiers/engine-comparison.json"),
            serde_json::to_vec_pretty(&results).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires the local portable city fixture and trained three-group profile"]
    fn local_speed_tiers_city_snapshot() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut app = Doslet::from_cli(crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--load-state".into(),
            root.join("target/heroes-states/city.pb.gz")
                .into_os_string(),
            "--speed-tier-profile".into(),
            root.join("target/speed-tiers/city-static-grey.json")
                .into_os_string(),
            "--no-audio".into(),
        ]))
        .unwrap();
        let initial = app.session.emulated_nanos();
        let now = Instant::now();
        app.tick_at(now);
        let mut cold_at_ten_seconds = 0;
        for i in 1..=2400 {
            if i % 20 == 0 {
                app.session
                    .pointer_relative(if i % 40 == 0 { 1.0 } else { -1.0 }, 0.0);
            }
            app.tick_at(now + Duration::from_millis(i * 5));
            if i == 2000 {
                cold_at_ten_seconds = app.speed_tier_stats().unwrap().steps[1];
            }
        }
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        assert!(app.session.emulated_nanos() >= initial + 11_500_000_000);
        let stats = app.speed_tier_stats().unwrap();
        assert!(stats.steps.iter().all(|v| *v > 0));
        assert!(stats.steps[1] > cold_at_ten_seconds + 100_000);
        println!(
            "live city: guest_ms={} steps={:?} time={:?}",
            (app.session.emulated_nanos() - initial) / 1_000_000,
            stats.steps,
            stats.nanos
        );
        let pc = app.session.debug_pc();
        let time = app.session.emulated_nanos();
        let _ = app.update(Message::SpeedTiersToggle);
        assert!(!app.speed_tier_stats().unwrap().enabled);
        assert_eq!(app.session.debug_pc(), pc);
        assert_eq!(app.session.emulated_nanos(), time);
        let _ = app.update(Message::SpeedTiersToggle);
        assert!(app.speed_tier_stats().unwrap().enabled);
        let _ = app.update(Message::CpuMhz(133));
        assert!(!app.speed_tier_stats().unwrap().enabled);
        app.configurator.discard_changes = true;
        app.reset();
        assert_eq!(app.session.emulated_nanos(), initial);
    }
    #[cfg(all(
        feature = "dynamic-core",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    mod native_profile_fixture {
        include!("../../../tests/fixtures/protected_graphics.rs");
    }
    #[cfg(all(
        feature = "dynamic-core",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn native_window_close_saves_observations_before_returning_the_close_task() {
        let fixture = crate::profile::tests::Fixture::new("");
        let path = fixture.root.join("native.core-profile");
        let mut program = native_profile_fixture::protected_graphics_com();
        let code = [0x40, 0x41, 0x42, 0xEB, 0xFB];
        program[0x300..0x300 + code.len()].copy_from_slice(&code);
        let mut app = Doslet::new();
        app.session = Session::new(&program).unwrap().into();
        app.launch.core_profile = Some(path.clone());
        app.session.start();
        assert_eq!(app.session.advance(1000), 1000);
        let mut expected = vec![0; doslet_x86::DYNAMIC_PROFILE_MAX_BYTES];
        let length = app.session.write_dynamic_profile(&mut expected);
        assert!(length > 6, "synthetic loop must record hot code");
        assert!(!path.exists());
        let _ = app.update(Message::CloseRequested(iced::window::Id::unique()));
        assert_eq!(std::fs::read(path).unwrap(), expected[..length]);
    }
    #[test]
    fn captured_at_typing_is_discarded_and_paused_release_allows_a_fresh_press() {
        use iced::keyboard::{
            Event, Key, Location, Modifiers,
            key::{Code, Physical},
        };
        let press = Event::KeyPressed {
            key: Key::Character("a".into()),
            modified_key: Key::Character("a".into()),
            physical_key: Physical::Code(Code::KeyA),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: Some("a".into()),
            repeat: false,
        };
        let release = Event::KeyReleased {
            key: Key::Character("a".into()),
            modified_key: Key::Character("a".into()),
            physical_key: Physical::Code(Code::KeyA),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
        };
        let mut app = Doslet::new();
        let hardware = doslet_config::HardwareConfig {
            keyboard: doslet_config::KeyboardMode::At,
            ..Default::default()
        };
        app.session = Session::demo_configured(hardware).unwrap().into();
        app.configurator.active.hardware = hardware;
        app.session.start();
        let _ = app.update(Message::KeyboardCaptured(press.clone()));
        assert_eq!(
            app.session.input_router_mut().next_event(),
            Some(doslet_input::GuestEvent::ReleaseAll)
        );
        assert_eq!(app.session.input_router_mut().next_event(), None);
        app.session.pause();
        let _ = app.update(Message::Keyboard(release));
        while app.session.input_router_mut().next_event().is_some() {}
        app.session.start();
        let _ = app.update(Message::Keyboard(press));
        assert_eq!(
            app.session.input_router_mut().next_event(),
            Some(doslet_input::GuestEvent::Key {
                key: doslet_input::PhysicalKey(4),
                pressed: true
            })
        );
        assert_eq!(app.session.steps(), 0);
    }
    #[test]
    fn gdb_stop_does_not_enqueue_input_release_or_change_guest_time() {
        use std::io::Write;
        let mut app = Doslet::new();
        app.session = Session::new(&[0x40, 0xEB, 0xFD]).unwrap().into();
        app.session.start();
        while app.session.input_router_mut().next_event().is_some() {}
        app.session.physical_key(4, true).unwrap();
        while app.session.input_router_mut().next_event().is_some() {}
        app.enable_gdb("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut client =
            std::net::TcpStream::connect(app.debugger.as_ref().unwrap().address().unwrap())
                .unwrap();
        client.write_all(b"$qSupported").unwrap();
        let now = Instant::now();
        let _ = app.update(Message::Tick);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert_eq!(app.session.steps(), 0);
        assert_eq!(app.session.input_router_mut().next_event(), None);
        let nanos = app.session.emulated_nanos();
        app.tick_at(now + Duration::from_secs(10));
        assert_eq!(app.session.emulated_nanos(), nanos);
        assert_eq!(app.session.input_router_mut().next_event(), None);
        let _ = app.update(Message::GdbToggle);
        app.tick_at(now + Duration::from_secs(10));
        assert_eq!(
            app.session.steps(),
            0,
            "resuming must not catch up paused wall time"
        );
    }
    #[test]
    fn failed_candidate_restores_running_session_and_reset_recipe() {
        let mut app = Box::new(Doslet::new());
        app.session.start();
        app.session.advance(25);
        let steps = app.session.steps();
        app.source = Some(std::path::PathBuf::from("old-game.exe"));
        let old = app.configurator.active.clone();
        let mut draft = old.clone();
        draft.hardware.memory_mib = 8;
        draft.game.executable = "/missing/doslet-game.exe".into();
        draft.output.audio = false;
        app.configurator.draft = draft.clone();
        app.configurator.pending = true;
        app.preparation_was_running = true;
        app.session.pause();
        let mut settings = app.launch.clone();
        settings.hardware = draft.hardware;
        let result = Doslet::prepare_candidate(draft, settings, None, None);
        assert!(result.result.is_err());
        app.finish_preparation(Ok(std::sync::Arc::new(std::sync::Mutex::new(Some(result)))));
        assert_eq!(app.session.steps(), steps);
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.configurator.active, old);
        assert_eq!(app.launch.hardware.memory_mib, 16);
        assert_eq!(
            app.source.as_deref(),
            Some(std::path::Path::new("old-game.exe"))
        );
        assert!(!app.configurator.pending);
    }
    #[test]
    fn prepared_hardware_becomes_the_active_session_and_reset_recipe() {
        let mut app = Box::new(Doslet::new());
        let mut spec = doslet_config::MachineSpec::default();
        spec.hardware.memory_mib = 4;
        spec.hardware.sb16 = false;
        spec.output.audio = false;
        let settings = crate::native_input::LaunchSettings {
            hardware: spec.hardware,
            machine_spec: Some(spec.clone()),
            no_dynamic_core: true,
            core_profile: Some(std::path::PathBuf::from("unused-disabled-core-profile")),
            ..Default::default()
        };
        app.dynamic_error_reported = true;
        let candidate = Doslet::prepare_candidate(spec.clone(), settings, None, None);
        assert!(candidate.result.is_ok());
        app.pending_output = Some(Box::new(
            Doslet::prepare_output(
                &spec,
                crate::audio::Backend::Cpal,
                crate::audio::Mixer::Application,
            )
            .unwrap(),
        ));
        app.configurator.pending = true;
        app.finish_preparation(Ok(std::sync::Arc::new(std::sync::Mutex::new(Some(
            candidate,
        )))));
        assert_eq!(app.session.hardware_config(), spec.hardware);
        assert_eq!(app.configurator.active, spec);
        assert_eq!(app.launch.hardware.memory_mib, 4);
        assert!(app.launch.no_dynamic_core);
        assert!(!app.session.dynamic_core_enabled());
        assert!(!app.dynamic_error_reported);
        assert_eq!(
            app.launch.core_profile.as_deref(),
            Some(std::path::Path::new("unused-disabled-core-profile"))
        );
        app.reset();
        assert_eq!(app.session.hardware_config(), spec.hardware);
        assert!(!app.session.dynamic_core_enabled());
    }
    #[test]
    fn configured_demo_respects_disabled_native_core_on_launch_and_reset() {
        let mut app = Doslet::from_cli(crate::cli::Cli {
            no_audio: true,
            no_dynamic_core: true,
            memory_mib: Some(4),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(app.session.hardware_config().memory_mib, 4);
        assert!(!app.session.dynamic_core_enabled());
        app.reset();
        assert_eq!(app.session.hardware_config().memory_mib, 4);
        assert!(!app.session.dynamic_core_enabled());
    }
    #[cfg(feature = "v86")]
    #[test]
    #[ignore = "requires tools/v86-build"]
    fn v86_cli_launch_and_reset_keep_the_selected_core() {
        use clap::Parser;
        let mut app = Doslet::from_cli(crate::cli::Cli::parse_from([
            "doslet",
            "--v86",
            "--demo",
            "--no-audio",
            "--v86-module",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/v86/doslet-v86.wasm"
            ),
        ]))
        .unwrap();
        assert!(app.session.v86_enabled());
        assert!(app.session.v86_host_clock_enabled());
        assert_eq!(app.cpu_settings().core, crate::cpu_config::Core::V86);
        app.session.start();
        for _ in 0..1000 {
            app.session.advance(4096);
            if app.session.status() != SessionStatus::Running {
                break;
            }
        }
        assert_eq!(
            app.session.status(),
            SessionStatus::Exited(0),
            "{:?}",
            app.session.fault()
        );
        app.reset();
        assert!(app.session.v86_enabled());
        assert!(app.session.v86_host_clock_enabled());
        assert_eq!(app.cpu_settings().core, crate::cpu_config::Core::V86);
    }
    include!("../../../tests/fixtures/sb16_pcm.rs");
    include!("../../../tests/fixtures/opl3.rs");
    #[test]
    fn opl3_fm_flows_through_native_pcm_mixer_without_host_device() {
        let mut app = Doslet::new();
        app.session = Session::new(&opl3_tone_program()).unwrap().into();
        app.session.start();
        for _ in 0..50 {
            app.session.advance(10000);
            app.process_audio().unwrap();
        }
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        assert!(app.audio_engine.stats().nonzero > 1000);
        assert!(
            app.audio_engine
                .inline_mut()
                .samples
                .iter()
                .flatten()
                .any(|sample| sample.abs() > 0.01)
        );
    }
    #[test]
    fn uncaptured_escape_reaches_dos_for_both_keyboard_and_capture_modes() {
        for keyboard in [
            doslet_config::KeyboardMode::Bios,
            doslet_config::KeyboardMode::At,
        ] {
            for capture in [false, true] {
                let mut app = Doslet::new();
                // Read the mouse, then one BIOS key and store AX at DS:0200h.
                let hardware = doslet_config::HardwareConfig {
                    keyboard,
                    ..Default::default()
                };
                app.configurator.active.hardware = hardware;
                app.session = Session::new_configured(
                    &[
                        0xB8, 3, 0, 0xCD, 0x33, 0xB4, 0, 0xCD, 0x16, 0xA3, 0, 2, 0xEB, 0xFE,
                    ],
                    hardware,
                )
                .unwrap()
                .into();
                app.configurator.active.input.mouse_mode = if capture {
                    doslet_config::MouseMode::Auto
                } else {
                    doslet_config::MouseMode::NoCapture
                };
                app.configurator.active.input.hide_cursor = true;
                app.session.start();
                assert!(!app.hide_host_cursor());
                app.session.advance(100);
                assert!(app.session.waiting_for_key());
                assert!(app.hide_host_cursor());
                let _ = app.update(Message::PointerMoved(123, 45));
                let _ = app.update(Message::PointerButton(0, true));
                assert_eq!(app.session.pointer_position(), (123, 45));
                assert!(app.mouse_capture.is_none());
                assert!(!app.mouse_capture_pending);
                let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
                let _ = app.update(Message::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: escape.clone(),
                    modified_key: escape,
                    physical_key: iced::keyboard::key::Physical::Code(
                        iced::keyboard::key::Code::Escape,
                    ),
                    location: iced::keyboard::Location::Standard,
                    modifiers: iced::keyboard::Modifiers::empty(),
                    text: None,
                    repeat: false,
                }));
                // The BIOS keyboard wait resumes at the next guest timer interrupt.
                app.session.advance(1_000_000);
                assert_eq!(app.session.ram_view(0x10200..0x10202).unwrap(), &[0x1B, 1]);
                assert!(app.hide_host_cursor());
                app.workspace.panel = Some(crate::ui::Panel::Audio);
                assert!(!app.hide_host_cursor());
                app.workspace.panel = None;
                app.session.pause();
                assert!(!app.hide_host_cursor());
            }
        }
    }

    #[test]
    fn configured_startup_text_queues_bios_keys_on_native_tick() {
        let mut app = Doslet::new();
        app.configurator.active.game.startup_keys = "A?".into();
        // Two blocking BIOS reads store their scan/ASCII pairs in the PSP.
        app.session = Session::new(&[
            0xb4, 0, 0xcd, 0x16, 0xa3, 0, 2, 0xb4, 0, 0xcd, 0x16, 0xa3, 2, 2, 0xeb, 0xfe,
        ])
        .unwrap()
        .into();
        app.session.start();
        app.session.advance(100);
        assert!(app.session.waiting_for_key());
        let now = Instant::now();
        app.anchor = Some((now - Duration::from_millis(5), app.session.emulated_nanos()));
        app.tick_at(now);
        assert_eq!(app.startup_keys_index, 2);
        // The blocked BIOS read resumes on the next guest timer interrupt.
        app.session.advance(1_000_000);
        assert_eq!(
            app.session.ram_view(0x10200..0x10204).unwrap(),
            &[b'A', 0x1e, b'?', 0x35],
            "index={} notice={:?} status={:?} steps={}",
            app.startup_keys_index,
            app.notice,
            app.session.status(),
            app.session.steps()
        );
    }

    #[test]
    fn captured_escape_reaches_dos_but_ctrl_escape_only_releases_mouse() {
        for keyboard in [
            doslet_config::KeyboardMode::Bios,
            doslet_config::KeyboardMode::At,
        ] {
            for pending in [false, true] {
                let mut app = Doslet::new();
                let hardware = doslet_config::HardwareConfig {
                    keyboard,
                    ..Default::default()
                };
                app.configurator.active.hardware = hardware;
                // Select relative mouse input, store the first key, then wait for another.
                app.session = Session::new_configured(
                    &[
                        0xB8, 0x0B, 0, 0xCD, 0x33, 0xB4, 0, 0xCD, 0x16, 0xA3, 0, 2, 0xB4, 0, 0xCD,
                        0x16, 0xA3, 2, 2, 0xEB, 0xFE,
                    ],
                    hardware,
                )
                .unwrap()
                .into();
                app.session.start();
                app.session.advance(100);
                assert!(app.session.waiting_for_key());
                lock_relative(&mut app);
                app.mouse_capture = Some(iced::window::Id::unique());
                app.mouse_capture_pending = pending;
                let escape = |modifiers| {
                    let key = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
                    Message::Keyboard(iced::keyboard::Event::KeyPressed {
                        key: key.clone(),
                        modified_key: key,
                        physical_key: iced::keyboard::key::Physical::Code(
                            iced::keyboard::key::Code::Escape,
                        ),
                        location: iced::keyboard::Location::Standard,
                        modifiers,
                        text: None,
                        repeat: false,
                    })
                };
                let _ = app.update(escape(iced::keyboard::Modifiers::empty()));
                assert!(app.mouse_capture.is_some());
                assert_eq!(app.mouse_capture_pending, pending);
                app.session.advance(1_000_000);
                assert_eq!(app.session.ram_view(0x10200..0x10202).unwrap(), &[0x1B, 1]);
                assert!(app.session.waiting_for_key());

                let _ = app.update(escape(iced::keyboard::Modifiers::CTRL));
                assert!(app.mouse_capture.is_none());
                assert!(!app.mouse_capture_pending);
                app.session.advance(1_000_000);
                assert!(app.session.waiting_for_key());
                assert_eq!(app.session.ram_view(0x10202..0x10204).unwrap(), &[0, 0]);
            }
        }
    }

    #[test]
    fn mouse_capture_mode_locks_without_guest_polling_and_changes_without_restart() {
        use doslet_config::MouseMode;
        use iced::window::MouseCaptureEvent;
        let mut app = Doslet::new();
        app.launch.machine_spec = Some(doslet_config::MachineSpec::default());
        app.session = Session::new(&[0xEB, 0xFE]).unwrap().into();
        app.session.start();
        let epoch = app.session.epoch();
        let _ = app.update(Message::Config(crate::configurator::Edit::MouseMode(
            MouseMode::Capture,
        )));
        assert!(!app.session.pointer_relative_in_use());
        assert!(app.relative_mouse());
        let _ = app.update(Message::PointerButton(0, true));
        assert!(app.mouse_capture_pending);
        let id = iced::window::Id::unique();
        let generation = app.mouse_capture_generation;
        let _ = app.update(Message::CaptureWindow(id, generation));
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Acquired,
        ));
        assert!(app.has_mouse_capture());
        let until = app.session.emulated_nanos() + 1_500_000_000;
        while app.session.emulated_nanos() < until {
            let remaining = until - app.session.emulated_nanos();
            app.session.debug_advance_device_time(remaining).unwrap();
        }
        let _ = app.update(Message::PointerMoved(0, 0));
        assert!(app.has_mouse_capture(), "forced capture must not expire");
        let before = app.session.pointer_position();
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Motion { dx: 10.0, dy: 2.0 },
        ));
        assert_eq!(
            app.session.pointer_position(),
            (before.0 + 10, before.1 + 2)
        );
        let _ = app.update(Message::Config(crate::configurator::Edit::MouseMode(
            MouseMode::NoCapture,
        )));
        assert!(!app.has_mouse_capture());
        assert!(!app.mouse_capture_pending);
        let _ = app.update(Message::PointerMoved(10, 20));
        assert_eq!(app.session.pointer_position(), (10, 20));
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Motion { dx: 99.0, dy: 99.0 },
        ));
        assert_eq!(
            app.session.pointer_position(),
            (10, 20),
            "stale relative input after mode change"
        );
        assert_eq!(app.session.epoch(), epoch);
        assert_eq!(
            app.configurator.active.input.mouse_mode,
            MouseMode::NoCapture
        );
        assert_eq!(
            app.configurator.draft.input.mouse_mode,
            MouseMode::NoCapture
        );
        assert_eq!(
            app.launch.machine_spec.unwrap().input.mouse_mode,
            MouseMode::NoCapture
        );
    }

    #[test]
    fn mouse_no_capture_uses_absolute_motion_even_after_guest_relative_reads() {
        let mut app = Doslet::new();
        app.session = Session::new(&[0xb8, 0x0b, 0, 0xcd, 0x33, 0xEB, 0xFE])
            .unwrap()
            .into();
        app.session.start();
        app.session.advance(16);
        assert!(app.session.pointer_relative_in_use());
        let _ = app.update(Message::Config(crate::configurator::Edit::MouseMode(
            doslet_config::MouseMode::NoCapture,
        )));
        let _ = app.update(Message::PointerMoved(12, 34));
        let _ = app.update(Message::PointerButton(0, true));
        assert_eq!(app.session.pointer_position(), (12, 34));
        assert!(!app.mouse_capture_pending);
        assert_eq!(app.session.pointer_buttons(), 1);
    }

    #[test]
    fn relative_mouse_blocks_hover_then_unlocks_after_queries_stop() {
        use iced::window::MouseCaptureEvent;
        let mut app = Doslet::new();
        app.session = Session::new(&[
            0xb8, 0x0b, 0, 0xcd, 0x33, 0xb8, 3, 0, 0xcd, 0x33, 0xeb, 0xfe,
        ])
        .unwrap()
        .into();
        app.session.start();
        app.session.advance(16);
        assert!(app.session.pointer_relative_in_use());
        lock_relative(&mut app);
        let before = app.session.pointer_position();
        let _ = app.update(Message::PointerMoved(5, 6));
        assert_eq!(app.session.pointer_position(), before);
        assert!(!app.hide_host_cursor());
        let id = iced::window::Id::unique();
        let _ = app.update(Message::PointerButton(0, true));
        let generation = app.mouse_capture_generation;
        let _ = app.update(Message::CaptureWindow(id, generation));
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Acquired,
        ));
        assert!(app.has_mouse_capture());
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Button {
                button: 0,
                pressed: true,
            },
        ));
        let last_read = app.session.pointer_relative_last_read().unwrap();
        let advance_to = |app: &mut Doslet, until: u64| {
            while app.session.emulated_nanos() < until {
                let remaining = until - app.session.emulated_nanos();
                app.session.debug_advance_device_time(remaining).unwrap();
            }
        };
        // The old one-second window no longer drops the capture.
        advance_to(&mut app, last_read + 1_500_000_000);
        let _ = app.update(Message::PointerMoved(7, 8));
        assert!(app.has_mouse_capture(), "inertia keeps the capture");
        advance_to(&mut app, last_read + RELATIVE_UNLOCK_NANOS);
        let _ = app.update(Message::PointerMoved(7, 8));
        assert!(!app.has_mouse_capture());
        assert_eq!(app.session.pointer_buttons(), 0);
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Motion { dx: 10.0, dy: 20.0 },
        ));
        assert_eq!(app.session.pointer_position(), before);
        let _ = app.update(Message::PointerMoved(9, 10));
        let _ = app.update(Message::PointerButton(0, true));
        assert_eq!(app.session.pointer_position(), (9, 10));
        assert!(!app.mouse_capture_pending);
        assert_eq!(app.session.pointer_buttons(), 1);
    }

    #[test]
    fn mouse_capture_uses_relative_motion_releases_on_ctrl_escape_and_ignores_stale_events() {
        use iced::window::MouseCaptureEvent;
        let mut app = Doslet::new();
        app.session = Session::new(&[0xb8, 0x0b, 0, 0xcd, 0x33, 0xEB, 0xFE])
            .unwrap()
            .into();
        app.session.start();
        app.session.advance(16);
        assert!(app.session.pointer_relative_in_use());
        lock_relative(&mut app);
        let id = iced::window::Id::unique();
        let _ = app.update(Message::PointerButton(0, true));
        let generation = app.mouse_capture_generation;
        let _ = app.update(Message::CaptureWindow(id, generation));
        assert!(!app.hide_host_cursor());
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Acquired,
        ));
        assert!(app.hide_host_cursor());
        let before = app.session.pointer_position();
        let _ = app.update(Message::PointerMoved(0, 0));
        assert_eq!(app.session.pointer_position(), before);
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Motion {
                dx: 20.0,
                dy: -15.0,
            },
        ));
        assert_eq!(
            app.session.pointer_position(),
            (before.0 + 20, before.1 - 15)
        );
        let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
        let _ = app.update(Message::Keyboard(iced::keyboard::Event::KeyPressed {
            key: escape.clone(),
            modified_key: escape,
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Escape),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::CTRL,
            text: None,
            repeat: false,
        }));
        assert!(!app.hide_host_cursor());
        let _ = app.update(Message::PointerButton(0, true));
        let next = app.mouse_capture_generation;
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Released,
        ));
        assert!(app.mouse_capture_pending);
        let _ = app.update(Message::CaptureWindow(id, next));
        let _ = app.update(Message::MouseCapture(id, next, MouseCaptureEvent::Acquired));
        let _ = app.update(Message::RunPause);
        assert!(!app.hide_host_cursor());
        assert_eq!(app.session.status(), SessionStatus::Paused);
        let _ = app.update(Message::MouseCapture(
            id,
            next,
            MouseCaptureEvent::Motion { dx: 10.0, dy: 10.0 },
        ));
        assert_eq!(
            app.session.pointer_position(),
            (before.0 + 20, before.1 - 15)
        );
    }
    #[test]
    fn rw_writeback_persists_closed_files_and_overlay_keeps_host_unchanged() {
        use doslet_dos::files::ReadOnlyDrive;
        let root = std::env::temp_dir().join(format!("doslet-rw-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("Heroes.cfg");
        std::fs::write(&target, b"original").unwrap();
        let mut drive = ReadOnlyDrive::new();
        drive.mount(b"HEROES.CFG", b"original".to_vec()).unwrap();
        let mut drive = drive.with_write_overlay(1024);
        let handle = drive.create(b"HEROES.CFG", 0).unwrap();
        drive
            .write_buffer(handle, 3)
            .unwrap()
            .copy_from_slice(b"new");
        let mut session = Session::new(&[0x90]).unwrap().with_drive(drive);
        let settings = crate::native_input::LaunchSettings {
            persist: true,
            ..Default::default()
        };
        let executable = root.join("GAME.EXE");
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &settings).unwrap(),
            0
        );
        let mut drive = ReadOnlyDrive::new();
        drive.mount(b"HEROES.CFG", b"original".to_vec()).unwrap();
        let mut drive = drive.with_write_overlay(1024);
        let handle = drive.create(b"HEROES.CFG", 0).unwrap();
        drive
            .write_buffer(handle, 3)
            .unwrap()
            .copy_from_slice(b"new");
        drive.close(handle).unwrap();
        let mut session = Session::new(&[0x90]).unwrap().with_drive(drive);
        let overlay = crate::native_input::LaunchSettings::default();
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &overlay).unwrap(),
            0
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &settings).unwrap(),
            1
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &settings).unwrap(),
            0
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sb16_pcm_flows_through_native_mixer_without_host_device() {
        let mut app = Doslet::new();
        app.session = Session::new(&sb16_pcm_program()).unwrap().into();
        app.session.start();
        for _ in 0..100 {
            app.session.advance(10000);
            app.process_audio().unwrap();
        }
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        assert!(app.audio_engine.stats().nonzero > 1000);
        assert!(
            app.audio_engine
                .inline_mut()
                .samples
                .iter()
                .flatten()
                .any(|sample| sample.abs() > 0.1)
        );
    }
    #[test]
    #[ignore = "opens the user's audio device and plays a short synthetic SB16 tone"]
    fn local_sb16_pcm_reaches_host_output() {
        use clap::Parser;
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cli = crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            repo.join("games/HoMnM.toml").into_os_string(),
            "--midi-output".into(),
            "rust".into(),
        ]);
        let mut app = Doslet::from_cli(cli).unwrap();
        assert!(app.audio.is_some());
        app.session = Session::new(&sb16_pcm_program()).unwrap().into();
        app.session.start();
        for _ in 0..100 {
            app.session.advance(10000);
            app.process_audio().unwrap();
        }
        std::thread::sleep(Duration::from_millis(300));
        let (played, underruns, peak) = app.audio.as_ref().unwrap().stats();
        println!(
            "SB16 host output: source={} nonzero={} played={played} underruns={underruns} peak={peak}",
            app.audio_engine.stats().received,
            app.audio_engine.stats().nonzero
        );
        assert!(played > 1000);
        assert!(peak > 0.1);
    }

    #[test]
    #[ignore = "requires the user's HoMnM installation and ISO; runs the real setup without window/audio"]
    fn local_heroes_setup_profile_reaches_menus_and_exits_without_saving() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cli = crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            root.join("games/HoMnM.toml").into_os_string(),
            "--game".into(),
            root.join("games/HoMnM/SETUP.EXE").into_os_string(),
            "--no-audio".into(),
        ]);
        let mut app = Doslet::from_cli(cli).unwrap();
        let text = |session: &Session| -> String {
            session
                .ram_view(0xB8000..0xB8000 + 4000)
                .unwrap()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|cell| char::from(cell[0]))
                .collect()
        };
        app.session.advance(90_000_000);
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        assert!(text(&app.session).contains("Main Menu"));
        assert!(text(&app.session).contains("VESA Compatible"));
        app.session.push_key(0x02, b'1').unwrap();
        app.session.advance(20_000_000);
        assert!(text(&app.session).contains("Select a New Sound Card"));
        app.session.push_key(0x01, 0x1B).unwrap();
        app.session.advance(20_000_000);
        assert!(text(&app.session).contains("Main Menu"));
        app.session.push_key(0x01, 0x1B).unwrap();
        app.session.advance(20_000_000);
        assert_eq!(
            app.session.status(),
            SessionStatus::Exited(0),
            "{:?}",
            app.session.fault()
        );
        println!(
            "SETUP profile: menus and exit verified at {} steps",
            app.session.steps()
        );
    }

    #[test]
    #[ignore = "requires the user's HEROES installation and homm1.gog ISO"]
    fn local_heroes_profile_passes_the_cd_check() {
        use clap::Parser;
        let config =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/HoMnM.toml");
        let cli = crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            config.into_os_string(),
            "--no-audio".into(),
        ]);
        let mut app = Doslet::from_cli(cli).unwrap();
        app.session.advance(30_000_000);
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        let console = String::from_utf8_lossy(app.session.console());
        assert!(!console.contains("Unable to access CD Drive"));
        assert!(!console.contains("must have the Heroes CD"));
        println!(
            "HEROES profile: {:?}, steps={}, mode={:?}",
            app.session.status(),
            app.session.steps(),
            app.session.video_mode()
        );
    }

    #[test]
    #[ignore = "requires local HoMM and ISO; compares 40 guest seconds with Rust GM"]
    fn local_heroes_background_jit_pacing() {
        use clap::Parser;
        let config =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/HoMnM.toml");
        let mut expected = None;
        for jit in [false, true] {
            let cli = crate::cli::Cli::parse_from([
                std::ffi::OsString::from("doslet"),
                "--config".into(),
                config.clone().into_os_string(),
                "--no-audio".into(),
                "--cpu-mhz".into(),
                "50".into(),
            ]);
            let mut app = Doslet::from_cli(cli).unwrap();
            app.session.set_dynamic_core(jit);
            assert!(app.audio_engine.inline().midi.is_none());
            let start = Instant::now();
            let mut worst = Duration::ZERO;
            let mut windows_over_100ms = 0;
            let mut maintenance = Duration::ZERO;
            for window in 1..=400 {
                let target = window * 100_000_000;
                let began = Instant::now();
                while app.session.emulated_nanos() < target {
                    assert!(start.elapsed() < Duration::from_secs(180));
                    if let Some(storage) = &mut app.storage {
                        storage.service(&mut app.session).unwrap();
                    }
                    if app.session.storage_waiting() {
                        std::thread::yield_now();
                        continue;
                    }
                    let compile = Instant::now();
                    app.jit.maintain(&mut app.session);
                    maintenance = maintenance.max(compile.elapsed());
                    let budget = ((target - app.session.emulated_nanos()) / 80).min(10000) as u32;
                    assert!(budget > 0);
                    app.session.advance(budget);
                    app.process_audio().unwrap();
                    assert_eq!(
                        app.session.status(),
                        SessionStatus::Running,
                        "{:?}",
                        app.session.fault()
                    );
                }
                let elapsed = began.elapsed();
                worst = worst.max(elapsed);
                windows_over_100ms += usize::from(elapsed > Duration::from_millis(100));
            }
            let elapsed = start.elapsed();
            let hash = |bytes: &[u8]| {
                bytes.iter().fold(0xcbf29ce484222325_u64, |h, b| {
                    (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
                })
            };
            let low_hash = hash(app.session.ram_view(0..0xA0000).unwrap());
            let ram_hash = hash(
                app.session
                    .ram_view(0xC0000..app.session.hardware_config().memory_bytes())
                    .unwrap(),
            );
            let frame = app.session.frame().unwrap();
            let frame_hash = hash(frame.pixels);
            let ip = (app.session.registers().cs, app.session.registers().ip);
            let state = (
                ip,
                app.session.steps(),
                app.session.emulated_nanos(),
                low_hash,
                ram_hash,
                frame_hash,
                hash(frame.palette.as_flattened()),
                [
                    doslet_x86::Gpr32::Eax,
                    doslet_x86::Gpr32::Ecx,
                    doslet_x86::Gpr32::Edx,
                    doslet_x86::Gpr32::Ebx,
                    doslet_x86::Gpr32::Esp,
                    doslet_x86::Gpr32::Ebp,
                    doslet_x86::Gpr32::Esi,
                    doslet_x86::Gpr32::Edi,
                ]
                .map(|r| app.session.registers().get32(r)),
                app.session.eflags(),
            );
            if let Some(expected) = expected {
                assert_eq!(state, expected);
            } else {
                expected = Some(state);
            }
            println!(
                "HoMM background_jit={jit}: total={elapsed:?} speed={:.2}x worst_100ms={worst:?} over_100ms={windows_over_100ms} max_maintenance={maintenance:?} compiled={} native_steps={} ram_hash={ram_hash:016x}",
                40.0 / elapsed.as_secs_f64(),
                app.session.dynamic_stats().compiled_blocks,
                app.session.dynamic_stats().executed_steps
            );
            if jit {
                assert!(app.session.dynamic_stats().compiled_blocks > 0);
                assert!(app.session.dynamic_stats().executed_steps > 0);
            }
        }
    }

    #[test]
    #[ignore = "requires the recorded local HoMM build and ISO"]
    fn local_heroes_132mhz_intro_matches_independent_frame() {
        use clap::Parser;
        let config =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/HoMnM.toml");
        for cpu_mhz in [132] {
            let cli = crate::cli::Cli::parse_from([
                std::ffi::OsString::from("doslet"),
                "--config".into(),
                config.clone().into_os_string(),
                "--no-audio".into(),
                "--cpu-mhz".into(),
                cpu_mhz.to_string().into(),
            ]);
            let mut app = Doslet::from_cli(cli).unwrap();
            assert_eq!(app.session.hardware_config().cpu_mhz, cpu_mhz);
            let mut midi = [0; 1024];
            let mut pcm = [doslet_pcm::EMPTY; 128];
            let mut matched = false;
            let deadline = Instant::now() + Duration::from_secs(120);
            while app.session.emulated_nanos() < 40_000_000_000
                && app.session.status() == SessionStatus::Running
            {
                assert!(
                    Instant::now() < deadline,
                    "HoMM intro timed out at {cpu_mhz} MHz"
                );
                if let Some(storage) = &mut app.storage {
                    storage.service(&mut app.session).unwrap();
                }
                if app.session.storage_waiting() {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                app.session.prepare_dynamic(2);
                app.session.advance(10000);
                while app.session.drain_midi(&mut midi) != 0 {}
                while app.session.drain_pcm_timed(&mut pcm) != 0 {}
                // Version-specific Smacker counters and an independently decoded
                // frame hash. CLI clocks do not depend on the user's current TOML value.
                let movie = app.session.ram_view(0x68EFC..0x68EFC + 0x76C).unwrap();
                let dword =
                    |offset| u32::from_le_bytes(movie[offset..offset + 4].try_into().unwrap());
                if (dword(4), dword(8), dword(0xC), dword(0x678)) == (640, 480, 161, 153) {
                    assert_eq!(dword(0x768), 0, "skipped frames at {cpu_mhz} MHz");
                    let pixels = app.session.ram_view(0x1A690..0x1A690 + 640 * 480).unwrap();
                    let hash = pixels.iter().fold(0xcbf29ce484222325_u64, |h, b| {
                        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
                    });
                    if hash == 0xE1C26C1E2CA04DC3 {
                        matched = true;
                        println!(
                            "HoMM {cpu_mhz} MHz: frame=153 skipped=0 indexed_hash={hash:016x} steps={}",
                            app.session.steps()
                        );
                        break;
                    }
                }
            }
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
            assert!(
                matched,
                "did not reproduce reference frame 152 at {cpu_mhz} MHz"
            );
        }
    }

    #[test]
    #[ignore = "requires the user's local games and TOML profiles; runs without a window or audio device"]
    fn local_toml_profiles_load_real_games_without_audio() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in ["CIV", "PRINCE", "HoMnM"] {
            let profile = root.join(format!("games/{name}.toml"));
            let cli = crate::cli::Cli::parse_from([
                std::ffi::OsString::from("doslet"),
                "--config".into(),
                profile.into_os_string(),
                "--no-audio".into(),
            ]);
            let mut app = Doslet::from_cli(cli).unwrap();
            assert!(app.audio.is_none());
            assert!(!app.audio_engine.is_mt32());
            app.session.advance(1_000_000);
            assert_ne!(
                app.session.status(),
                SessionStatus::Faulted,
                "{name}: {:?}",
                app.session.fault()
            );
            println!(
                "{name}: {:?}, steps={}",
                app.session.status(),
                app.session.steps()
            );
        }
    }

    #[test]
    fn profile_arguments_working_directory_and_mode_survive_game_reset() {
        let fixture = crate::profile::tests::Fixture::new(
            r#"
version = 1
name = "Configured game"
[game]
executable = "../TINY.EXE"
arguments = ["/test", "two words"]
working_directory = 'C:\SUB'
[midi]
game_mode = "mt32"
[audio]
enabled = false
"#,
        );
        std::fs::create_dir(fixture.root.join("SUB")).unwrap();
        std::fs::write(fixture.root.join("SUB/DATA.DAT"), b"data").unwrap();
        let mut app = Doslet::from_cli(fixture.cli(&[])).unwrap();
        assert_eq!(app.name, "Configured game");
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert!(app.audio.is_none());
        let tail = b" /test \"two words\"";
        for _ in 0..2 {
            assert_eq!(
                app.session.ram_view(0x10080..0x10081).unwrap(),
                &[tail.len() as u8]
            );
            assert_eq!(
                app.session.ram_view(0x10081..0x10081 + tail.len()).unwrap(),
                tail
            );
            app.session.advance(100);
            assert_eq!(app.session.status(), SessionStatus::Exited(7));
            assert_eq!(app.session.console(), b"Relocated MZ");
            let _ = app.update(Message::Reset);
            assert_eq!(app.session.status(), SessionStatus::Running);
            assert_eq!(app.midi_mode, Mode::Mt32);
        }
    }

    #[test]
    fn incompatible_host_dialect_selection_keeps_the_existing_output_and_mode() {
        let mut app = Doslet::new();
        let (output, _receiver) = crate::native_midi::Output::mock();
        app.midi_choice = output.name.clone();
        app.host_midi = Some(output);
        let _ = app.update(Message::MidiOutputModeSelected(Mode::Mt32));
        assert_eq!(app.midi_output_mode, Mode::Gm);
        assert!(app.host_midi.is_some());
        assert!(app.notice.is_some());
        app.midi_mode = Mode::Mt32;
        app.routing.set_mode(Mode::Mt32);
        app.midi_output_mode = Mode::Mt32;
        let _ = app.update(Message::MidiModeSelected(Mode::Gm));
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert_eq!(app.routing.mode(), Mode::Mt32);
        assert!(app.host_midi.is_some());
    }
    #[test]
    fn native_rust_output_upgrades_roland_and_accepts_translated_notes() {
        let mut app = Doslet::new();
        let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, 0xBA, 0x30, 3];
        for byte in [
            0xF0, 0x41, 0x10, 0x16, 0x12, 0x20, 0, 0, b'A', 0x1F, 0xF7, 0x91, 60, 100,
        ] {
            append_midi_byte(&mut program, byte);
        }
        program.extend_from_slice(&[0xEB, 0xFE]);
        app.session = Session::new(&program).unwrap().into();
        app.session.start();
        app.session.advance(40000);
        app.process_audio().unwrap();
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert_eq!(app.routing.translator().stats().sysex, 1);
        assert_eq!(app.audio_engine.stats().notes, 1);
        assert!(app.midi_mode_label().contains("approximation"));
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires the local v86 module; no host audio device"]
    fn v86_midi_phrase_reaches_rust_synthesis_with_both_clocks() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let module = std::fs::read(root.join("target/v86/doslet-v86.wasm")).unwrap();
        for host_clock in [false, true] {
            let mut app = Doslet::new();
            let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE];
            for byte in [0xC0, 0, 0x90, 60, 100] {
                append_midi_byte(&mut program, byte);
            }
            program.extend_from_slice(&[0xEB, 0xFE]);
            app.session = Session::new(&program).unwrap().into();
            app.session.enable_v86(&module).unwrap();
            app.session.set_v86_host_clock(host_clock).unwrap();
            app.session.start();
            let began = Instant::now();
            while app.session.emulated_nanos() < 100_000_000 || app.session.midi_stats().1 < 5 {
                assert!(began.elapsed() < Duration::from_secs(10));
                app.tick();
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "{:?}; {:?}; {:?}",
                    app.session.fault(),
                    app.session.v86_last_error(),
                    app.notice
                );
            }
            assert_eq!(
                app.session.midi_stats().1,
                5,
                "host_clock={host_clock} pc={:x}",
                app.session.debug_pc()
            );
            assert_eq!(app.audio_engine.stats().notes, 1, "host_clock={host_clock}");
            let nanos = app.session.emulated_nanos() + 10_000_000;
            let synth = app.audio_engine.inline_mut().midi.as_mut().unwrap();
            let mut frames = [[0.0; 2]; 512];
            let count = synth.render_until(nanos, &mut frames).unwrap();
            assert!(
                frames[..count].iter().any(|f| *f != [0.0; 2]),
                "host_clock={host_clock}"
            );
        }
    }
    #[test]
    fn saturated_audio_worker_yields_before_advancing_the_pc_and_reset_recovers() {
        let mut app = Doslet::new();
        app.session = Session::new(&[0xEB, 0xFE]).unwrap().into();
        app.session.start();
        let output = crate::audio::Writer::mock(48000);
        let config = app.audio_config("Rust synth", 48000, false).unwrap();
        app.audio_engine
            .configure(config, Some(output.clone()))
            .unwrap();
        output.set_active(false);
        output.push(&vec![[0.5; 2]; output.available()]).unwrap();
        let now = Instant::now();
        app.tick_at(now);
        app.tick_at(now + Duration::from_secs(1));
        let steps = app.session.steps();
        assert!(steps > 0);
        app.tick_at(now + Duration::from_secs(2));
        assert_eq!(
            app.session.steps(),
            steps,
            "a full worker must not lose guest audio"
        );
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert!(app.notice.is_none(), "{:?}", app.notice);
        let _ = app.update(Message::Step);
        assert_eq!(app.session.steps(), steps);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert_eq!(app.notice, app.logged_notice);
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.emulated_nanos(), 0);
        assert_eq!(output.available(), 16384);
        assert_eq!(
            app.audio_engine.stats(),
            crate::native_audio::Stats::default()
        );
        assert!(app.audio_engine.ready().unwrap());
    }
    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires tools/v86-build; no game or audio device"]
    fn v86_host_clock_catchup_services_timer_edges_between_elapsed_slices() {
        use doslet_x86::Gpr16;
        // PIT channel 0 mode 2, divisor 17045 (~70 Hz); STI; HLT loop.
        let mut code = vec![
            0xb0, 0x34, 0xe6, 0x43, 0xb8, 0x95, 0x42, 0xe6, 0x40, 0x88, 0xe0, 0xe6, 0x40, 0xfb,
            0xf4, 0xeb, 0xfd,
        ];
        let handler = 0x100 + code.len() as u16;
        code.extend_from_slice(&[0x43, 0xb0, 0x20, 0xe6, 0x20, 0xcf]); // INC BX; EOI; IRET.
        let mut app = Doslet::new();
        app.session = Session::new(&code).unwrap().into();
        app.session
            .enable_v86(
                &std::fs::read(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../target/v86/doslet-v86.wasm"
                ))
                .unwrap(),
            )
            .unwrap();
        app.session.set_v86_host_clock(true).unwrap();
        let vector = [handler as u8, (handler >> 8) as u8, 0, 0x10];
        assert!(app.session.debug_write_memory(8 * 4, &vector));
        app.session.start();
        for _ in 0..30 {
            app.session.advance(4096);
            if app.session.debug_cpu().is_halted() {
                break;
            }
        }
        assert!(app.session.debug_cpu().is_halted());
        let initial = app.session.emulated_nanos();
        let now = Instant::now();
        // A stalled owner must deliver the elapsed timer events, rather than
        // coalescing all 200 ms into one PIC request before running the CPU.
        app.anchor = Some((now - Duration::from_millis(200), initial));
        for _ in 0..1000 {
            app.tick_at(Instant::now());
            if app.session.emulated_nanos() >= initial + 200_000_000
                && !app.session.debug_irq_pending()
                && app.session.debug_cpu().is_halted()
            {
                break;
            }
        }
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        let elapsed = app.session.emulated_nanos() - initial;
        let expected = elapsed * 1_193_182 / 1_000_000_000 / 17045;
        let delivered = u64::from(app.session.debug_cpu().registers.get(Gpr16::Bx));
        assert!(expected >= 14);
        assert!(
            delivered + 1 >= expected && delivered <= expected,
            "timer edges lost: delivered={delivered}, expected={expected}, elapsed={elapsed}"
        );
    }

    #[test]
    fn synchronous_learning_finishes_before_execution_and_rebases_host_time() {
        let mut app = Doslet::new();
        app.session = Session::new(&[0x90, 0xeb, 0xfd]).unwrap().into();
        app.session.start();
        let mut tiers = crate::native_speed_tiers::Controller::untrained();
        tiers.configure(Some(1), 1, 1, true);
        tiers.enable(&mut app.session);
        assert_eq!(tiers.stats.learning_jobs, 0);
        app.speed_tiers = Some(tiers);
        let before = (
            app.session.steps(),
            app.session.emulated_nanos(),
            app.session.debug_pc(),
        );
        let revision = app.presentation_revision;
        let now = Instant::now();
        app.anchor = Some((now - Duration::from_secs(30), 0));
        app.tick_at(now);
        assert_eq!(app.speed_tier_stats().unwrap().learning_jobs, 1);
        assert!(!app.speed_tier_stats().unwrap().learning_running);
        assert_eq!(app.speed_tier_stats().unwrap().learning_pending, 0);
        assert!(app.speed_tier_stats().unwrap().learning_changes > 0);
        assert!(app.presentation_revision > revision);
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert!(app.anchor.is_none());
        assert_eq!(
            (
                app.session.steps(),
                app.session.emulated_nanos(),
                app.session.debug_pc()
            ),
            before
        );
        // A fresh anchor excludes the whole profiling pause from guest time.
        let resumed = Instant::now();
        app.tick_at(resumed);
        assert_eq!(app.session.steps(), before.0);
        app.tick_at(resumed + Duration::from_millis(1));
        assert!(app.session.steps() > before.0);
        assert!(app.session.emulated_nanos() <= 1_100_000);
        app.session.pause();
        let paused = (app.session.steps(), app.session.emulated_nanos());
        let _ = app.worker_tick();
        assert_eq!(app.speed_tier_stats().unwrap().learning_jobs, 1);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert_eq!((app.session.steps(), app.session.emulated_nanos()), paused);
    }
    #[test]
    fn audio_worker_errors_reach_the_notice_even_when_the_pc_is_paused() {
        let mut app = Doslet::new();
        app.session.start();
        app.session.pause();
        let config = app.audio_config("Rust synth", 48000, false).unwrap();
        app.audio_engine
            .configure(config, Some(crate::audio::Writer::mock(48000)))
            .unwrap();
        assert!(app.audio_engine.ready().unwrap());
        app.audio_engine.receive_pcm(&[doslet_pcm::EMPTY]).unwrap();
        app.audio_engine.render_until(1_000_000, false).unwrap();
        app.audio_engine.submit().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.notice.is_none() {
            assert!(Instant::now() < deadline);
            let _ = app.worker_tick();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert!(app.notice.as_ref().unwrap().contains("invalid PCM frame"));
        assert_eq!(app.notice, app.logged_notice);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn apple_output_routes_mt32_replays_notes_and_survives_pause_and_reset() {
        let mut app = Doslet::new();
        app.midi_choice = APPLE_LABEL.into();
        app.audio_engine.inline_mut().midi = Some(Timeline::apple(48000).unwrap());
        app.midi_output_mode = Mode::Mt32; // Applies only to external ports.
        let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, 0xBA, 0x30, 3];
        for byte in [
            0xF0, 0x41, 0x10, 0x16, 0x12, 0x20, 0, 0, b'A', 0x1F, 0xF7, 0x91, 60, 100,
        ] {
            append_midi_byte(&mut program, byte);
        }
        program.extend_from_slice(&[0xEB, 0xFE]);
        app.session = Session::new(&program).unwrap().into();
        app.session.start();
        app.session.advance(40000);
        app.process_audio().unwrap();
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert_eq!(app.output_dialect(), Mode::Gm);
        assert_eq!(app.audio_engine.stats().notes, 1);
        let now = app.session.emulated_nanos();
        let mut restored = app.restored_synth(48000, APPLE_LABEL).unwrap();
        assert_eq!(restored.note_events(), 1);
        assert_eq!(
            restored
                .render_until(now, &mut app.audio_engine.inline_mut().samples)
                .unwrap(),
            0
        );
        let mut peak = 0.0_f32;
        loop {
            let count = restored
                .render_until(
                    now + 100_000_000,
                    &mut app.audio_engine.inline_mut().samples,
                )
                .unwrap();
            if count == 0 {
                break;
            }
            peak = app.audio_engine.inline_mut().samples[..count]
                .iter()
                .flatten()
                .fold(peak, |p, s| p.max(s.abs()));
        }
        assert!(peak > 0.0001, "restored translated note must produce PCM");
        let _ = app.update(Message::RunPause);
        let _ = app.update(Message::Tick);
        assert_eq!(app.session.emulated_nanos(), now);
        assert_eq!(app.audio_engine.stats().notes, 1);
        let _ = app.update(Message::RunPause);
        assert_eq!(app.audio_engine.stats().notes, 1);
        let _ = app.update(Message::Reset);
        assert_eq!(app.midi_choice, APPLE_LABEL);
        assert!(app.audio_engine.inline().midi.is_none());
        assert_eq!(app.audio_engine.stats().notes, 0);
        assert_eq!(app.session.emulated_nanos(), 0);
        assert!(app.notice.is_none(), "{:?}", app.notice);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn apple_output_is_rejected_without_changing_the_current_output() {
        let mut app = Doslet::new();
        let error = app.select_midi(APPLE_LABEL).unwrap_err();
        assert!(error.contains("only on native macOS"));
        assert_eq!(app.midi_choice, "Rust synth");
        assert!(!app.midi_choices.iter().any(|v| v == APPLE_LABEL));
    }

    #[test]
    fn game_restart_clears_midi_controls_sysex_and_automatic_dialect() {
        let mut app = Doslet::new();
        app.configurator.active.output.game_midi = "gm".into();
        app.midi_mode = Mode::Mt32; // Detected during the previous run.
        app.routing = Routing::new(Mode::Mt32);
        for byte in [
            0xb1, 7, 0, 0xb1, 11, 0, 0xb1, 64, 127, 0xc1, 48, 0x91, 60, 100, 0xf0, 0x41, 0x10,
            0x16, 0x12, 0x20, 0, 0, b'A', 0x1f, 0xf7,
        ] {
            app.routing.push(byte, Mode::Mt32, &mut |_| {}).unwrap();
        }
        let (mut output, rx) = crate::native_midi::Output::mock();
        output.set_active(false).unwrap();
        app.host_midi = Some(output);
        app.midi_output_mode = Mode::Mt32;
        app.clear_presentation();
        assert_eq!(app.midi_mode, Mode::Gm);
        let snapshot = |routing: &Routing| {
            let mut bytes = Vec::new();
            routing.replay(Mode::Mt32, &mut |message| bytes.extend_from_slice(message));
            bytes
        };
        assert_eq!(snapshot(&app.routing), snapshot(&Routing::new(Mode::Gm)));
        assert_eq!(app.audio_engine.stats().notes, 0);
        loop {
            let (_, message) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            if message == Mode::Mt32.reset_message() {
                break;
            }
            assert_eq!(message[0] & 0xf0, 0xb0);
        }
    }

    #[test]
    fn host_midi_lookahead_is_bounded_and_rebases_after_pause_and_reset() {
        let mut app = Doslet::new();
        app.session = Session::new(&[0xEB, 0xFE]).unwrap().into();
        app.session.start();
        let (mut output, _receiver) = crate::native_midi::Output::mock();
        output.set_active(true).unwrap();
        app.host_midi = Some(output);
        let start = Instant::now();
        app.tick_at(start);
        assert_eq!(app.session.steps(), u64::from(MAX_STEPS_PER_TICK));
        app.tick_at(start + Duration::from_millis(16));
        assert!((96_000_000..98_000_000).contains(&app.session.emulated_nanos()));
        let steps = app.session.steps();
        let _ = app.update(Message::RunPause);
        app.tick_at(start + Duration::from_secs(60));
        assert_eq!(app.session.steps(), steps);
        let _ = app.update(Message::RunPause);
        let guest = app.session.emulated_nanos();
        app.tick_at(start + Duration::from_secs(120));
        assert_eq!(app.session.steps() - steps, u64::from(MAX_STEPS_PER_TICK));
        assert!(app.session.emulated_nanos() - guest < 80_000_000);
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.emulated_nanos(), 0);
        assert!(app.anchor.is_none());
        assert!(app.notice.is_none());
        let _ = app.update(Message::RunPause);
        app.tick_at(start + Duration::from_secs(180));
        assert!(app.session.emulated_nanos() < 80_000_000);
    }

    #[test]
    fn worker_turn_yields_between_chunks_and_waits_only_when_caught_up_or_blocked() {
        let mut app = Doslet::new();
        app.session = Session::new(&[0x40, 0xEB, 0xFD]).unwrap().into();
        app.session.start();
        let now = Instant::now();
        app.anchor = Some((now - Duration::from_secs(1), 0));
        let mut chunks = 0;
        app.tick_while(now, || {
            chunks += 1;
            chunks <= 2
        });
        assert_eq!(app.session.steps(), u64::from(2 * CHUNK));
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.worker_delay(true), Duration::ZERO);
        assert_eq!(app.worker_delay(false), Duration::from_millis(1));
        app.anchor = Some((Instant::now(), 0));
        assert_eq!(app.worker_delay(true), Duration::from_millis(1));
        app.session.pause();
        assert_eq!(app.worker_delay(true), Duration::from_millis(16));
    }

    #[test]
    fn guest_timeslice_yield_ends_the_native_tick() {
        let mut app = Doslet::new();
        app.session = Session::new(&[
            0xB8, 0x80, 0x16, 0xCD, 0x2F, // Yield.
            0xB8, 0, 0x4C, 0xCD, 0x21, // Exit only on the next tick.
        ])
        .unwrap()
        .into();
        app.session.start();
        let start = Instant::now();
        app.tick_at(start);
        app.tick_at(start + Duration::from_millis(16));
        assert!(app.session.timeslice_yielded());
        assert_eq!(app.session.status(), SessionStatus::Running);
        app.tick_at(start + Duration::from_millis(32));
        assert_eq!(app.session.status(), SessionStatus::Exited(0));
    }

    #[test]
    fn archive_selection_failure_and_reset_preserve_the_active_game() {
        use std::{
            io::{Cursor, Write},
            sync::Arc,
        };
        use zip::{ZipWriter, write::SimpleFileOptions};
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["GAME/START.COM", "GAME/OTHER.COM"] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&[0xb8, 0, 0x4c, 0xcd, 0x21]).unwrap();
        }
        let mut app = Doslet::new();
        let _ = app.update(Message::ArchiveLoaded(Ok(Some(Arc::new(
            zip.finish().unwrap().into_inner(),
        )))));
        assert!(app.program.is_none());
        let _ = app.update(Message::ProgramSelected("GAME/START.COM".into()));
        let _ = app.update(Message::StartArchive);
        assert_eq!(app.name, "START.COM");
        app.session.advance(100);
        assert_eq!(app.session.status(), SessionStatus::Exited(0));
        let _ = app.update(Message::ProgramSelected("GAME/OTHER.COM".into()));
        let _ = app.update(Message::ArchiveLoaded(Ok(Some(Arc::new(
            b"invalid".to_vec(),
        )))));
        assert!(app.notice.is_some());
        let _ = app.update(Message::Reset);
        assert_eq!(app.name, "START.COM");
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.session.steps(), 0);
        assert!(app.notice.is_none());
        let _ = app.update(Message::ArchiveLoaded(Ok(None)));
        assert!(app.archive.is_some());
    }

    #[test]
    fn controller_notifications_update_a_paused_session_without_advancing_it() {
        use crate::native_gamepad::{GamepadUpdate, NativeGamepads, PadState, uuid_string};
        let mut app = Doslet::new();
        app.session.start();
        app.session.advance(25);
        app.session.pause();
        let steps = app.session.steps();
        let nanos = app.session.emulated_nanos();
        let (pads, send) = NativeGamepads::test_pair();
        app.gamepads = Some(pads);
        let uuid = [0xAB; 16];
        send(GamepadUpdate::Connected {
            id: 0,
            name: "Test controller".into(),
            uuid: uuid_string(uuid),
            generation: 1,
        });
        send(GamepadUpdate::State(PadState {
            id: 0,
            uuid,
            generation: 1,
            sequence: 1,
            resync: true,
            ambiguous: false,
            axes: [32767, -32767, 0, 0, 0, 0, 0, 0],
            buttons: 1,
        }));
        let _ = app.update(Message::GamepadsReady);
        assert!(
            app.configurator
                .controllers
                .iter()
                .any(|v| v.contains("Test controller"))
        );
        assert_eq!(app.pad_generation, Some(1));
        assert_eq!(app.pad_axes, [32767, -32767]);
        assert_eq!(app.pad_buttons, [true, false]);
        send(GamepadUpdate::Disconnected {
            id: 0,
            uuid: uuid_string(uuid),
            generation: 1,
            sequence: 5,
        });
        let _ = app.update(Message::GamepadsReady);
        assert_eq!(app.pad_axes, [0; 2]);
        assert_eq!(app.pad_buttons, [false; 2]);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert_eq!(app.session.steps(), steps);
        assert_eq!(app.session.emulated_nanos(), nanos);
        assert!(app.notice.is_none(), "{:?}", app.notice);
    }

    #[test]
    fn pause_does_not_accumulate_host_time_and_reset_restores_demo() {
        let mut app = Doslet::new();
        let start = Instant::now();
        let _ = app.update(Message::RunPause);
        app.tick_at(start);
        app.tick_at(start + Duration::from_micros(100));
        assert!(app.session.steps() > 0);
        let _ = app.update(Message::RunPause);
        let steps = app.session.steps();
        app.tick_at(start + Duration::from_secs(60));
        assert_eq!(app.session.steps(), steps);
        let _ = app.update(Message::RunPause);
        app.tick_at(start + Duration::from_secs(120));
        assert_eq!(app.session.steps(), steps);
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.status(), SessionStatus::Ready);
        assert_eq!(app.session.steps(), 0);
        assert_eq!(app.audio_engine.stats().notes, 0);
    }

    #[test]
    fn live_cpu_clock_keeps_session_and_reset_recipe() {
        let mut app = Doslet::new();
        app.session.start();
        app.session.advance(50);
        let registers = format!("{:?}", app.session.registers());
        let nanos = app.session.emulated_nanos();
        let steps = app.session.steps();
        let epoch = app.session.epoch();
        let _ = app.update(Message::CpuMhz(133));
        assert_eq!(format!("{:?}", app.session.registers()), registers);
        assert_eq!(app.session.emulated_nanos(), nanos);
        assert_eq!(app.session.steps(), steps);
        assert_eq!(app.session.epoch(), epoch);
        assert_eq!(app.session.hardware_config().cpu_mhz, 133);
        assert_eq!(app.launch.hardware.cpu_mhz, 133);
        assert_eq!(app.configurator.active.hardware.cpu_mhz, 133);
        let _ = app.update(Message::CpuMhz(0));
        assert_eq!(app.session.hardware_config().cpu_mhz, 133);
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.hardware_config().cpu_mhz, 133);
    }

    #[test]
    #[ignore = "requires local Heroes/ISO; observes menu idle with audio synthesis, without a host output device"]
    fn local_heroes_menu_idle_experiment() {
        heroes_idle_experiment(false);
    }

    #[test]
    #[ignore = "requires local Heroes/ISO; verifies learned idle with audio synthesis"]
    fn local_heroes_learned_idle_experiment() {
        heroes_idle_experiment(true);
    }

    fn heroes_analysis_app(profile: bool) -> Doslet {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut app = Doslet::new();
        let mut settings = crate::native_input::LaunchSettings {
            cdrom: Some((b'D', root.join("games/HoMnM/homm1.gog"))),
            no_dynamic_core: true,
            ..Default::default()
        };
        if profile {
            use clap::Parser;
            let config = root.join("games/HoMnM.toml");
            settings = crate::cli::Cli::try_parse_from([
                std::ffi::OsStr::new("doslet"),
                std::ffi::OsStr::new("--config"),
                config.as_os_str(),
                std::ffi::OsStr::new("--no-dynamic-core"),
            ])
            .unwrap()
            .resolve()
            .unwrap()
            .launch;
            // Keep the established deterministic startup/Esc schedule. The
            // adaptive measurement restores the profile's 133 MHz ceiling.
            settings.hardware.cpu_mhz = 33;
            // This helper uses the synchronous C:/D: mount adapter, without the
            // desktop's asynchronous machine-spec storage setup.
            settings.machine_spec = None;
            assert!(settings.cdrom.is_some());
            assert!(!settings.hardware.mpu401);
        }
        app.session = crate::native_zip::load_game_with_settings(
            &root.join("games/HoMnM/HEROES.EXE"),
            &settings,
        )
        .unwrap()
        .into();
        app.session.start();
        let start = Instant::now();
        while app.session.steps() < 350_000_000 {
            let at = app.session.steps();
            let next = if at < 100_000_000 {
                100_000_000
            } else {
                350_000_000
            };
            app.session.advance((next - at).min(10000) as u32);
            if app.session.steps() == 100_000_000 {
                app.session.push_key(1, 0x1b).unwrap();
            }
            app.process_audio().unwrap();
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
            assert!(start.elapsed() < Duration::from_secs(120));
        }
        app
    }

    #[test]
    #[ignore = "requires local read-only Heroes/ISO; records bounded synchronization diagnostics"]
    fn local_heroes_synchronization_analysis() {
        let mut app = heroes_analysis_app(false);
        for enabled in [false, true].into_iter().cycle().take(6) {
            app.session.set_synchronization_analysis(enabled);
            let began = Instant::now();
            let target = app.session.steps() + 4_000_000;
            while app.session.steps() < target {
                app.session.advance(10000);
                app.process_audio().unwrap();
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    app.session.fault()
                );
            }
            println!(
                "SYNC enabled={enabled} elapsed={:?} idle={:?} status={:?} effects={:?}",
                began.elapsed(),
                app.session.idle_loop_stats(),
                app.session.synchronization_analysis(),
                app.session.synchronization_caller_effects()
            );
        }
    }

    #[test]
    #[ignore = "requires local read-only Heroes/ISO; headless adaptive menu evidence, no audible playback"]
    fn local_heroes_adaptive_recurrence() {
        for dynamic in [false, true] {
            let mut app = heroes_analysis_app(true);
            app.session.set_dynamic_core(dynamic);
            app.session.set_cpu_mhz(133).unwrap();
            app.session.set_adaptive_cpu(true);
            let start = app.session.emulated_nanos();
            let host = Instant::now();
            let mut next = start + 1_000_000_000;
            let mut minimum = 133;
            while app.session.emulated_nanos() < start + 60_000_000_000 {
                app.session.advance(10000);
                app.process_audio().unwrap();
                app.session.prepare_dynamic(8);
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    app.session.fault()
                );
                minimum = minimum.min(app.session.hardware_config().cpu_mhz);
                if app.session.emulated_nanos() >= next {
                    let s = app.session.synchronization_analysis();
                    println!(
                        "RECURRENCE jit={dynamic} seconds={} minimum={minimum} host={:?} mhz={} down={} up={} reason={:?} recovery={:?} measured_wait={} episodes={:?}",
                        (next - start) / 1_000_000_000,
                        host.elapsed(),
                        s.effective_mhz,
                        s.reductions,
                        s.recoveries,
                        s.reason,
                        s.last_recovery_reason,
                        s.recurrence.measured_wait_nanos,
                        s.candidates.map(|c| (
                            c.head,
                            c.episodes,
                            c.work_nanos,
                            c.period_nanos,
                            c.missed_deadlines,
                            c.predicted_mhz
                        )),
                    );
                    next += 1_000_000_000;
                }
            }
            assert!(
                app.session
                    .synchronization_analysis()
                    .recurrence
                    .measured_wait_nanos
                    > 0,
                "no verified waiting for jit={dynamic}: {:?}",
                app.session.synchronization_analysis()
            );
            assert!(minimum < 133, "no automatic reduction for jit={dynamic}");
            assert!(
                app.session.hardware_config().cpu_mhz < 133,
                "controller stayed at its ceiling for jit={dynamic}: {:?}",
                app.session.synchronization_analysis()
            );
            println!(
                "RECURRENCE_FINAL jit={dynamic} status={:?}",
                app.session.synchronization_analysis()
            );
            println!(
                "RECURRENCE_NATIVE jit={dynamic} stats={:?}",
                app.session.dynamic_stats()
            );
            println!(
                "RECURRENCE_AUDIO jit={dynamic} stats={:?}",
                app.session.audio_status()
            );
            if dynamic {
                assert!(app.session.dynamic_stats().executed_steps > 0);
            }
            app.session.set_adaptive_cpu(false);
            assert_eq!(app.session.hardware_config().cpu_mhz, 133);
        }
    }

    #[test]
    #[ignore = "requires local read-only Heroes/ISO and TUTORIAL save; headless city evidence, no audible playback"]
    fn local_heroes_city_adaptive_recurrence() {
        fn advance(app: &mut Doslet, millis: u64) {
            let target = app.session.emulated_nanos() + millis * 1_000_000;
            let deadline = Instant::now() + Duration::from_secs(60);
            while app.session.emulated_nanos() < target {
                app.session.advance(10000);
                app.process_audio().unwrap();
                app.session.prepare_dynamic(8);
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    app.session.fault()
                );
                assert!(Instant::now() < deadline);
            }
        }
        fn click(app: &mut Doslet, x: f64, y: f64) {
            // Reset the game's independent relative cursor at its top-left
            // bound; DOS driver's absolute position is not authoritative.
            app.session.pointer_relative(-1000.0, -1000.0);
            advance(app, 250);
            app.session.pointer_relative(x, y);
            advance(app, 250);
            app.session.pointer_button(0, true);
            advance(app, 250);
            app.session.pointer_button(0, false);
            advance(app, 500);
        }
        fn screenshot(app: &Doslet, scene: &str) {
            let frame = app.session.frame().unwrap();
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
            let file =
                std::fs::File::create(root.join(format!("heroes-headless-{scene}.png"))).unwrap();
            let mut encoder = png::Encoder::new(file, 640, 480);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_palette(
                frame
                    .palette
                    .iter()
                    .flat_map(|rgba| rgba[..3].iter().copied())
                    .collect::<Vec<_>>(),
            );
            encoder
                .write_header()
                .unwrap()
                .write_image_data(frame.pixels)
                .unwrap();
        }
        let mut app = heroes_analysis_app(true);
        app.session.set_dynamic_core(true);
        app.session.set_cpu_mhz(133).unwrap();
        advance(&mut app, 1000);
        screenshot(&app, "menu");
        click(&mut app, 495.0, 173.0);
        click(&mut app, 495.0, 108.0);
        screenshot(&app, "load");
        click(&mut app, 465.0, 63.0);
        click(&mut app, 395.0, 305.0);
        advance(&mut app, 2000);
        screenshot(&app, "map");
        click(&mut app, 580.0, 192.0);
        click(&mut app, 580.0, 192.0);
        screenshot(&app, "city");
        app.session.set_adaptive_cpu(true);
        let audio = app.session.audio_status().dsp;
        for second in 1..=240 {
            advance(&mut app, 1000);
            let s = app.session.synchronization_analysis();
            println!(
                "CITY second={second} mhz={} reductions={} recoveries={} reason={:?} recovery={:?} recurrence={:?} candidates={:?}",
                s.effective_mhz,
                s.reductions,
                s.recoveries,
                s.reason,
                s.last_recovery_reason,
                s.recurrence,
                s.candidates
            );
        }
        screenshot(&app, "city-adaptive");
        let s = app.session.synchronization_analysis();
        assert!(
            s.recurrence.backedge && s.recurrence.measured_wait_nanos > 0,
            "{s:?}"
        );
        assert!(s.effective_mhz < 133, "{s:?}");
        let audio_after = app.session.audio_status().dsp;
        let audio_checks = audio_after.block_irq_deadlines - audio.block_irq_deadlines;
        let audio_misses = audio_after.block_irq_deadline_misses - audio.block_irq_deadline_misses;
        println!("CITY_AUDIO checks={audio_checks} misses={audio_misses}");
        assert!(
            audio_checks >= 4000,
            "the TUTORIAL music DMA stream must stay active throughout the 240-second run"
        );
        assert_eq!(audio_misses, 0);
        assert!(
            s.candidates
                .iter()
                .any(|c| c.port && c.cadence_mean_nanos.is_some())
        );
        for c in s
            .candidates
            .iter()
            .filter(|c| c.port && c.reference_period_nanos != 0)
        {
            if let Some(mean) = c.cadence_mean_nanos {
                assert!(
                    mean <= c.reference_period_nanos * 33 / 32 + 100_000,
                    "slow presentation: {c:?}"
                );
            }
        }
        assert!(app.session.dynamic_stats().executed_steps > 0);
    }

    #[test]
    #[ignore = "requires local read-only Heroes/ISO; alternating warm analyzer overhead windows"]
    fn local_heroes_recurrence_overhead() {
        for dynamic in [false, true] {
            let mut app = heroes_analysis_app(true);
            app.session.set_dynamic_core(dynamic);
            app.session.set_cpu_mhz(133).unwrap();
            let warm = app.session.emulated_nanos() + 2_000_000_000;
            while app.session.emulated_nanos() < warm {
                app.session.advance(10000);
                app.process_audio().unwrap();
                app.session.prepare_dynamic(8);
                assert_eq!(app.session.status(), SessionStatus::Running);
            }
            // Alternate pair order. Each pair uses adjacent menu windows, not
            // an identical saved machine state; host wall time is not CPU time.
            for enabled in [false, true, true, false, false, true] {
                app.session.set_synchronization_analysis(enabled);
                let settle = app.session.emulated_nanos() + 1_000_000_000;
                while app.session.emulated_nanos() < settle {
                    app.session.advance(10000);
                    app.process_audio().unwrap();
                    app.session.prepare_dynamic(8);
                    assert_eq!(app.session.status(), SessionStatus::Running);
                }
                let start = app.session.emulated_nanos();
                let began = Instant::now();
                let cpu = crate::native_computer::process_cpu_ticks();
                while app.session.emulated_nanos() < start + 2_000_000_000 {
                    app.session.advance(10000);
                    app.process_audio().unwrap();
                    app.session.prepare_dynamic(8);
                    assert_eq!(app.session.status(), SessionStatus::Running);
                }
                println!(
                    "RECURRENCE_COST jit={dynamic} enabled={enabled} guest={} host={:?} cpu_ticks={:?} mhz={} wait={} native_steps={}",
                    app.session.emulated_nanos() - start,
                    began.elapsed(),
                    crate::native_computer::process_cpu_ticks()
                        .zip(cpu)
                        .and_then(|(after, before)| after.checked_sub(before)),
                    app.session.hardware_config().cpu_mhz,
                    app.session
                        .synchronization_analysis()
                        .recurrence
                        .measured_wait_nanos,
                    app.session.dynamic_stats().executed_steps
                );
            }
        }
    }

    fn heroes_idle_experiment(learned: bool) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut app = Doslet::new();
        let settings = crate::native_input::LaunchSettings {
            cdrom: Some((b'D', root.join("games/HoMnM/homm1.gog"))),
            no_dynamic_core: true,
            ..Default::default()
        };
        app.session = crate::native_zip::load_game_with_settings(
            &root.join("games/HoMnM/HEROES.EXE"),
            &settings,
        )
        .unwrap()
        .into();
        app.session.start();
        let start = Instant::now();
        while app.session.steps() < 350_000_000 {
            let at = app.session.steps();
            let next = if at < 100_000_000 {
                100_000_000
            } else {
                350_000_000
            };
            app.session.advance((next - at).min(10000) as u32);
            if app.session.steps() == 100_000_000 {
                app.session.push_key(1, 0x1b).unwrap();
            }
            app.process_audio().unwrap();
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
            assert!(start.elapsed() < Duration::from_secs(120));
        }
        app.session.set_cpu_mhz(133).unwrap();
        if learned {
            app.session.set_adaptive_idle(true);
            app.session.pointer_move(320, 240);
            let until = app.session.emulated_nanos() + 10_000_000_000;
            let began = Instant::now();
            while app.session.emulated_nanos() < until {
                app.session.advance(10000);
                app.process_audio().unwrap();
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    app.session.fault()
                );
            }
            println!(
                "Heroes learned idle host={:?} idle={:?} sync={:?} PCM={:?}",
                began.elapsed(),
                app.session.menu_idle_stats(),
                app.session.synchronization_stats(),
                app.session.audio_status()
            );
            assert!(app.session.menu_idle_stats().nanos > 1_000_000_000);
            return;
        }
        for enabled in [false, true].into_iter().cycle().take(10) {
            app.session.set_idle_observation(true);
            crate::native_input::set_heroes_menu_idle(&mut app.session, enabled);
            let until = app.session.emulated_nanos() + 500_000_000;
            let began = Instant::now();
            while app.session.emulated_nanos() < until {
                app.session.advance(10000);
                app.process_audio().unwrap();
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    app.session.fault()
                );
            }
            println!(
                "Heroes menu idle={enabled} host={:?} stats={:?} PCM={:?} observer={:?} sync={:?}",
                began.elapsed(),
                app.session.menu_idle_stats(),
                app.session.audio_status(),
                app.session.idle_observation(),
                app.session.synchronization_stats(),
            );
            if enabled {
                assert!(app.session.menu_idle_stats().waits > 0);
            }
        }
    }

    #[test]
    #[ignore = "requires the user's local PRINCE installation; run from repository root"]
    fn local_prince_moont_queue_survives_repeated_restoration() {
        check_local_prince_moont_queue(121_000_000_000, true);
    }

    #[test]
    #[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs"]
    fn local_moont_expression_flood_obeys_uart_backpressure() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("DOSLET_MT32_ROM_DIR").expect("set DOSLET_MT32_ROM_DIR"),
        );
        // A guest driver that sends CC 11 as fast as write-ready permits, like
        // PRINCE's music fade at Doge's Palace. More than moont's 4096 events.
        let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE];
        for byte in [0x90, 60, 100] {
            append_midi_byte(&mut program, byte);
        }
        program.extend_from_slice(&[0xB9, 0x70, 0x17]); // CX = 6000 messages.
        let start = program.len();
        for byte in [0xB0, 11, 63] {
            append_midi_byte(&mut program, byte);
        }
        let jump = (start as isize - program.len() as isize - 2) as i8;
        program.extend_from_slice(&[0xE2, jump as u8]);
        for byte in [0x80, 60, 0, 0x90, 64, 100] {
            append_midi_byte(&mut program, byte);
        }
        program.extend_from_slice(&[0xB8, 0, 0x4C, 0xCD, 0x21]);
        for threaded in [false, true] {
            let mut app = Doslet::new();
            app.session = Session::new(&program).unwrap().into();
            app.session.set_cpu_mhz(1).unwrap();
            let output = crate::audio::Writer::mock(48000);
            app.audio_engine
                .configure(
                    crate::native_audio::Config {
                        source: crate::native_audio::Source::Mt32 {
                            directory: directory.clone(),
                            mode: Mode::Gm,
                        },
                        rate: 48000,
                        nanos: 0,
                        replay: Vec::new(),
                    },
                    threaded.then(|| output.clone()),
                )
                .unwrap();
            app.session.start();
            let deadline = Instant::now() + Duration::from_secs(30);
            while app.session.status() == SessionStatus::Running {
                output.consume(512);
                assert!(Instant::now() < deadline, "audio worker made no progress");
                if !app.audio_engine.ready().unwrap() {
                    std::thread::yield_now();
                    continue;
                }
                app.session.advance(256);
                app.process_audio().unwrap();
            }
            while app.audio_engine.pending_nanos() != 0 {
                output.consume(512);
                app.audio_engine.check().unwrap();
                assert!(Instant::now() < deadline, "audio worker did not finish");
                std::thread::yield_now();
            }
            assert_eq!(
                app.session.status(),
                SessionStatus::Exited(0),
                "{:?}",
                app.session.fault()
            );
            assert_eq!(app.session.midi_stats().1, 18_009);
            assert_eq!(app.audio_engine.stats().notes, 2);
            assert!(app.session.emulated_nanos() >= 18_008 * 320_000);
            eprintln!(
                "CM-32L CC11 flood: threaded={threaded}, bytes={}, guest={} ms",
                app.session.midi_stats().1,
                app.session.emulated_nanos() / 1_000_000
            );
        }
    }

    #[test]
    #[ignore = "requires local PRINCE and DOSLET_MT32_ROM_DIR; run from repository root"]
    fn local_prince_moont_queue_survives_ten_guest_minutes() {
        check_local_prince_moont_queue(600_000_000_000, false);
    }

    #[test]
    #[ignore = "requires local games/civ; never writes back guest files or opens host audio"]
    fn local_civ_midi_startup_diagnostic() {
        check_local_civ_midi_startup(false, false);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "requires local CIV, CM-32L ROMs and PULSE_SINK=doslet-audio-probe-output; no physical audio"]
    fn local_civ_system_mixer_loopback_without_v86() {
        use clap::Parser;
        use doslet_pulse::{connect, protocol as p, wait};
        assert_eq!(
            std::env::var("PULSE_SINK").unwrap(),
            "doslet-audio-probe-output"
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let config = root.join("games/CIV.toml");
        let client = connect().unwrap();
        let sink = wait(client.sink_info_by_name(c"doslet-audio-probe-output".into()))
            .expect("create the dedicated null sink before recording");
        assert_eq!(sink.sample_spec.channels, 2);
        wait(client.source_info_by_name(c"doslet-audio-probe-output.monitor".into()))
            .expect("the dedicated null sink monitor must exist");
        let info = wait(client.server_info()).unwrap();
        eprintln!(
            "CIV PULSE server={:?} version={:?}; verified dedicated null sink and monitor",
            info.server_name, info.server_version
        );
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<[f32; 2]>::with_capacity(
            48000 * 12,
        )));
        let capture = captured.clone();
        let record = wait(client.create_record_stream(
            p::RecordStreamParams {
                sample_spec: p::SampleSpec {
                    format: p::SampleFormat::Float32Le,
                    channels: 2,
                    sample_rate: 48000,
                },
                channel_map: p::ChannelMap::stereo(),
                source_name: Some(c"doslet-audio-probe-output.monitor".into()),
                buffer_attr: p::stream::BufferAttr {
                    fragment_size: 480 * 8,
                    ..Default::default()
                },
                ..Default::default()
            },
            move |bytes: &[u8]| {
                let mut frames = capture.lock().unwrap();
                for frame in bytes.as_chunks::<8>().0 {
                    if frames.len() < frames.capacity() {
                        frames.push([
                            f32::from_le_bytes(frame[..4].try_into().unwrap()),
                            f32::from_le_bytes(frame[4..].try_into().unwrap()),
                        ]);
                    }
                }
            },
        ))
        .unwrap();
        let cli = crate::cli::Cli::parse_from([
            "doslet",
            "--config",
            config.to_str().unwrap(),
            "--drive-mode",
            "overlay",
        ])
        .resolve()
        .unwrap();
        let mut app = Doslet::new();
        app.launch = cli.launch;
        app.launch.preload_local_media = true;
        app.configurator
            .set_active(app.launch.machine_spec.clone().unwrap());
        app.midi_mode = cli.midi_mode.unwrap().into();
        app.routing.set_mode(app.midi_mode);
        app.mt32_rom_dir = cli.mt32_rom_dir;
        app.audio_mixer = crate::audio::Mixer::System;
        app.audio_deferred = true;
        app.audio = Some(
            crate::audio::Output::new(crate::audio::Backend::Pulse, crate::audio::Mixer::System)
                .unwrap(),
        );
        app.select_midi("CM-32L / MT-32").unwrap();
        app.init_game_session(cli.game.unwrap()).unwrap();
        assert!(!app.session.v86_enabled());
        let started = Instant::now();
        let inject_stalls = std::env::var_os("DOSLET_AUDIO_PROBE_STALLS").is_some();
        let mut next_stall = Duration::from_millis(2500);
        let mut stalls = 0;
        while app.session.emulated_nanos() < 8_000_000_000
            && started.elapsed() < Duration::from_secs(20)
        {
            if inject_stalls && stalls < 3 && started.elapsed() >= next_stall {
                // Host/UI scheduling can stop the PC owner while the audio
                // server keeps consuming. No guest pause or anchor rebase.
                std::thread::sleep(Duration::from_millis(50));
                next_stall += Duration::from_secs(1);
                stalls += 1;
            }
            let _ = app.worker_tick();
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}; {:?}",
                app.session.fault(),
                app.notice
            );
            std::thread::yield_now();
        }
        assert!(app.session.emulated_nanos() >= 8_000_000_000);
        assert_eq!(stalls, if inject_stalls { 3 } else { 0 });
        std::thread::sleep(Duration::from_millis(150));
        app.audio.as_ref().unwrap().set_active(false);
        wait(record.cork()).unwrap();
        let frames = captured.lock().unwrap();
        let audible = |f: &[f32; 2]| f.iter().any(|v| v.abs() > 0.000001);
        let first = frames
            .iter()
            .position(audible)
            .expect("no MIDI in system mixer monitor");
        let last = frames.iter().rposition(audible).unwrap();
        let mut longest = 0;
        let mut run = 0;
        let mut silence = 0;
        for (index, frame) in frames[first..=last].iter().enumerate() {
            if audible(frame) {
                if run >= 480 {
                    eprintln!(
                        "CIV PULSE gap at {:.3}s, duration {:.3}ms",
                        (first + index - run) as f64 / 48000.0,
                        run as f64 / 48.0
                    );
                }
                run = 0;
            } else {
                run += 1;
                silence += 1;
                longest = longest.max(run);
            }
        }
        eprintln!(
            "CIV SYSTEM native stalls={stalls} guest={} wall={:?} frames={} first={first} last={last} zero_frames={silence} longest_zero_run={longest} sink={:?} notes={}",
            app.session.emulated_nanos(),
            started.elapsed(),
            frames.len(),
            app.audio.as_ref().unwrap().stats(),
            app.audio_engine.stats().notes
        );
        assert!(last - first > 48000);
        assert!(
            longest < 480,
            "system mixer interruption: {longest} frames at 48 kHz"
        );
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local CIV, CM-32L ROMs and v86; captures actual app audio without a host device"]
    fn local_civ_app_audio_matches_between_cpu_backends() {
        check_local_civ_app_audio(false, 8, None, false, None);
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local CIV, CM-32L ROMs and v86; no host device"]
    fn local_civ_host_clock_audio_reserve_comparison() {
        check_local_civ_app_audio(true, 8, None, false, None);
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local CIV, CM-32L ROMs and v86; measures MIDI and BIOS timer cadence"]
    fn local_civ_tempo_diagnostic() {
        check_local_civ_app_audio(false, 30, None, false, None);
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local CIV, CM-32L ROMs and v86; records bounded startup frames with one ESC tap"]
    fn local_civ_escape_animation_diagnostic() {
        check_local_civ_app_audio(false, 12, None, true, None);
        check_local_civ_app_audio(false, 12, Some(4_000_000_000), true, None);
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local CIV, CM-32L ROMs and v86; compares title animation without VGA trapping"]
    fn local_civ_v86_animation_without_vga_trap_diagnostic() {
        check_local_civ_app_audio(true, 12, Some(4_000_000_000), true, Some(false));
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local CIV, CM-32L ROMs and v86; compares title animation with VGA trapping"]
    fn local_civ_v86_animation_with_vga_trap_diagnostic() {
        check_local_civ_app_audio(true, 12, Some(4_000_000_000), true, Some(true));
    }

    #[cfg(feature = "v86")]
    fn check_local_civ_app_audio(
        host_only: bool,
        seconds: u64,
        escape_at: Option<u64>,
        capture: bool,
        vga_trap: Option<bool>,
    ) {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let config = root.join("games/CIV.toml");
        let module = root.join("target/v86/doslet-v86.wasm");
        let mut reference_cadence = None;
        for (v86, host_clock, reserve) in [
            (false, false, false),
            (true, false, false),
            (true, true, false),
            (true, true, true),
        ] {
            if (host_only && !host_clock) || (seconds > 8 && v86 && (!host_clock || !reserve)) {
                continue;
            }
            let mut args = vec![
                "doslet",
                "--config",
                config.to_str().unwrap(),
                "--drive-mode",
                "overlay",
            ];
            if v86 {
                args.extend(["--v86", "--v86-module", module.to_str().unwrap()]);
            }
            if v86 && !host_clock {
                args.push("--v86-instruction-clock");
            }
            let cli = crate::cli::Cli::parse_from(args).resolve().unwrap();
            let mut app = Doslet::new();
            app.launch = cli.launch;
            app.launch.preload_local_media = true;
            app.configurator
                .set_active(app.launch.machine_spec.clone().unwrap());
            app.midi_mode = cli.midi_mode.unwrap().into();
            app.routing.set_mode(app.midi_mode);
            app.mt32_rom_dir = cli.mt32_rom_dir;
            app.audio_deferred = true;
            app.audio = Some(crate::audio::Output::mock(48000));
            let writer = app.audio.as_ref().unwrap().writer();
            app.select_midi("CM-32L / MT-32").unwrap();
            app.init_game_session(cli.game.unwrap()).unwrap();
            if let Some(enabled) = vga_trap {
                app.session.set_v86_vga_polling(enabled);
            }
            app.session.set_v86_profiling(v86);
            let began = Instant::now();
            let mut consumed = 0_u64;
            let mut nonzero = 0_u64;
            let mut peak = 0_f32;
            let mut zero_after_signal = 0_u64;
            let mut zero_run = 0_u64;
            let mut longest_zero_run = 0_u64;
            let mut last_underruns = 0;
            let mut reported_gaps = 0;
            let report_period = if capture {
                1_000_000_000
            } else {
                5_000_000_000
            };
            let mut next_report = report_period;
            let mut escape_sent = false;
            let mut warmup_cadence = None;
            while app.session.emulated_nanos() < seconds * 1_000_000_000
                && began.elapsed() < Duration::from_secs(60)
            {
                let due = (began.elapsed().as_nanos() * 48000 / 1_000_000_000) as u64;
                let count = due.saturating_sub(consumed).min(4096) as usize;
                for frame in writer.consume(count) {
                    if frame != [0.0; 2] {
                        nonzero += 1;
                        zero_run = 0;
                    } else if nonzero != 0 {
                        zero_after_signal += 1;
                        zero_run += 1;
                        longest_zero_run = longest_zero_run.max(zero_run);
                    }
                    peak = peak.max(frame[0].abs()).max(frame[1].abs());
                }
                consumed += count as u64;
                let underruns = writer.stats().1;
                if underruns > last_underruns && nonzero != 0 && reported_gaps < 8 {
                    eprintln!(
                        "CIV GAP reserve={reserve} wall={:?} missing={} pending={}ns queued={} frames",
                        began.elapsed(),
                        underruns - last_underruns,
                        app.audio_engine.pending_nanos(),
                        16384 - writer.available()
                    );
                    reported_gaps += 1;
                }
                last_underruns = underruns;
                let _ = app.worker_tick();
                if !escape_sent
                    && app.audio_engine.stats().notes != 0
                    && escape_at.is_some_and(|at| app.session.emulated_nanos() >= at)
                {
                    assert!(app.session.push_key(0x01, 0x1b).unwrap());
                    escape_sent = true;
                    eprintln!(
                        "CIV ESC v86={v86} guest={} notes={}",
                        app.session.emulated_nanos(),
                        app.audio_engine.stats().notes
                    );
                }
                if app.session.emulated_nanos() >= next_report {
                    let mut ticks = [0; 4];
                    assert!(app.session.debug_read_memory(0x46c, &mut ticks));
                    eprintln!(
                        "CIV TEMPO v86={v86} guest={} wall={:?} bios_ticks={} irqs={} notes={} pc={:x}",
                        app.session.emulated_nanos(),
                        began.elapsed(),
                        u32::from_le_bytes(ticks),
                        app.session.debug_irq_entries(),
                        app.audio_engine.stats().notes,
                        app.session.debug_pc()
                    );
                    if warmup_cadence.is_none() {
                        warmup_cadence =
                            Some((u32::from_le_bytes(ticks), app.session.debug_irq_entries()));
                    }
                    if capture {
                        let pc = app.session.debug_pc();
                        let mut code = [0; 64];
                        app.session
                            .debug_read_memory(pc.saturating_sub(16), &mut code);
                        eprintln!(
                            "CIV ANIMATION v86={v86} escape={escape_at:?} second={} regs={:?} code={code:02x?}",
                            next_report / 1_000_000_000,
                            app.session.debug_cpu().registers
                        );
                        if let Some(frame) = app.session.frame() {
                            let rgba: Vec<_> = frame
                                .indices()
                                .flat_map(|p| frame.palette[usize::from(p)])
                                .collect();
                            let path = root.join(format!(
                                "target/civ-startup-v86-{v86}-esc-{}-vga-{vga_trap:?}-{}.png",
                                escape_at.is_some(),
                                next_report / 1_000_000_000
                            ));
                            let mut encoder = png::Encoder::new(
                                std::fs::File::create(path).unwrap(),
                                frame.width as u32,
                                frame.height as u32,
                            );
                            encoder.set_color(png::ColorType::Rgba);
                            encoder.set_depth(png::BitDepth::Eight);
                            encoder
                                .write_header()
                                .unwrap()
                                .write_image_data(&rgba)
                                .unwrap();
                        }
                    }
                    next_report += report_period;
                }
                if host_clock && !reserve {
                    // Diagnostic reference: disable the reserve even when
                    // deferred device activation has just configured it.
                    writer.set_host_clock_reserve(false);
                }
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "v86={v86} fault={:?} notice={:?}",
                    app.session.fault(),
                    app.notice
                );
                std::thread::yield_now();
            }
            eprintln!(
                "CIV APP v86={v86} host_clock={host_clock} reserve={reserve} zero_after_signal={zero_after_signal} longest_zero_run={longest_zero_run} guest={} wall={:?} bytes={:?} synth={:?} nonzero={nonzero} peak={peak} notice={:?} cpu={:?} sink={:?}",
                app.session.emulated_nanos(),
                began.elapsed(),
                app.session.midi_stats(),
                app.audio_engine.stats(),
                app.notice,
                app.session.v86_statistics(),
                writer.stats()
            );
            if seconds > 8 && !capture {
                let mut ticks = [0; 4];
                assert!(app.session.debug_read_memory(0x46c, &mut ticks));
                let (initial_ticks, initial_irqs) = warmup_cadence.unwrap();
                let cadence = (
                    u32::from_le_bytes(ticks) - initial_ticks,
                    app.session.debug_irq_entries() - initial_irqs,
                );
                if !v86 {
                    reference_cadence = Some(cadence);
                } else if let Some(reference) = reference_cadence {
                    // Exclude startup/calibration. Missing timer edges cannot
                    // be hidden by a correctly advancing device-time counter.
                    assert!(
                        cadence.0 * 100 >= reference.0 * 95,
                        "BIOS cadence: {cadence:?} vs {reference:?}"
                    );
                    assert!(
                        cadence.1 * 100 >= reference.1 * 95,
                        "IRQ cadence: {cadence:?} vs {reference:?}"
                    );
                }
            }
            assert!(app.session.emulated_nanos() >= seconds * 1_000_000_000);
            assert!(app.audio_engine.stats().notes > 0, "v86={v86}");
            assert!(nonzero > 1000 && peak > 0.001, "v86={v86}");
        }
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and the v86 module; no host audio"]
    fn local_civ_v86_midi_startup_diagnostic() {
        check_local_civ_midi_startup(true, false);
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and the v86 module; no host audio"]
    fn local_civ_v86_host_clock_midi_startup_diagnostic() {
        check_local_civ_midi_startup(true, true);
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and v86 module; measures the app worker without host audio"]
    fn local_civ_v86_worker_wait_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(false, false, true, false, false, true))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and v86 module; samples menu after ESC"]
    fn local_civ_v86_start_load_idle_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(true, false, true, false, false, true))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ, CM-32L ROMs and v86 module; mock audio output"]
    fn local_civ_v86_audio_start_load_idle_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(true, true, true, false, false, true))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and v86 module; comparison without VGA trap"]
    fn local_civ_v86_start_load_without_vga_trap_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(true, false, false, false, false, true))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and v86 module; settled Start/Load menu"]
    fn local_civ_v86_settled_start_load_idle_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(true, false, true, true, false, true))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and v86 module; settled Start/Load menu with audio"]
    fn local_civ_v86_audio_settled_start_load_idle_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(true, true, true, true, false, true))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and v86 module; baseline without DOS input pacing"]
    fn local_civ_v86_audio_settled_start_load_without_input_polling_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(true, true, true, true, false, false))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires local games/civ and v86 module; settled Start/Load menu with audio"]
    fn local_civ_v86_audio_settled_start_load_with_mpu_pacing_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| local_civ_v86_worker_wait_probe_inner(true, true, true, true, true, true))
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[cfg(feature = "v86")]
    #[ignore = "requires the local ROTT archive and v86 module; diagnostic only"]
    fn local_rott_v86_worker_wait_probe() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                use clap::Parser;
                let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
                let cli = crate::cli::Cli::parse_from([
                    "doslet",
                    "--config",
                    root.join("games/ROTT.toml").to_str().unwrap(),
                    "--v86",
                    "--v86-module",
                    root.join("target/v86/doslet-v86.wasm").to_str().unwrap(),
                    "--no-audio",
                ])
                .resolve()
                .unwrap();
                let mut app = Doslet::new();
                app.launch = cli.launch;
                app.launch.persist = false;
                app.launch.preload_local_media = true;
                app.configurator
                    .set_active(app.launch.machine_spec.clone().unwrap());
                app.init_game_session(cli.game.unwrap()).unwrap();
                let began = Instant::now();
                let mut last = Instant::now();
                let mut last_steps = app.session.steps();
                let mut last_irqs = app.session.debug_irq_entries();
                let mut last_parks = app.session.v86_ram_waits().map_or(0, |(_, p)| p);
                let mut busy = Duration::ZERO;
                let mut first_park = None;
                while began.elapsed() < Duration::from_secs(20)
                    && first_park.is_none_or(|at: Instant| at.elapsed() < Duration::from_secs(10)) {
                    let turn = Instant::now();
                    let guest = app.session.emulated_nanos();
                    let _ = app.worker_tick();
                    busy += turn.elapsed();
                    assert_eq!(app.session.status(), SessionStatus::Running,
                        "fault={:?} error={:?}", app.session.fault(), app.session.v86_last_error());
                    if last.elapsed() >= Duration::from_secs(2) {
                        let parks = app.session.v86_ram_waits().map_or(0, |(_, p)| p);
                        if parks != 0 && first_park.is_none() {
                            first_park = Some(Instant::now());
                        }
                        let irqs = app.session.debug_irq_entries();
                        eprintln!("ROTT v86 t={:.1}s guest={:.1}s steps/s={} IRQ/s={} parks/s={} active={:.1}% pc={:x} frame={:?} stats={:?}",
                            began.elapsed().as_secs_f64(), app.session.emulated_nanos() as f64 / 1e9,
                            (app.session.steps() - last_steps) as f64 / last.elapsed().as_secs_f64(),
                            (irqs - last_irqs) as f64 / last.elapsed().as_secs_f64(),
                            (parks - last_parks) as f64 / last.elapsed().as_secs_f64(),
                            100.0 * busy.as_secs_f64() / began.elapsed().as_secs_f64(),
                            app.session.debug_pc(), app.session.frame().map(|f| (f.width, f.height)), app.session.v86_statistics());
                        last = Instant::now();
                        last_steps = app.session.steps();
                        last_irqs = irqs;
                        last_parks = parks;
                    }
                    std::thread::sleep(app.worker_delay(app.session.emulated_nanos() != guest));
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[cfg(feature = "v86")]
    fn local_civ_v86_worker_wait_probe_inner(
        menu: bool,
        audio: bool,
        vga_trap: bool,
        settled: bool,
        mpu_pacing: bool,
        input_polling: bool,
    ) {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let config = root.join("games/CIV.toml");
        let module = root.join("target/v86/doslet-v86.wasm");
        let mut arguments = vec![
            "doslet",
            "--config",
            config.to_str().unwrap(),
            "--v86",
            "--v86-module",
            module.to_str().unwrap(),
        ];
        if !audio {
            arguments.push("--no-audio");
        }
        let cli = crate::cli::Cli::parse_from(arguments).resolve().unwrap();
        let mut app = Doslet::new();
        app.launch = cli.launch;
        app.launch.preload_local_media = true;
        app.configurator
            .set_active(app.launch.machine_spec.clone().unwrap());
        if audio {
            app.midi_mode = cli.midi_mode.unwrap().into();
            app.routing.set_mode(app.midi_mode);
            app.mt32_rom_dir = cli.mt32_rom_dir;
            app.audio_deferred = true;
            app.audio = Some(crate::audio::Output::mock(48000));
            app.select_midi("CM-32L / MT-32").unwrap();
        }
        let writer = app.audio.as_ref().map(|output| output.writer());
        app.init_game_session(cli.game.unwrap()).unwrap();
        app.session.set_v86_vga_polling(vga_trap);
        app.session.set_v86_mpu_polling(mpu_pacing);
        app.session.set_v86_input_polling(input_polling);
        app.session.set_v86_profiling(true);
        let began = Instant::now();
        let mut busy = Duration::ZERO;
        let mut pages = std::collections::BTreeMap::<u32, u64>::new();
        let mut ips = std::collections::BTreeMap::<u32, u64>::new();
        let mut start_steps = app.session.steps();
        let mut sampled_at = began;
        let mut sample_started = !menu;
        let mut escaped = false;
        let mut consumed_audio = 0_u64;
        let mut first_input_park_at = None;
        let wait_for_input_park = menu && audio && settled && input_polling && !mpu_pacing;
        let duration = if wait_for_input_park {
            60
        } else if settled {
            30
        } else if menu {
            12
        } else {
            5
        };
        while began.elapsed() < Duration::from_secs(duration) {
            if wait_for_input_park
                && sample_started
                && sampled_at.elapsed() >= Duration::from_secs(4)
            {
                break;
            }
            if let Some(writer) = &writer {
                let due = (began.elapsed().as_nanos() * 48000 / 1_000_000_000) as u64;
                let count = due.saturating_sub(consumed_audio).min(4096) as usize;
                let _ = writer.consume(count);
                consumed_audio += count as u64;
            }
            if menu && !escaped && began.elapsed() >= Duration::from_secs(4) {
                assert!(app.session.push_key(0x01, 0x1b).unwrap());
                escaped = true;
            }
            if menu
                && !sample_started
                && if wait_for_input_park {
                    app.session
                        .v86_input_polling()
                        .is_some_and(|(_, parks)| parks >= 100)
                } else {
                    began.elapsed() >= Duration::from_secs(if settled { 26 } else { 8 })
                }
            {
                sample_started = true;
                sampled_at = Instant::now();
                start_steps = app.session.steps();
            }
            let turn = Instant::now();
            let guest = app.session.emulated_nanos();
            let _ = app.worker_tick();
            if first_input_park_at.is_none()
                && app
                    .session
                    .v86_input_polling()
                    .is_some_and(|(_, parks)| parks != 0)
            {
                first_input_park_at = Some(began.elapsed());
            }
            if sample_started {
                busy += turn.elapsed();
                *pages.entry(app.session.debug_pc() & !0xfff).or_default() += 1;
                *ips.entry(app.session.debug_pc()).or_default() += 1;
            }
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "fault={:?} error={:?}",
                app.session.fault(),
                app.session.v86_last_error()
            );
            std::thread::sleep(app.worker_delay(app.session.emulated_nanos() != guest));
        }
        assert!(
            sample_started,
            "CIV did not reach a repeatable input-poll wait within {duration}s"
        );
        let mut pages: Vec<_> = pages.into_iter().collect();
        pages.sort_by_key(|(_, hits)| std::cmp::Reverse(*hits));
        let mut ips: Vec<_> = ips.into_iter().collect();
        ips.sort_by_key(|(_, hits)| std::cmp::Reverse(*hits));
        let frame_hash = app.session.frame().map(|frame| {
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for pixel in frame.indices() {
                hash = (hash ^ u64::from(pixel)).wrapping_mul(0x100_0000_01b3);
            }
            for rgba in frame.palette {
                for byte in rgba {
                    hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
                }
            }
            hash
        });
        if wait_for_input_park {
            let frame = app.session.frame().unwrap();
            let rgba: Vec<_> = frame
                .indices()
                .flat_map(|pixel| frame.palette[usize::from(pixel)])
                .collect();
            let path = root.join("target/civ-v86-input-poll-menu.png");
            let mut encoder = png::Encoder::new(
                std::fs::File::create(path).unwrap(),
                frame.width as u32,
                frame.height as u32,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgba)
                .unwrap();
        }
        eprintln!(
            "CIV v86 menu={menu} audio={audio} vga_trap={vga_trap} settled={settled} input_polling={input_polling}: worker active {:.1}%, steps={} parks={:?} VGA={:?} MPU={:?} input={:?} first_input_park={first_input_park_at:?} notes={} guest={} frame={frame_hash:?} stats={:?} pages={:?}",
            100.0 * busy.as_secs_f64() / sampled_at.elapsed().as_secs_f64(),
            app.session.steps() - start_steps,
            app.session.v86_ram_waits(),
            app.session.v86_vga_polling(),
            app.session.v86_mpu_polling(),
            app.session.v86_input_polling(),
            app.audio_engine.stats().notes,
            app.session.emulated_nanos(),
            app.session.v86_statistics(),
            &pages[..pages.len().min(12)]
        );
        for &(ip, hits) in ips.iter().take(12) {
            eprintln!(
                "CIV v86 IP {ip:08x} hits={hits} code={:02x?}",
                app.session
                    .ram_view(ip.saturating_sub(32) as usize..ip as usize + 32)
            );
        }
        if wait_for_input_park {
            assert!(app.session.push_key(0x1c, 13).unwrap());
            let pressed = Instant::now();
            let deadline = pressed + Duration::from_secs(2);
            let before = frame_hash.unwrap();
            let mut changed = false;
            while Instant::now() < deadline {
                let guest = app.session.emulated_nanos();
                let _ = app.worker_tick();
                if let Some(frame) = app.session.frame() {
                    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
                    for pixel in frame.indices() {
                        hash = (hash ^ u64::from(pixel)).wrapping_mul(0x100_0000_01b3);
                    }
                    for rgba in frame.palette {
                        for byte in rgba {
                            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
                        }
                    }
                    if hash != before {
                        changed = true;
                        eprintln!("CIV menu Enter changed frame after {:?}", pressed.elapsed());
                        break;
                    }
                }
                std::thread::sleep(app.worker_delay(app.session.emulated_nanos() != guest));
            }
            assert!(changed, "CIV menu ignored Enter after the input poll park");
        }
    }

    fn check_local_civ_midi_startup(v86: bool, host_clock: bool) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut session = crate::native_zip::load_game_with_settings(
            &root.join("games/civ/CIV.EXE"),
            &crate::native_input::LaunchSettings {
                no_dynamic_core: true,
                ..Default::default()
            },
        )
        .unwrap();
        if v86 {
            session
                .enable_v86(&std::fs::read(root.join("target/v86/doslet-v86.wasm")).unwrap())
                .unwrap();
            session.set_v86_host_clock(host_clock).unwrap();
        }
        let started = Instant::now();
        let mut routing = Routing::new(Mode::Mt32);
        let mut bytes = [0; 4096];
        let mut pcm = [doslet_pcm::EMPTY; 4096];
        let mut notes = [0u64; 16];
        let mut wire = doslet_midi_synth::wire::Decoder::new();
        let mut mt32 = crate::native_roms::detect().map(|directory| {
            let (control, pcm) = crate::native_roms::read(&directory).unwrap();
            doslet_mt32::Timeline::new(&control, &pcm, 48000).unwrap()
        });
        let mut synth_frames = [[0.0; 2]; 256];
        let mut synth_nonzero = 0_u64;
        let mut synth_busy = Duration::ZERO;
        let mut graphics_selected = false;
        let mut sound_selected = false;
        let mut input_selected = false;
        session.start();
        let seconds = if host_clock { 10 } else { 30 };
        for second in 0..seconds {
            while session.emulated_nanos() < (second + 1) * 1_000_000_000 {
                assert!(
                    started.elapsed() < Duration::from_secs(120),
                    "Civ diagnostic timed out"
                );
                if host_clock {
                    // Match the app: delayed device time is settled separately
                    // instead of replaying a full CPU grant for each late ms.
                    let target = started.elapsed().as_nanos() as u64;
                    if target.saturating_sub(session.emulated_nanos()) <= 1_000_000 {
                        session.advance(65_536);
                    }
                    session
                        .advance_v86_host_time(started.elapsed().as_nanos() as u64)
                        .unwrap();
                } else {
                    session.advance(10000);
                }
                assert_eq!(
                    session.status(),
                    SessionStatus::Running,
                    "v86={v86} host_clock={host_clock} fault={:?}; v86={:?}; console={}; PC={:x}",
                    session.fault(),
                    session.v86_last_error(),
                    String::from_utf8_lossy(session.console()),
                    session.debug_pc()
                );
                while session.drain_pcm_timed(&mut pcm) != 0 {}
                let count = session.drain_midi(&mut bytes);
                if let Some(synth) = &mut mt32 {
                    loop {
                        let render_started = Instant::now();
                        let n = synth
                            .render_until(session.emulated_nanos(), &mut synth_frames)
                            .unwrap();
                        synth_busy += render_started.elapsed();
                        if n == 0 {
                            break;
                        }
                        synth_nonzero +=
                            synth_frames[..n].iter().filter(|f| **f != [0.0; 2]).count() as u64;
                    }
                    for &byte in &bytes[..count] {
                        synth.push_byte(byte).unwrap();
                    }
                }
                for &byte in &bytes[..count] {
                    routing.push(byte, Mode::Mt32, &mut |_| {}).unwrap();
                    if let Some(m) = wire.push(byte).unwrap()
                        && m[0] & 0xF0 == 0x90
                        && m[2] != 0
                    {
                        notes[usize::from(m[0] & 15)] += 1;
                    }
                }
            }
            let screen: String = session
                .ram_view(0xB8000..0xB8FA0)
                .unwrap()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|cell| char::from(cell[0]))
                .collect();
            if !graphics_selected && screen.contains("Select graphics mode") {
                session.push_key(0x02, b'1').unwrap();
                graphics_selected = true;
            } else if !sound_selected && screen.contains("Select sound mode") {
                session.push_key(0x06, b'5').unwrap();
                sound_selected = true;
            } else if !input_selected && screen.contains("Keyboard only") {
                session.push_key(0x02, b'1').unwrap();
                input_selected = true;
            }
            if second % 5 == 4 {
                eprintln!(
                    "CIV guest={}s notes={notes:?} bytes={} SysEx={} synth_nonzero={synth_nonzero}",
                    second + 1,
                    session.midi_stats().1,
                    routing.translator().stats().sysex
                );
            }
        }
        for (index, channel) in routing.channels(Mode::Mt32).iter().enumerate() {
            if channel.enabled {
                eprintln!(
                    "CIV channel={} name={} state={:?}",
                    index + 1,
                    String::from_utf8_lossy(&channel.instrument).trim_matches(char::from(0)),
                    channel.state
                );
            }
        }
        eprintln!(
            "CIV MT-32 render busy {:.1}% of wall time, {} ms over {} ms",
            100.0 * synth_busy.as_secs_f64() / started.elapsed().as_secs_f64(),
            synth_busy.as_millis(),
            started.elapsed().as_millis()
        );
        assert!(
            notes.iter().sum::<u64>() > 0,
            "Civ must reach actual MIDI playback"
        );
        if mt32.is_some() {
            assert!(
                synth_nonzero > 0,
                "Civ MIDI must produce a non-silent CM-32L signal"
            );
        }
    }

    fn check_local_prince_moont_queue(until: u64, restore: bool) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let legacy = root.join("PRINCE/PRINCE.EXE");
        let path = if legacy.is_file() {
            legacy
        } else {
            root.join("games/PRINCE/PRINCE.EXE")
        };
        let directory = std::path::PathBuf::from(
            std::env::var_os("DOSLET_MT32_ROM_DIR").expect("set DOSLET_MT32_ROM_DIR"),
        );
        let mut app = Doslet::new();
        let cli = crate::cli::Cli {
            config: Some(root.join("games/PRINCE.toml")),
            no_audio: true,
            ..Default::default()
        }
        .resolve()
        .unwrap();
        app.launch = cli.launch;
        app.session = crate::native_zip::load_game_with_settings(&path, &app.launch)
            .unwrap()
            .into();
        app.mt32_rom_dir = Some(directory);
        app.audio_engine.inline_mut().mt32 = Some(app.restored_mt32(44100).unwrap());
        app.audio_engine.inline_mut().mixer.reset(44100, 0).unwrap();
        app.session.start();
        let mut batch = 0;
        while app.session.emulated_nanos() < until {
            if restore && batch != 0 && batch % 20000 == 0 {
                app.audio_engine.inline_mut().mt32 = Some(app.restored_mt32(44100).unwrap());
            }
            app.session.prepare_dynamic(2);
            assert_ne!(app.session.advance(CHUNK), 0);
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
            app.process_audio().unwrap_or_else(|error| {
                panic!(
                    "step={} nanos={} notes={} sysex={}: {error}",
                    app.session.steps(),
                    app.session.emulated_nanos(),
                    app.audio_engine
                        .inline()
                        .mt32
                        .as_ref()
                        .unwrap()
                        .note_events(),
                    app.audio_engine
                        .inline()
                        .mt32
                        .as_ref()
                        .unwrap()
                        .sysex_messages()
                )
            });
            batch += 1;
        }
        eprintln!(
            "PRINCE moont: guest={} ms, notes={}, SysEx={}",
            app.session.emulated_nanos() / 1_000_000,
            app.audio_engine
                .inline()
                .mt32
                .as_ref()
                .unwrap()
                .note_events(),
            app.audio_engine
                .inline()
                .mt32
                .as_ref()
                .unwrap()
                .sysex_messages()
        );
    }

    #[test]
    #[ignore = "requires the user's local PRINCE installation; run from repository root"]
    fn local_prince_menu_pcm_and_keyboard() {
        fn advance(session: &mut Session, millis: u64) -> u64 {
            let until = session.emulated_nanos() + millis * 1_000_000;
            let mut pcm = [doslet_pcm::EMPTY; 1024];
            let mut midi = [0; 4096];
            let mut nonzero = 0;
            while session.emulated_nanos() < until {
                session.advance(1000);
                assert_eq!(
                    session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    session.fault()
                );
                loop {
                    let n = session.drain_pcm_timed(&mut pcm);
                    if n == 0 {
                        break;
                    }
                    nonzero += pcm[..n].iter().filter(|f| f.samples != [0; 2]).count() as u64;
                }
                while session.drain_midi(&mut midi) != 0 {}
            }
            nonzero
        }
        fn click(session: &mut Session, x: u16, y: u16) -> u64 {
            session.pointer_move(x, y);
            session.pointer_button(0, true);
            let n = advance(session, 100);
            session.pointer_button(0, false);
            n
        }
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join("games/PRINCE/PRINCE.EXE");
        let profile = crate::machine_profile::parse_machine_profile(
            &std::fs::read_to_string(root.join("games/PRINCE.toml")).unwrap(),
        )
        .unwrap();
        for mode in [
            None,
            Some(doslet_runtime::session::dpmi::Mode::Reflect),
            Some(doslet_runtime::session::dpmi::Mode::Direct),
        ] {
            let mut session = crate::native_zip::load_game_with_settings(
                &path,
                &crate::native_input::LaunchSettings {
                    dpmi: mode,
                    no_dynamic_core: true,
                    hardware: profile.hardware,
                    ..Default::default()
                },
            )
            .unwrap();
            session.start();
            assert_eq!(advance(&mut session, 15_000), 0);
            click(&mut session, 320, 240); // Dismiss the intro.
            advance(&mut session, 3000);
            let onset = click(&mut session, 320, 150); // Start New menu sound.
            let effect = onset + advance(&mut session, 8000);
            assert!(effect > 0, "menu click must actually play PCM: {mode:?}");
            assert_eq!(
                advance(&mut session, 5000),
                0,
                "completed effect must not repeat: {mode:?}"
            );
            assert_eq!(session.audio_status().dsp.block_irq_deadline_misses, 0);
            click(&mut session, 320, 455); // Done: open the name editor.
            advance(&mut session, 3000);
            // Type qzx742, then Backspace and 3. The game's IRQ1 handler must
            // receive make/break events; BIOS-ring injection cannot edit this field.
            for usage in [20, 29, 27, 36, 33, 31, 42, 32] {
                let key = doslet_input::PhysicalKey(usage);
                session.key_event(key, true).unwrap();
                advance(&mut session, 50);
                session.key_event(key, false).unwrap();
                advance(&mut session, 50);
            }
            assert!(
                session
                    .ram_view(0xB0000..0x1000000)
                    .unwrap()
                    .windows(7)
                    .any(|s| s == b"qzx743\0"),
                "keyboard must edit the game's name buffer: {mode:?}"
            );
        }
    }

    #[test]
    #[ignore = "requires the user's local PRINCE installation; run from repository root"]
    #[cfg(feature = "v86")]
    fn local_prince_v86_startup_through_app_clock() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cli = crate::cli::Cli::parse_from([
            "doslet",
            "--config",
            root.join("games/PRINCE.toml").to_str().unwrap(),
            "--v86",
            "--v86-module",
            root.join("target/v86/doslet-v86.wasm").to_str().unwrap(),
            "--no-audio",
            "--drive-mode",
            "overlay",
        ])
        .resolve()
        .unwrap();
        let mut app = Doslet::new();
        app.launch = cli.launch;
        app.launch.preload_local_media = true;
        app.configurator
            .set_active(app.launch.machine_spec.clone().unwrap());
        app.init_game_session(cli.game.unwrap()).unwrap();
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(20) {
            let _ = app.worker_tick();
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "fault={:?} error={:?} regs={:?}",
                app.session.fault(),
                app.session.v86_last_error(),
                app.session.registers()
            );
            std::thread::yield_now();
        }
        eprintln!(
            "PRINCE app steps={} guest={} mode={:?} frame={:?}",
            app.session.steps(),
            app.session.emulated_nanos(),
            app.launch.dpmi,
            app.session.frame().map(|f| (f.width, f.height))
        );
        assert!(app.session.frame().is_some());
    }

    #[test]
    #[ignore = "requires the user's local PRINCE installation; run from repository root"]
    fn local_prince_reaches_intro_through_ui_batches() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let legacy = root.join("PRINCE/PRINCE.EXE");
        let path = if legacy.is_file() {
            legacy
        } else {
            root.join("games/PRINCE/PRINCE.EXE")
        };
        let mut app = Doslet::from_cli(crate::cli::Cli {
            game: Some(path),
            no_audio: true,
            midi_output: None,
            list_midi_outputs: false,
            mt32_rom_dir: None,
            midi_mode: Some(crate::cli::MidiMode::Gm),
            midi_output_mode: Some(crate::cli::MidiMode::Gm),
            ..Default::default()
        })
        .unwrap();
        let start = Instant::now();
        app.tick_at(start);
        for _ in 0..750 {
            let before = app.session.steps();
            app.tick_at(start + Duration::from_secs(60));
            assert!(app.session.steps() - before <= u64::from(MAX_STEPS_PER_TICK));
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
        }
        assert_eq!(app.session.steps(), 300_000_000);
        assert!(app.audio_engine.stats().notes > 0);
        let frame = app.session.frame().expect("game selected a graphics mode");
        assert_eq!((frame.width, frame.height), (640, 480));
        let mut pixels = vec![0; frame.width * frame.height * 4];
        frame.write_rgba(&mut pixels).unwrap();
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[..3] != [0, 0, 0])
        );
        app.display.refresh(&app.session);
        assert!(app.display.image.is_some());
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.session.steps(), 0);
        assert_eq!(app.audio_engine.stats().notes, 0);
        assert_eq!(app.name, "PRINCE.EXE");
    }

    /// `cargo test -p doslet --release --bin doslet heroes_direct -- --ignored --nocapture`
    #[test]
    #[ignore = "requires the local Heroes profile and its learned speed tiers; runs 20 s"]
    fn heroes_direct_follows_its_layout_in_the_app() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cache = std::path::Path::new(&std::env::var_os("HOME").unwrap())
            .join(".cache/doslet/speed-tiers");
        let copy = std::env::temp_dir().join(format!("doslet-app-tiers-{}", std::process::id()));
        std::fs::create_dir_all(&copy).unwrap();
        let name = "0e87d3025f437158dfb9099ca2921c83bac5bf9e4708bad04e4d5fc8297218a2.json";
        std::fs::copy(cache.join(name), copy.join(name)).unwrap();
        let mut app = Doslet::from_cli(crate::cli::Cli::parse_from([
            "doslet",
            "--config",
            root.join("games/HoMnM.toml").to_str().unwrap(),
            "--dpmi",
            "direct",
            "--no-audio",
            "--speed-tier-cache",
            copy.to_str().unwrap(),
        ]))
        .unwrap();
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            let _ = app.worker_tick();
            std::thread::sleep(app.worker_delay(true).min(Duration::from_millis(4)));
        }
        let (_, layout) = app.live_layout.clone().expect("live layout");
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.session.applied_patch_count(), 1);
        let tiers = app.speed_tiers.as_ref().unwrap();
        assert_eq!(tiers.label_layout(), Some(layout.bases().as_slice()));
        let steps = tiers.stats.steps;
        let labelled = 100.0 * (steps[0] + steps[1] + steps[3]) as f64
            / steps.iter().sum::<u64>().max(1) as f64;
        eprintln!(
            "{:.1} guest s, labelled {labelled:.1}%, AOT active {}",
            app.session.emulated_nanos() as f64 / 1e9,
            app.session.wasm_procedures_active()
        );
        assert!(labelled > 80.0, "{labelled}");
        std::fs::remove_dir_all(&copy).unwrap();
    }

    /// `cargo test -p doslet --release --bin doslet dos4gw_games_resume -- --ignored --nocapture`
    #[test]
    #[ignore = "requires the local ROTT, Heroes and PRINCE profiles"]
    fn dos4gw_games_resume_from_states_under_dpmi_direct() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let run = |session: &mut Session, until: u64| {
            let mut pcm = [doslet_pcm::EMPTY; 4096];
            let mut midi = [0; 4096];
            while session.emulated_nanos() < until && session.status() == SessionStatus::Running {
                session.advance(10_000);
                while session.drain_pcm_timed(&mut pcm) != 0 {}
                while session.drain_midi(&mut midi) != 0 {}
            }
        };
        for profile in ["ROTT.toml", "HoMnM.toml", "PRINCE.toml"] {
            let cli = crate::cli::Cli::parse_from([
                "doslet",
                "--config",
                root.join("games").join(profile).to_str().unwrap(),
                "--dpmi",
                "direct",
                "--no-aot",
            ])
            .resolve()
            .unwrap();
            let game = cli.game.clone().unwrap();
            let mut launch = cli.launch;
            launch.persist = false;
            launch.preload_local_media = true;
            // Patches move only with the desktop's layout watcher.
            launch.patches.clear();
            let mut original = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
            original.start();
            run(&mut original, 10_000_000_000);
            assert_eq!(original.status(), SessionStatus::Running);
            let calls = original.dpmi_stats().unwrap().calls;
            let state = original.save_state().unwrap();
            let media = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
            let mut restored = Session::load_state_with_media(&state, &media).unwrap();
            assert_eq!(restored.save_state().unwrap(), state);
            run(&mut original, 15_000_000_000);
            run(&mut restored, 15_000_000_000);
            let (a, b) = (
                original.save_state().unwrap(),
                restored.save_state().unwrap(),
            );
            eprintln!(
                "{profile}: {} KiB state at 10 s ({calls} DPMI calls); {:?} and {:?} at 15 s, states equal: {}",
                state.len() / 1024,
                original.status(),
                restored.status(),
                a == b
            );
            assert!(a == b);
            assert!(original.dpmi_stats().unwrap().calls >= calls);
        }
    }
}
