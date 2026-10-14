use clap::Parser;
use std::path::PathBuf;

/// Run a DOS game in the Doslet desktop window.
#[derive(Debug, Parser)]
#[command(
    version,
    after_help = "Without --game, opens the built-in color-bar demo.\nGame support is experimental; keyboard/mouse input and saves are pending."
)]
pub struct Cli {
    /// DOS executable; its containing directory is mounted as C:
    #[arg(long, value_name = "EXE")]
    pub game: Option<PathBuf>,

    /// Run without opening an audio output device
    #[arg(long, conflicts_with = "midi_output")]
    pub no_audio: bool,

    /// MIDI output index or exact port name; use the host's synthesizer
    #[arg(long, value_name = "PORT")]
    pub midi_output: Option<String>,

    /// List host MIDI output ports and exit before opening a window
    #[arg(long)]
    pub list_midi_outputs: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn game_options_and_demo_default() {
        let cli =
            Cli::try_parse_from(["doslet", "--game", "my game/PRINCE.EXE", "--no-audio"]).unwrap();
        assert_eq!(cli.game.unwrap(), PathBuf::from("my game/PRINCE.EXE"));
        assert!(cli.no_audio);
        assert!(Cli::try_parse_from(["doslet"]).unwrap().game.is_none());
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
