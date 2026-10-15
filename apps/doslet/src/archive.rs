//! Host setup only: a ZIP remains the immutable source for each new session.
use doslet_dos::files::ReadOnlyDrive;
use doslet_runtime::Session;
use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
    sync::Arc,
};
#[cfg(test)]
#[path = "../../../tests/fixtures/relocated_mz.rs"]
mod fixture;

pub const MAX_ZIP_BYTES: usize = 64 * 1024 * 1024;
const MAX_DATA_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

#[derive(Clone)]
pub struct GameArchive {
    bytes: Arc<Vec<u8>>,
    pub programs: Vec<String>,
}
impl GameArchive {
    pub fn new(bytes: Arc<Vec<u8>>) -> Result<Self, String> {
        if bytes.len() > MAX_ZIP_BYTES {
            return Err("ZIP exceeds the 64-MiB limit".into());
        }
        let mut zip =
            zip::ZipArchive::new(Cursor::new(bytes.as_slice())).map_err(|e| e.to_string())?;
        if zip.len() > MAX_ENTRIES {
            return Err("ZIP exceeds the 4096-entry limit".into());
        }
        let mut names = BTreeSet::new();
        let mut programs = Vec::new();
        let mut total = 0u64;
        for i in 0..zip.len() {
            let file = zip.by_index(i).map_err(|e| e.to_string())?;
            let name = checked_name(file.name())?;
            if file.is_symlink() {
                return Err("ZIP symbolic links are unsupported".into());
            }
            if file.is_dir() {
                continue;
            }
            if !names.insert(name.clone()) {
                return Err(format!("Duplicate DOS path: {name}"));
            }
            total = total.checked_add(file.size()).ok_or("Invalid ZIP sizes")?;
            if file.size() > MAX_ZIP_BYTES as u64 || total > MAX_DATA_BYTES {
                return Err("ZIP exceeds the 64-MiB file or 256-MiB expanded-size limit".into());
            }
            if name.ends_with(".EXE") || name.ends_with(".COM") {
                programs.push(name);
            }
        }
        if programs.is_empty() {
            return Err("ZIP contains no EXE or COM program".into());
        }
        programs.sort();
        Ok(Self { bytes, programs })
    }
    pub fn load(&self, program: &str) -> Result<Session, String> {
        self.load_configured(program, &doslet_config::MachineSpec::default())
    }
    pub fn load_configured(
        &self,
        program: &str,
        spec: &doslet_config::MachineSpec,
    ) -> Result<Session, String> {
        let plan = spec.resolve().map_err(|e| e.to_string())?;
        if !self.programs.iter().any(|name| name == program) {
            return Err("Choose a program from the ZIP".into());
        }
        let prefix = program.rfind('/').map_or("", |index| &program[..=index]);
        let filename = &program[prefix.len()..];
        let mut zip =
            zip::ZipArchive::new(Cursor::new(self.bytes.as_slice())).map_err(|e| e.to_string())?;
        let mut drive = ReadOnlyDrive::new();
        if spec
            .mounts
            .iter()
            .find(|m| m.letter == b'C')
            .is_none_or(|m| m.policy != doslet_config::WritePolicy::ReadOnly)
        {
            drive = drive.with_write_overlay(1024 * 1024);
        }
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
            if file.is_dir() {
                continue;
            }
            let name = checked_name(file.name())?;
            let Some(relative) = name.strip_prefix(prefix) else {
                continue;
            };
            let mut data = Vec::with_capacity(file.size() as usize);
            file.by_ref()
                .take(MAX_ZIP_BYTES as u64 + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            if data.len() as u64 != file.size() || data.len() > MAX_ZIP_BYTES {
                return Err(format!("Invalid expanded size: {name}"));
            }
            drive
                .mount(relative.as_bytes(), data)
                .map_err(|code| format!("Cannot mount {relative}: DOS error {code}"))?;
        }
        let data = drive
            .file(filename.as_bytes())
            .map_err(|code| format!("Cannot load {filename}: DOS error {code}"))?;
        let session = if filename.ends_with(".COM") {
            Session::new_configured(data, plan.hardware)
        } else {
            Session::from_mz_stub_named_configured(
                data,
                format!("C:\\{filename}").as_bytes(),
                plan.hardware,
            )
        }
        .map_err(|e| e.to_string())?;
        let mut session = session.with_drive(drive);
        session
            .configure_launch(&plan.command_tail, spec.game.working_directory.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(session)
    }
}

fn checked_name(raw: &str) -> Result<String, String> {
    let name = raw.replace('\\', "/");
    if !name.is_ascii()
        || name.len() > 255
        || name.starts_with('/')
        || name
            .bytes()
            .any(|b| b < 32 || matches!(b, b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|'))
        || name
            .trim_end_matches('/')
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("Unsupported ZIP path: {raw}"));
    }
    Ok(name.to_ascii_uppercase())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
    pub fn pack(files: &[(&str, &[u8])], method: CompressionMethod) -> Arc<Vec<u8>> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(method),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        Arc::new(writer.finish().unwrap().into_inner())
    }
    #[test]
    fn root_and_wrapped_installations_load_and_reset_from_immutable_zip() {
        for method in [CompressionMethod::Stored, CompressionMethod::Deflated] {
            for root in ["", "PRINCE/"] {
                let exe = format!("{root}GAME.EXE");
                let data = format!("{root}DATA/TEST.DAT");
                let zip = GameArchive::new(pack(
                    &[(&exe, &fixture::relocated_mz()), (&data, b"input")],
                    method,
                ))
                .unwrap();
                assert_eq!(zip.programs, [exe.to_ascii_uppercase()]);
                for _ in 0..2 {
                    let mut session = zip.load(&exe.to_ascii_uppercase()).unwrap();
                    session.start();
                    session.advance(2000);
                    assert_eq!(session.status(), doslet_runtime::SessionStatus::Exited(7));
                }
            }
        }
    }
    #[test]
    fn invalid_archives_are_rejected_before_mounting() {
        for files in [
            vec![("../GAME.EXE", b"x".as_slice())],
            vec![("C:/GAME.EXE", b"x")],
            vec![("GAME.EXE", b"x"), ("game.exe", b"y")],
            vec![("DATA.BIN", b"x")],
        ] {
            assert!(GameArchive::new(pack(&files, CompressionMethod::Stored)).is_err());
        }
        assert!(GameArchive::new(Arc::new(b"not a ZIP".to_vec())).is_err());
    }
    #[test]
    fn com_programs_are_supported() {
        let zip = GameArchive::new(pack(
            &[("DEMO.COM", &[0xb8, 0, 0x4c, 0xcd, 0x21])],
            CompressionMethod::Deflated,
        ))
        .unwrap();
        let mut session = zip.load("DEMO.COM").unwrap();
        session.start();
        session.advance(100);
        assert_eq!(session.status(), doslet_runtime::SessionStatus::Exited(0));
        assert!(zip.load("MISSING.EXE").is_err());
    }

    #[test]
    fn declared_expansion_limits_and_corrupt_file_data_are_rejected() {
        let mut bytes = (*pack(
            &[("GAME.COM", b"unique corrupt data")],
            CompressionMethod::Stored,
        ))
        .clone();
        let header = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        bytes[header + 24..header + 28]
            .copy_from_slice(&((MAX_ZIP_BYTES as u32) + 1).to_le_bytes());
        assert!(GameArchive::new(Arc::new(bytes)).is_err());
        let mut bytes = (*pack(
            &[("GAME.COM", b"unique corrupt data")],
            CompressionMethod::Stored,
        ))
        .clone();
        let offset = bytes
            .windows(19)
            .position(|w| w == b"unique corrupt data")
            .unwrap();
        bytes[offset] ^= 1;
        let archive = GameArchive::new(Arc::new(bytes)).unwrap();
        assert!(archive.load("GAME.COM").is_err());
    }

    #[test]
    #[ignore = "requires local PRINCE; creates an ignored ZIP for browser import"]
    fn local_prince_zip_round_trip() {
        use std::{fs, path::Path};
        fn add_directory(writer: &mut ZipWriter<Cursor<Vec<u8>>>, root: &Path, directory: &Path) {
            let mut entries = fs::read_dir(directory)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let kind = entry.file_type().unwrap();
                let path = entry.path();
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    add_directory(writer, root, &path);
                } else if kind.is_file() {
                    let name = format!(
                        "PRINCE/{}",
                        path.strip_prefix(root)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .replace('\\', "/")
                    );
                    writer
                        .start_file(
                            name,
                            SimpleFileOptions::default()
                                .compression_method(CompressionMethod::Deflated),
                        )
                        .unwrap();
                    std::io::copy(&mut fs::File::open(path).unwrap(), writer).unwrap();
                }
            }
        }
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = repository.join("PRINCE");
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        add_directory(&mut writer, &root, &root);
        let bytes = Arc::new(writer.finish().unwrap().into_inner());
        let destination = repository.join("target/prince-game.zip");
        fs::write(&destination, bytes.as_slice()).unwrap();
        println!(
            "Local browser input: {} ({} bytes)",
            destination.display(),
            bytes.len()
        );
        let archive = GameArchive::new(bytes).unwrap();
        let mut session = archive.load("PRINCE/PRINCE.EXE").unwrap();
        session.start();
        let mut midi = [0; 1024];
        let mut pcm = [[0; 2]; 128];
        while session.steps() < 300_000_000 {
            session.advance(10_000);
            assert_eq!(
                session.status(),
                doslet_runtime::SessionStatus::Running,
                "{:?}",
                session.fault()
            );
            session.drain_midi(&mut midi);
            while session.drain_pcm(&mut pcm) != 0 {}
        }
        assert!(session.midi_stats().1 > 0);
        let frame = session.frame().unwrap();
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
        println!(
            "ZIP-mounted real game: {} steps, {} MIDI bytes, nonblank 640x480",
            session.steps(),
            session.midi_stats().1
        );
    }
}
