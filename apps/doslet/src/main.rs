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
#[cfg(not(target_arch = "wasm32"))]
#[path = "../../common/native_input.rs"]
mod native_input;
#[cfg(not(target_arch = "wasm32"))]
mod native_midi;
mod presentation;
mod ui;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser;
    let cli = cli::Cli::parse();
    if cli.list_midi_outputs {
        for name in native_midi::ports()?.names {
            println!("{name}");
        }
        return Ok(());
    }
    let app = app::Doslet::from_cli(cli)?;
    // Transfer ownership once at iced startup. Execution owns the state directly.
    let initial = std::cell::Cell::new(Some(app));
    run(move || initial.take().expect("iced boots the application once"))?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() -> iced::Result {
    console_error_panic_hook::set_once();
    browser_diagnostics::init();
    run(app::Doslet::new)
}

fn run(boot: impl iced::application::BootFn<app::Doslet, app::Message> + 'static) -> iced::Result {
    iced::application(boot, app::Doslet::update, app::Doslet::view)
        .title("Doslet · A little DOS")
        .theme(app::Doslet::theme)
        .subscription(app::Doslet::subscription)
        .window_size((1120.0, 760.0))
        .centered()
        .run()
}
