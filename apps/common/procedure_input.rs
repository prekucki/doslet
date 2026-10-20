//! Read-only host input for offline procedure preparation and replay diagnostics.
#[allow(dead_code)]
#[path = "../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "machine_profile.rs"]
mod machine_profile;
#[cfg(test)]
#[path = "../../tests/fixtures/relocated_mz.rs"]
mod mz_fixture;
#[allow(dead_code)]
#[path = "native_input.rs"]
mod native_input;

use doslet_config::{SourceKind, WritePolicy};
use doslet_runtime::Session;
use std::{error::Error, fs::File, io::Read, path::Path, sync::Arc};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub struct Input {
    pub exe: Vec<u8>,
    pub media: Session,
}

/// Reuse the native adapter's installer without loading its backend module twice.
#[cfg(feature = "wasm-procedures")]
pub fn install_procedures(session: &mut Session, path: &Path, interpreted: bool) -> Result<()> {
    native_input::install_wasm_procedures(
        session,
        &native_input::LaunchSettings {
            wasm_procedures: Some(path.to_owned()),
            interpret_procedures: interpreted,
            ..native_input::LaunchSettings::default()
        },
    )
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!(
            "{} exceeds the procedure input limit ({limit} bytes)",
            path.display()
        )
        .into());
    }
    Ok(bytes)
}

/// Resolve profile-relative native paths without opening host audio or ROMs.
/// ZIP sessions retain their existing private, temporary guest write overlay.
pub fn load(config: &Path) -> Result<Input> {
    let mut spec =
        machine_profile::parse_machine_profile(std::str::from_utf8(&read(config, 1024 * 1024)?)?)?;
    let plan = spec.resolve().map_err(|error| error.to_string())?;
    let base = config.parent().unwrap_or(Path::new("."));
    if spec.mounts.iter().any(|m| m.kind == SourceKind::HttpsIso) {
        return Err("procedure diagnostics require local CD-ROM media".into());
    }
    let cdrom = spec
        .mounts
        .iter()
        .find(|m| m.kind == SourceKind::LocalIso)
        .map(|m| (m.letter, base.join(spec.binding(&m.source))));
    let c_mount = spec.mounts.iter().find(|m| m.letter == b'C');
    if let Some(mount) = c_mount.filter(|m| m.kind == SourceKind::BrowserZip) {
        let source = base.join(spec.binding(&mount.source));
        let archive = archive::GameArchive::new(Arc::new(read(&source, archive::MAX_ZIP_BYTES)?))?;
        let wanted = spec.game.executable.replace('\\', "/").to_ascii_uppercase();
        let selected = archive
            .programs
            .iter()
            .find(|program| **program == wanted || program.ends_with(&format!("/{wanted}")))
            .ok_or("EXE missing in ZIP")?;
        let exe = archive.program_bytes(selected)?;
        for mount in &mut spec.mounts {
            if mount.letter == b'C' {
                mount.policy = WritePolicy::Temporary;
                mount.store = None;
            }
        }
        let mut media = archive.load_configured(selected, &spec)?;
        if let Some((letter, path)) = cdrom {
            media = media.with_cdrom_image(letter, std::fs::read(path)?)?;
        }
        media.set_dynamic_core(false);
        return Ok(Input { exe, media });
    }

    let executable = base.join(&spec.game.executable);
    let exe = read(&executable, archive::MAX_ZIP_BYTES)?;
    let settings = native_input::LaunchSettings {
        hardware: spec.hardware,
        command_tail: plan.command_tail,
        working_directory: spec.game.working_directory.as_bytes().to_vec(),
        patches: spec.game.patches.iter().map(|p| base.join(p)).collect(),
        root: c_mount.map(|m| base.join(spec.binding(&m.source))),
        writable: false,
        persist: false,
        preload_local_media: true,
        cdrom,
        no_dynamic_core: true,
        machine_spec: Some(spec),
        ..native_input::LaunchSettings::default()
    };
    let media = native_input::load_game_with_settings(&executable, &settings)?;
    Ok(Input { exe, media })
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_runtime::SessionStatus;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    include!("../../tests/fixtures/iso9660.rs");

    struct Files(PathBuf);
    impl Files {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "doslet procedure input {} {}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Files {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn native_bindings_preserve_launch_settings_and_deny_guest_writes() {
        let files = Files::new();
        let mut exe = mz_fixture::relocated_mz();
        exe[6] = 0; // This program has no relocation entries.
        exe[32..].fill(0);
        // Get CWD, require one mounted CD-ROM, then attempt to create NEW.DAT.
        // The DOS error becomes the exit code: read-only C: must return 5.
        let code = [
            0x0e, 0x1f, 0xb4, 0x47, 0xb2, 0, 0xbe, 0x40, 0, 0xcd, 0x21, 0xb8, 0, 0x15, 0x31, 0xdb,
            0xcd, 0x2f, 0x83, 0xfb, 1, 0x75, 14, 0xba, 0x60, 0, 0x31, 0xc9, 0xb8, 0, 0x3c, 0xcd,
            0x21, 0xb4, 0x4c, 0xcd, 0x21, 0xb8, 99, 0x4c, 0xcd, 0x21,
        ];
        exe[32..32 + code.len()].copy_from_slice(&code);
        exe[32 + 0x60..32 + 0x68].copy_from_slice(b"NEW.DAT\0");
        let executable = files.write("game files/SUB/GAME.EXE", &exe);
        let iso = test_iso();
        let disc = files.write("local disc.iso", &iso);
        let config = files.write(
            "profile dir/native.toml",
            br#"
version = 2
[machine]
memory_mib = 8
[game]
executable = '../game files/SUB/GAME.EXE'
arguments = ['/test', 'two words']
working_directory = 'C:\SUB'
[output]
midi_output = 'moont'
rom_binding = 'absent ROM directory'
[[drives]]
letter = 'C'
kind = 'directory'
source = 'game'
policy = 'persistent'
store = 'absent store'
[[drives]]
letter = 'E'
kind = 'iso'
source = 'disc'
policy = 'read-only'
[[bindings]]
id = 'game'
kind = 'directory'
location = '../game files'
[[bindings]]
id = 'disc'
kind = 'iso'
location = '../local disc.iso'
"#,
        );
        let Input {
            exe: loaded,
            mut media,
        } = load(&config).unwrap();
        assert_eq!(loaded, exe);
        assert!(!media.dynamic_core_enabled());
        assert_eq!(media.memory_usage().installed_bytes, 8 * 1024 * 1024);
        let psp = usize::from(doslet_dos::loader::PSP_SEGMENT) * 16;
        let tail = b" /test \"two words\"";
        assert_eq!(
            media.ram_view(psp + 0x80..psp + 0x81).unwrap(),
            [tail.len() as u8]
        );
        assert_eq!(
            media.ram_view(psp + 0x81..psp + 0x81 + tail.len()).unwrap(),
            tail
        );
        media.start();
        media.advance(1000);
        assert_eq!(
            media.status(),
            SessionStatus::Exited(5),
            "{:?}",
            media.fault()
        );
        assert_eq!(
            media
                .ram_view(psp + 0x100 + 0x40..psp + 0x100 + 0x44)
                .unwrap(),
            b"SUB\0"
        );
        assert!(!media.has_closed_writes());
        assert!(!executable.parent().unwrap().join("NEW.DAT").exists());
        assert!(!config.parent().unwrap().join("absent store").exists());
        assert_eq!(fs::read(executable).unwrap(), exe);
        assert_eq!(fs::read(disc).unwrap(), iso);
    }

    #[test]
    fn native_profile_without_mount_uses_executable_directory() {
        let files = Files::new();
        files.write("game files/GAME.EXE", &mz_fixture::relocated_mz());
        let config = files.write(
            "profile.toml",
            br#"
version = 2
[game]
executable = 'game files/GAME.EXE'
"#,
        );
        let mut media = load(&config).unwrap().media;
        media.start();
        media.advance(1000);
        assert_eq!(media.status(), SessionStatus::Exited(7));
    }

    #[test]
    fn zip_wrapper_selection_keeps_temporary_overlay_and_immutable_source() {
        let files = Files::new();
        // Create a DOS file and close it; the ZIP must receive no writeback.
        let mut com = vec![
            0xba, 0x20, 1, 0x31, 0xc9, 0xb8, 0, 0x3c, 0xcd, 0x21, 0x89, 0xc3, 0xb4, 0x3e, 0xcd,
            0x21, 0xb8, 0, 0x4c, 0xcd, 0x21,
        ];
        com.resize(0x20, 0);
        com.extend_from_slice(b"SAVE.DAT\0");
        let bytes = archive::tests::pack(
            &[("WRAPPER/GAME.COM", &com)],
            zip::CompressionMethod::Deflated,
        );
        let source = files.write("game archive.zip", bytes.as_slice());
        let config = files.write(
            "profile dir/zip.toml",
            br#"
version = 2
[game]
executable = 'game.com'
[[drives]]
letter = 'C'
kind = 'zip'
source = 'archive'
policy = 'persistent'
store = 'absent saves'
[[bindings]]
id = 'archive'
kind = 'zip'
location = '../game archive.zip'
"#,
        );
        let Input { exe, mut media } = load(&config).unwrap();
        assert_eq!(exe, com);
        assert!(!media.dynamic_core_enabled());
        media.start();
        media.advance(1000);
        assert_eq!(media.status(), SessionStatus::Exited(0));
        assert!(media.has_closed_writes());
        assert_eq!(fs::read(source).unwrap(), bytes.as_slice());
        assert!(!config.parent().unwrap().join("absent saves").exists());
    }

    #[test]
    fn oversized_direct_input_is_rejected() {
        let files = Files::new();
        let path = files.write("input", &[0; 17]);
        assert!(
            read(&path, 16)
                .unwrap_err()
                .to_string()
                .contains("input limit")
        );
    }
}
