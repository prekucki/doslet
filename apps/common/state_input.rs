//! Resolve snapshot assets once from a launch profile; no host paths in state files.
#[allow(dead_code)]
#[path = "machine_profile.rs"]
mod machine_profile;
#[allow(dead_code)]
#[path = "native_input.rs"]
pub(super) mod native_input;
use doslet_runtime::Session;
use std::{error::Error, path::Path};

pub struct Loader {
    media: Option<Session>,
}
impl Loader {
    /// Diagnostic examples accept an optional `--config FILE` beside their positionals.
    pub fn arguments(args: &mut Vec<String>) -> Result<Self, Box<dyn Error>> {
        let Some(index) = args.iter().position(|a| a == "--config") else {
            return Ok(Self { media: None });
        };
        if index + 1 >= args.len() {
            return Err("--config needs a path".into());
        }
        let path = args.remove(index + 1);
        args.remove(index);
        let path = Path::new(&path);
        let spec = machine_profile::parse_machine_profile(&std::fs::read_to_string(path)?)?;
        let base = path.parent().unwrap_or(Path::new("."));
        let mut settings = native_input::LaunchSettings {
            hardware: spec.hardware,
            no_dynamic_core: true,
            preload_local_media: true,
            ..Default::default()
        };
        for mount in &spec.mounts {
            let source = base.join(spec.binding(&mount.source));
            match mount.kind {
                doslet_config::SourceKind::NativeDirectory if mount.letter == b'C' => {
                    settings.root = Some(source)
                }
                doslet_config::SourceKind::LocalIso => {
                    settings.cdrom = Some((mount.letter, source))
                }
                _ => {
                    return Err(
                        "snapshot resources require local C: directory and ISO media".into(),
                    );
                }
            }
        }
        let media =
            native_input::load_game_with_settings(&base.join(&spec.game.executable), &settings)?;
        Ok(Self { media: Some(media) })
    }
    pub fn restore(&self, bytes: &[u8]) -> Result<Session, Box<dyn Error>> {
        Ok(match &self.media {
            Some(media) => Session::load_state_with_media(bytes, media)?,
            None => Session::load_state(bytes)?,
        })
    }
}
