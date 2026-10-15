#[cfg(not(target_arch = "wasm32"))]
mod app;
#[cfg(target_arch = "wasm32")]
#[path = "browser_app.rs"]
mod app;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod archive;
#[cfg(not(target_arch = "wasm32"))]
mod audio;
#[cfg(target_arch = "wasm32")]
mod browser_diagnostics;
#[cfg(target_arch = "wasm32")]
mod browser_input;
#[cfg(not(target_arch = "wasm32"))]
mod cli;
mod configurator;
mod keyboard;
#[path = "../../common/machine_profile.rs"]
mod machine_profile;
mod midi_activity;
#[cfg(not(target_arch = "wasm32"))]
mod native_gamepad;
#[cfg(not(target_arch = "wasm32"))]
#[path = "../../common/native_input.rs"]
mod native_input;
#[cfg(not(target_arch = "wasm32"))]
mod native_midi;
#[cfg(not(target_arch = "wasm32"))]
#[path = "../../common/native_storage.rs"]
mod native_storage;
#[cfg(not(target_arch = "wasm32"))]
mod native_synth;
mod presentation;
#[cfg(not(target_arch = "wasm32"))]
mod profile;
mod ui;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser;
    let cli = cli::Cli::parse();
    if cli.list_midi_outputs {
        println!("rust: Rust synth");
        #[cfg(target_os = "macos")]
        println!("apple: {}", native_synth::APPLE_LABEL);
        println!("moont: CM-32L / MT-32 (requires ROMs)");
        for name in native_midi::ports()?.names {
            println!("{name}");
        }
        return Ok(());
    }
    let app = app::Doslet::from_cli(cli)?;
    // Transfer ownership once at iced startup. Execution owns the state directly.
    let initial = std::cell::Cell::new(Some(app));
    run(move || *initial.take().expect("iced boots the application once"))?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() -> iced::Result {
    console_error_panic_hook::set_once();
    browser_diagnostics::init();
    run(app::Doslet::new)
}

fn run(boot: impl iced::application::BootFn<app::Doslet, app::Message> + 'static) -> iced::Result {
    let window = iced::window::Settings {
        #[cfg(not(target_arch = "wasm32"))]
        size: presentation::DEFAULT_WINDOW_SIZE,
        #[cfg(not(target_arch = "wasm32"))]
        min_size: Some(presentation::MIN_WINDOW_SIZE),
        #[cfg(target_arch = "wasm32")]
        size: iced::Size::new(1120.0, 760.0),
        #[cfg(not(target_arch = "wasm32"))]
        exit_on_close_request: false,
        #[cfg(target_os = "linux")]
        platform_specific: iced::window::settings::PlatformSpecific {
            // Match the desktop entry so Wayland and X11 can resolve our icon.
            application_id: "org.doslet.Doslet".into(),
            ..Default::default()
        },
        ..Default::default()
    };

    iced::application(boot, app::Doslet::update, app::Doslet::view)
        .title("Doslet · A little DOS")
        .theme(app::Doslet::theme)
        .subscription(app::Doslet::subscription)
        .window(window)
        .centered()
        .run()
}
