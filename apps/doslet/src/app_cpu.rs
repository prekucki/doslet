use super::Doslet;
use crate::cpu_config::{Core, Edit, Mode, Settings};

impl Doslet {
    pub(super) fn edit_cpu_profile(&mut self, edit: crate::cpu_config::ProfileEdit) {
        use crate::cpu_config::{ProfileEdit, ProfileForm};
        match edit {
            ProfileEdit::New | ProfileEdit::Open => {
                let path = if matches!(edit, ProfileEdit::Open) {
                    self.speed_tiers
                        .as_ref()
                        .and_then(|t| t.profile_info().path)
                        .unwrap_or_default()
                } else {
                    let name = std::path::Path::new(&self.name)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy();
                    crate::native_speed_tiers::Controller::suggested_path(&name)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| "speed-tier-profile.json".into())
                };
                self.workspace.cpu_profile_form = Some(ProfileForm {
                    path,
                    open: matches!(edit, ProfileEdit::Open),
                    error: None,
                });
            }
            ProfileEdit::Cancel => self.workspace.cpu_profile_form = None,
            ProfileEdit::Path(path) => {
                if let Some(form) = &mut self.workspace.cpu_profile_form {
                    form.path = path;
                    form.error = None;
                }
            }
            ProfileEdit::Submit => {
                if self.gdb_connected() || self.configurator.pending {
                    if let Some(form) = &mut self.workspace.cpu_profile_form {
                        form.error = Some("Detach GDB or wait for machine preparation.".into());
                    }
                    return;
                }
                if let Some(form) = self.workspace.cpu_profile_form.clone() {
                    match self.install_cpu_profile(&form) {
                        Ok(()) => self.workspace.cpu_profile_form = None,
                        Err(error) => {
                            if let Some(form) = &mut self.workspace.cpu_profile_form {
                                form.error = Some(error);
                            }
                        }
                    }
                }
            }
        }
    }
    fn install_cpu_profile(&mut self, form: &crate::cpu_config::ProfileForm) -> Result<(), String> {
        use crate::native_speed_tiers::Controller;
        let path = std::path::Path::new(form.path.trim());
        if path.as_os_str().is_empty() {
            return Err("Enter a JSON file path.".into());
        }
        let mut settings = self.workspace.cpu.unwrap_or_else(|| self.cpu_settings());
        settings.mode = Mode::Tiers;
        settings.learning = false;
        if !settings.valid() {
            return Err("Choose valid CPU speeds and an available core.".into());
        }
        let rates = [settings.hot, Some(settings.cold), Some(settings.grey)];
        if !form.open {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let empty = Controller::untrained();
            self.speed_tiers
                .as_ref()
                .unwrap_or(&empty)
                .export_new(path, rates)
                .map_err(|e| {
                    format!(
                        "Cannot create profile: {e}. Choose a new file or open an existing profile."
                    )
                })?;
        }
        let mut candidate =
            Controller::load_ui(path, rates).map_err(|e| format!("Cannot open profile: {e}"))?;
        if let Some(exe) = &self.source {
            crate::native_zip::bind_game(&mut candidate, exe, &self.launch, None)
                .map_err(|e| e.to_string())?;
        }
        settings.hot = candidate.stats.hot_mhz;
        settings.grey = candidate.stats.grey_mhz;
        settings.cold = candidate.stats.cold_mhz;
        self.disable_speed_tiers();
        if let Some(old) = &mut self.speed_tiers {
            candidate.inherit_pending(old, &self.session);
        }
        self.speed_tiers = Some(candidate);
        self.apply_cpu(settings)?;
        self.workspace.cpu = Some(self.cpu_settings());
        Ok(())
    }
    fn aot_installed(&self) -> bool {
        #[cfg(feature = "wasm-procedures")]
        return self.session.wasm_procedures_active();
        #[cfg(not(feature = "wasm-procedures"))]
        false
    }
    pub(super) fn cpu_settings(&self) -> Settings {
        let tiers = self.speed_tiers.as_ref().map(|t| t.stats);
        Settings {
            mode: if tiers.is_some_and(|t| t.enabled) {
                Mode::Tiers
            } else if self.session.synchronization_analysis().controller_enabled {
                Mode::Adaptive
            } else {
                Mode::Fixed
            },
            core: if self.aot_installed() {
                Core::Aot
            } else if crate::cpu_config::JIT_AVAILABLE && self.session.dynamic_core_enabled() {
                Core::Jit
            } else {
                Core::Interpreter
            },
            mhz: self.configurator.active.hardware.cpu_mhz,
            grey: tiers.map_or(133, |t| t.grey_mhz),
            hot: tiers.map_or(Some(500), |t| t.hot_mhz),
            cold: tiers.map_or(8, |t| t.cold_mhz),
            learning: tiers.is_some_and(|t| t.enabled && t.learning),
        }
    }
    pub(super) fn edit_cpu(&mut self, edit: Edit) {
        match edit {
            Edit::Revert => self.workspace.cpu = Some(self.cpu_settings()),
            Edit::Apply => {
                if self.gdb_connected() || self.configurator.pending {
                    self.notice = Some(
                        "CPU controls are unavailable during debugging or machine preparation."
                            .into(),
                    );
                    return;
                }
                if let Some(settings) = self.workspace.cpu {
                    if let Err(error) = self.apply_cpu(settings) {
                        self.notice = Some(error);
                    } else {
                        self.workspace.cpu = Some(self.cpu_settings());
                    }
                }
            }
            _ => {
                let current = self.cpu_settings();
                self.workspace.cpu.get_or_insert(current).edit(edit);
            }
        }
    }
    fn apply_cpu(&mut self, settings: Settings) -> Result<(), String> {
        if !settings.valid() {
            return Err(
                "CPU speeds must be between 1 and 1000 MHz; select an available core.".into(),
            );
        }
        if settings.mode == Mode::Tiers && settings.learning && !self.cpu_settings().learning {
            self.session
                .check_working_state()
                .map_err(|e| format!("Tier learning is unavailable: {e}"))?;
        }
        // Prepare an optional profile before changing live execution settings.
        if settings.mode == Mode::Tiers && self.speed_tiers.is_none() {
            let mut tiers = crate::native_speed_tiers::Controller::untrained();
            if let Some(exe) = &self.source {
                crate::native_zip::bind_game(&mut tiers, exe, &self.launch, None)
                    .map_err(|e| e.to_string())?;
            }
            self.speed_tiers = Some(tiers);
        }
        self.disable_speed_tiers();
        self.session.set_synchronization_analysis(false);
        self.session.set_adaptive_idle(false);
        self.session.set_idle_observation(false);
        self.session.set_menu_idle_site(None);
        self.session.set_event_idle_site(None);
        self.session
            .set_cpu_mhz(settings.mhz)
            .map_err(|e| e.to_string())?;
        self.configurator.active.hardware.cpu_mhz = settings.mhz;
        self.configurator.draft.hardware.cpu_mhz = settings.mhz;
        self.launch.hardware.cpu_mhz = settings.mhz;
        if let Some(spec) = &mut self.launch.machine_spec {
            spec.hardware.cpu_mhz = settings.mhz;
        }
        self.launch.no_dynamic_core = settings.core != Core::Jit;
        self.launch.adaptive_cpu = settings.mode == Mode::Adaptive;
        self.launch.analyze_synchronization = self.launch.adaptive_cpu;
        self.launch.adaptive_idle = false;
        self.launch.observe_idle = false;
        self.launch.heroes_menu_idle = false;
        self.launch.heroes_timer_waits = false;
        self.session.set_dynamic_core(settings.core == Core::Jit);
        if settings.mode == Mode::Adaptive {
            self.session.set_adaptive_cpu(true);
        }
        if let Some(tiers) = &mut self.speed_tiers {
            tiers.configure(
                settings.hot,
                settings.grey,
                settings.cold,
                settings.learning && settings.mode == Mode::Tiers,
            );
            if settings.mode == Mode::Tiers {
                tiers.enable(&mut self.session);
            }
        }
        if settings.mode == Mode::Tiers {
            self.follow_layout_with_tiers();
        }
        self.anchor = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Message;
    use crate::cpu_config::ProfileEdit;

    #[test]
    fn unreadable_zip_binary_does_not_activate_overclocking() {
        use clap::Parser;
        let fixture = crate::profile::tests::Fixture::new("version=2");
        let zip = fixture.root.join("game.zip");
        std::fs::write(
            &zip,
            crate::archive::tests::pack(
                &[("GAME/PLAY.COM", &[0xeb, 0xfe])],
                zip::CompressionMethod::Deflated,
            )
            .as_slice(),
        )
        .unwrap();
        let cli =
            crate::cli::Cli::parse_from(["doslet", "--zip", zip.to_str().unwrap(), "--no-audio"])
                .resolve()
                .unwrap();
        let mut app = app();
        app.source = cli.game;
        app.launch = cli.launch;
        let before = app.cpu_settings();
        let position = (
            app.session.debug_pc(),
            app.session.steps(),
            app.session.emulated_nanos(),
        );
        std::fs::write(zip, b"invalid archive").unwrap();
        let mut settings = before;
        settings.mode = Mode::Tiers;
        assert!(app.apply_cpu(settings).is_err());
        assert!(app.speed_tiers.is_none());
        assert_eq!(app.cpu_settings(), before);
        assert_eq!(
            (
                app.session.debug_pc(),
                app.session.steps(),
                app.session.emulated_nanos()
            ),
            position
        );
    }

    #[test]
    fn profile_creation_is_durable_activates_without_restart_and_keeps_learning_off() {
        let fixture = crate::profile::tests::Fixture::new("");
        let path = fixture.root.join("new profile.json");
        let mut app = app();
        let before = (
            app.session.debug_pc(),
            app.session.steps(),
            app.session.emulated_nanos(),
            app.session.epoch(),
        );
        let _ = app.update(Message::Cpu(Edit::Grey(100)));
        let _ = app.update(Message::Cpu(Edit::Hot(500)));
        let _ = app.update(Message::Cpu(Edit::Learning(true)));
        let _ = app.update(Message::CpuProfile(ProfileEdit::New));
        let _ = app.update(Message::CpuProfile(ProfileEdit::Path(
            path.to_string_lossy().into_owned(),
        )));
        assert!(!path.exists());
        app.configurator.pending = true;
        let _ = app.update(Message::CpuProfile(ProfileEdit::Submit));
        assert!(!path.exists());
        app.configurator.pending = false;
        let _ = app.update(Message::CpuProfile(ProfileEdit::Submit));
        assert!(
            app.workspace.cpu_profile_form.is_none(),
            "{:?}",
            app.workspace.cpu_profile_form
        );
        assert_eq!(
            (
                app.session.debug_pc(),
                app.session.steps(),
                app.session.emulated_nanos(),
                app.session.epoch()
            ),
            before
        );
        assert_eq!(app.cpu_settings().mode, Mode::Tiers);
        assert!(!app.cpu_settings().learning);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["rates"], serde_json::json!([500, 8, 100]));
        assert_eq!(value["learning"], false);
        assert_eq!(value["nodes"], serde_json::json!([]));
        let loaded = crate::native_speed_tiers::Controller::load(&path, 8, Some(500), 100).unwrap();
        assert_eq!(loaded.profile_info().labels, 0);
        app.reset();
        assert_eq!(app.cpu_settings().mode, Mode::Tiers);
        assert!(!app.cpu_settings().learning);
    }
    #[test]
    fn profile_open_restores_rates_and_rejects_bad_files_without_changing_execution() {
        let fixture = crate::profile::tests::Fixture::new("");
        let path = fixture.root.join("selected.json");
        let bytes = serde_json::to_vec(&serde_json::json!({"rates":[500,8,100],"learning":true,"unknown":"grey","irq_policy":"static-grey","nodes":[
            {"pc":0x10100,"bits":16,"bytes":vec![0x90;15],"tier":"fast","reason":"manual"}
        ]})).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut app = app();
        let before = (
            app.session.debug_pc(),
            app.session.steps(),
            app.session.emulated_nanos(),
        );
        let _ = app.update(Message::CpuProfile(ProfileEdit::Open));
        let _ = app.update(Message::CpuProfile(ProfileEdit::Path(
            path.to_string_lossy().into_owned(),
        )));
        let _ = app.update(Message::CpuProfile(ProfileEdit::Submit));
        assert!(app.workspace.cpu_profile_form.is_none());
        let actual = app.cpu_settings();
        assert_eq!((actual.hot, actual.grey, actual.cold), (Some(500), 100, 8));
        assert!(!actual.learning);
        assert_eq!(app.speed_tiers.as_ref().unwrap().profile_info().labels, 1);
        let _ = app.update(Message::CpuProfile(ProfileEdit::New));
        let _ = app.update(Message::CpuProfile(ProfileEdit::Path(
            path.to_string_lossy().into_owned(),
        )));
        let _ = app.update(Message::CpuProfile(ProfileEdit::Submit));
        assert!(
            app.workspace
                .cpu_profile_form
                .as_ref()
                .unwrap()
                .error
                .is_some()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(app.cpu_settings(), actual);
        std::fs::write(&path, b"invalid JSON").unwrap();
        let _ = app.update(Message::CpuProfile(ProfileEdit::Open));
        let _ = app.update(Message::CpuProfile(ProfileEdit::Path(
            path.to_string_lossy().into_owned(),
        )));
        let _ = app.update(Message::CpuProfile(ProfileEdit::Submit));
        assert!(
            app.workspace
                .cpu_profile_form
                .as_ref()
                .unwrap()
                .error
                .is_some()
        );
        assert_eq!(app.cpu_settings(), actual);
        assert_eq!(
            (
                app.session.debug_pc(),
                app.session.steps(),
                app.session.emulated_nanos()
            ),
            before
        );
    }

    fn app() -> Box<Doslet> {
        let mut app = Box::new(Doslet::new());
        app.session = doslet_runtime::Session::new(&[0x40, 0xeb, 0xfd])
            .unwrap()
            .into();
        app.session.start();
        app.session.advance(100);
        app.session.pause();
        app.workspace.cpu = Some(app.cpu_settings());
        app
    }
    #[test]
    fn drafts_and_apply_preserve_program_position_and_guest_clock() {
        let mut app = app();
        let pc = app.session.debug_pc();
        let nanos = app.session.emulated_nanos();
        let steps = app.session.steps();
        let old_mhz = app.session.hardware_config().cpu_mhz;
        let _ = app.update(Message::Cpu(Edit::Mhz(66)));
        assert_eq!(app.session.hardware_config().cpu_mhz, old_mhz);
        let _ = app.update(Message::Cpu(Edit::Revert));
        assert_eq!(app.workspace.cpu.unwrap(), app.cpu_settings());
        let _ = app.update(Message::Cpu(Edit::Mhz(100)));
        let _ = app.update(Message::Cpu(Edit::Core(Core::Interpreter)));
        let _ = app.update(Message::Cpu(Edit::Apply));
        assert_eq!(app.session.hardware_config().cpu_mhz, 100);
        assert!(!app.session.dynamic_core_enabled());
        assert_eq!(app.session.debug_pc(), pc);
        assert_eq!(app.session.emulated_nanos(), nanos);
        assert_eq!(app.session.steps(), steps);
        assert_eq!(app.session.status(), doslet_runtime::SessionStatus::Paused);
        app.reset();
        assert_eq!(app.session.hardware_config().cpu_mhz, 100);
        assert!(!app.session.dynamic_core_enabled());
    }
    #[test]
    fn current_cpu_settings_always_select_an_available_core() {
        let mut app = app();
        // Workspace feature unification can enable the runtime's dynamic core
        // without enabling this frontend's compiler worker.
        app.session.set_dynamic_core(true);
        let settings = app.cpu_settings();
        assert!(settings.valid());
        if !crate::cpu_config::JIT_AVAILABLE {
            assert_eq!(settings.core, Core::Interpreter);
        }
    }
    #[test]
    fn tier_rates_and_core_apply_together_and_learning_requires_opt_in() {
        let mut app = app();
        let _ = app.update(Message::Cpu(Edit::Mode(Mode::Tiers)));
        let _ = app.update(Message::Cpu(Edit::Grey(100)));
        let _ = app.update(Message::Cpu(Edit::Hot(500)));
        let _ = app.update(Message::Cpu(Edit::Cold(8)));
        let _ = app.update(Message::Cpu(Edit::Apply));
        let actual = app.cpu_settings();
        assert_eq!((actual.grey, actual.hot, actual.cold), (100, Some(500), 8));
        assert!(!actual.learning);
        let pc = app.session.debug_pc();
        let nanos = app.session.emulated_nanos();
        let _ = app.update(Message::Cpu(Edit::Learning(true)));
        let _ = app.update(Message::Cpu(Edit::Apply));
        assert!(app.speed_tiers.as_ref().unwrap().stats.learning);
        let _ = app.update(Message::Cpu(Edit::Learning(false)));
        let _ = app.update(Message::Cpu(Edit::Uncapped(true)));
        let _ = app.update(Message::Cpu(Edit::Core(Core::Interpreter)));
        let _ = app.update(Message::Cpu(Edit::Apply));
        let tiers = app.speed_tiers.as_ref().unwrap();
        assert!(tiers.stats.enabled);
        assert!(!tiers.stats.learning && !tiers.stats.learning_running);
        assert_eq!(tiers.stats.learning_pending, 0);
        assert_eq!(tiers.stats.hot_mhz, None);
        assert!(!app.session.dynamic_core_enabled());
        assert_eq!(app.session.debug_pc(), pc);
        assert_eq!(app.session.emulated_nanos(), nanos);
        app.reset();
        assert!(app.cpu_settings().mode == Mode::Tiers);
        assert!(!app.cpu_settings().learning);
        assert!(!app.session.dynamic_core_enabled());
    }
    #[test]
    fn invalid_or_blocked_apply_leaves_active_configuration_unchanged() {
        let mut app = app();
        let before = app.cpu_settings();
        app.workspace.cpu.as_mut().unwrap().mhz = 0;
        let _ = app.update(Message::Cpu(Edit::Apply));
        assert_eq!(app.cpu_settings(), before);
        assert!(app.notice.is_some());
        app.workspace.cpu.as_mut().unwrap().mhz = 66;
        app.configurator.pending = true;
        let _ = app.update(Message::Cpu(Edit::Apply));
        assert_eq!(app.cpu_settings(), before);
    }
    #[test]
    fn adaptive_mode_uses_its_ceiling_and_fixed_mode_disables_it() {
        let mut app = app();
        let _ = app.update(Message::Cpu(Edit::Mhz(133)));
        let _ = app.update(Message::Cpu(Edit::Mode(Mode::Adaptive)));
        let _ = app.update(Message::Cpu(Edit::Apply));
        assert_eq!(app.cpu_settings().mode, Mode::Adaptive);
        assert_eq!(app.session.synchronization_analysis().configured_mhz, 133);
        let _ = app.update(Message::Cpu(Edit::Mode(Mode::Fixed)));
        let _ = app.update(Message::Cpu(Edit::Mhz(66)));
        let _ = app.update(Message::Cpu(Edit::Apply));
        assert_eq!(app.cpu_settings().mode, Mode::Fixed);
        assert_eq!(app.session.hardware_config().cpu_mhz, 66);
    }
}
