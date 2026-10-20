//! Build a self-contained static game site using Cargo and Trunk only.
mod aot;
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/host_dirs.rs"]
mod host_dirs;
#[allow(dead_code)]
#[path = "../../common/i18n.rs"]
mod i18n;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;
#[allow(dead_code)]
#[path = "../../common/native_store_format.rs"]
mod native_store_format;
mod procedures;
mod server;
#[path = "../../common/web_speed_tiers.rs"]
mod web_speed_tiers;

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use doslet_config::{BindingKind, HostBinding, MachineSpec, MountSpec, SourceKind, WritePolicy};
use doslet_storage::StorageError;
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};
use zip::{ZipWriter, write::SimpleFileOptions};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum Aot {
    Auto,
    Required,
    Off,
}

/// Register-code blocks end at every memory access. Below this mean length a
/// grant costs more than it runs: ROTT averages 2.5 instructions and measured
/// 2–8% slower than the interpreter, so `--aot auto` skips it.
const MIN_AUTO_BLOCK_INSTRUCTIONS: usize = 4;

#[derive(Parser, Debug)]
#[command(version, about = "Build and preview portable Doslet game websites")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand, Debug)]
enum Action {
    /// Build a self-contained static website.
    Build(Args),
    /// Build a website, then serve it on localhost for a quick browser test.
    Serve {
        #[command(flatten)]
        build: Args,
        #[arg(short, long, default_value_t = 8080)]
        port: u16,
    },
}
#[derive(ClapArgs, Debug)]
struct Args {
    /// Existing version-2 game profile; paths resolve beside this file.
    config: PathBuf,
    /// New output directory (existing paths are never overwritten).
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Reuse an already built Trunk distribution instead of building it.
    #[arg(long)]
    web_dist: Option<PathBuf>,
    /// Use cached memory procedures or prepare guarded register code; unsupported code uses fallback.
    #[arg(long, value_enum, default_value = "auto")]
    aot: Aot,
    /// Optional observed code profile (DSJ1 or speed-tiers JSON), used for AOT only.
    #[arg(long)]
    aot_profile: Option<PathBuf>,
    /// Reuse an exact native procedure_memory bundle instead of register-code AOT.
    /// Its bound source addresses must also match the browser's loader layout.
    #[arg(long)]
    wasm_procedures: Option<PathBuf>,
    /// Enable HOT/COLD/GREY CPU pacing from an exact-executable JSON profile.
    /// Can be combined with AOT; translated work uses the same live labels.
    #[arg(long)]
    speed_tiers: Option<PathBuf>,
    /// Explicit directory containing MT32 or CM32L CONTROL/PCM ROMs.
    #[arg(long)]
    rom_dir: Option<PathBuf>,
    /// Also pass --offline to the frontend's Cargo/Trunk build.
    #[arg(long)]
    offline: bool,
    /// Build from the original game files, without merging desktop saves.
    #[arg(long)]
    no_saves: bool,
    /// Override game.startup_keys; arguments are joined in order.
    #[arg(long, num_args = 1..=15)]
    startup_keys: Vec<String>,
}

fn read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(format!("{} exceeds {max} bytes", path.display()).into());
    }
    Ok(bytes)
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
/// The desktop overlay of a persistent C: store, to merge into the game ZIP. The
/// store resolves as on the desktop: a direct path beside the profile, or a
/// binding whose bare name lives in the per-user save directory.
fn native_saves(spec: &MachineSpec, base: &Path) -> Result<Option<(Vec<u8>, PathBuf)>> {
    let mount = &spec.mounts[0];
    let Some(store) = mount
        .store
        .as_deref()
        .filter(|_| mount.policy == WritePolicy::Persistent)
    else {
        return Ok(None);
    };
    let directory = if spec.host.bindings.iter().any(|binding| binding.id == store) {
        base.join(host_dirs::store_directory(
            spec.binding(store),
            host_dirs::HostDirs::detect,
        )?)
    } else {
        base.join(store)
    };
    let payload = native_store_format::read_committed(&directory).map_err(|error| {
        if error == StorageError::BusyWriter {
            format!(
                "{}: save store is open; close Doslet first",
                directory.display()
            )
        } else {
            format!("{}: unreadable save store ({error:?})", directory.display())
        }
    })?;
    Ok(payload.map(|payload| (payload, directory)))
}
/// Cover art is copied unchanged; its signature selects the served type.
fn cover_image(path: &Path) -> Result<(Vec<u8>, &'static str)> {
    let bytes = read(path, 16 * 1024 * 1024)?;
    let extension = if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "webp"
    } else {
        return Err(format!(
            "{}: game.cover must be a JPEG, PNG or WebP image",
            path.display()
        )
        .into());
    };
    Ok((bytes, extension))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
    }
    result
}

// Reject symlinks and special files rather than accidentally packaging host data.
fn files(
    root: &Path,
    at: &Path,
    out: &mut Vec<PathBuf>,
    total: &mut u64,
    limit: u64,
    visited: &mut usize,
) -> Result<()> {
    if at.strip_prefix(root)?.components().count() > 32 {
        return Err("directory depth exceeds 32".into());
    }
    for entry in fs::read_dir(at)? {
        *visited += 1;
        if *visited > 8192 {
            return Err("directory traversal exceeds 8192 entries".into());
        }
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            files(root, &path, out, total, limit, visited)?;
        } else if meta.is_file() {
            *total = total
                .checked_add(meta.len())
                .ok_or("directory size overflow")?;
            if *total > limit || out.len() >= 4096 {
                return Err("directory exceeds bundle size/file limits".into());
            }
            out.push(path);
        } else {
            return Err(format!("unsupported symlink or special file: {}", path.display()).into());
        }
    }
    Ok(())
}
fn zip_directory(root: &Path) -> Result<Vec<u8>> {
    let mut paths = Vec::new();
    files(root, root, &mut paths, &mut 0, 256 * 1024 * 1024, &mut 0)?;
    paths.sort();
    let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for path in paths {
        let name = path
            .strip_prefix(root)?
            .to_str()
            .ok_or("non-UTF8 game path")?
            .replace('\\', "/");
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )?;
        zip.write_all(&read(&path, archive::MAX_ZIP_BYTES)?)?;
    }
    let bytes = zip.finish()?.into_inner();
    if bytes.len() > archive::MAX_ZIP_BYTES {
        return Err("compressed game exceeds browser's 64 MiB ZIP limit".into());
    }
    Ok(bytes)
}

/// Materialize the imported C: view as one ZIP. The loader mounts only the
/// selected program's directory, so files elsewhere in the source ZIP pass
/// through unchanged. DOS-visible names and bytes come from the same VFS view
/// that validated the native overlay.
fn flatten_zip(
    source: &[u8],
    executable: &str,
    session: &doslet_runtime::Session,
) -> Result<Vec<u8>> {
    if session.drive_has_empty_directories() {
        return Err("cannot flatten saves containing empty DOS directories".into());
    }
    let prefix = executable
        .rfind('/')
        .map_or("", |index| &executable[..=index]);
    let mut input = zip::ZipArchive::new(std::io::Cursor::new(source))?;
    let mut output = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut count = 0usize;
    let mut total = 0u64;
    for index in 0..input.len() {
        let mut file = input.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().replace('\\', "/").to_ascii_uppercase();
        if name.starts_with(prefix) {
            continue;
        }
        count += 1;
        total = total
            .checked_add(file.size())
            .ok_or("expanded ZIP size overflow")?;
        if count > 4096 || total > 256 * 1024 * 1024 {
            return Err("merged game exceeds ZIP entry or expanded-size limit".into());
        }
        output.start_file(name, options)?;
        let expected = file.size();
        if std::io::copy(&mut file, &mut output)? != expected {
            return Err("source ZIP entry has an invalid expanded size".into());
        }
    }
    let mut chunk = [0u8; 65536];
    for file in session.visible_drive_files() {
        let relative = std::str::from_utf8(file.name())?.replace('\\', "/");
        let name = format!("{prefix}{relative}");
        let length = file.len() as usize;
        count += 1;
        total = total
            .checked_add(length as u64)
            .ok_or("expanded ZIP size overflow")?;
        if count > 4096 || length > archive::MAX_ZIP_BYTES || total > 256 * 1024 * 1024 {
            return Err("merged game exceeds ZIP entry or expanded-size limit".into());
        }
        output.start_file(name, options)?;
        let mut position = 0usize;
        while position < length {
            let size = (length - position).min(chunk.len());
            file.read_into(position as u32, &mut chunk[..size]);
            output.write_all(&chunk[..size])?;
            position += size;
        }
    }
    let bytes = output.finish()?.into_inner();
    if bytes.len() > archive::MAX_ZIP_BYTES {
        return Err("merged game exceeds browser's 64 MiB ZIP limit".into());
    }
    Ok(bytes)
}

/// A staging guard only removes a directory this invocation created. The final
/// destination must not exist; failures leave no half-published website.
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn stage(bar: &ProgressBar, n: u64, text: &str) {
    bar.set_position(n);
    bar.set_message(text.to_owned());
    if bar.is_hidden() {
        eprintln!("[{}/6] {text}", n + 1);
    }
}
fn report(bar: &ProgressBar, text: &str) {
    if bar.is_hidden() {
        eprintln!("{text}");
    } else {
        bar.println(text);
    }
}
fn build(args: &Args, bar: &ProgressBar) -> Result<PathBuf> {
    if args.aot == Aot::Off && (args.aot_profile.is_some() || args.wasm_procedures.is_some()) {
        return Err("--aot-profile and --wasm-procedures cannot be used with --aot off".into());
    }
    if args.aot_profile.is_some() && args.wasm_procedures.is_some() {
        return Err("--aot-profile and --wasm-procedures select different AOT backends".into());
    }
    let default_output = PathBuf::from("target").join(format!(
        "{}-web",
        args.config
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("game")
    ));
    let output = args.output.as_ref().unwrap_or(&default_output);
    if output.symlink_metadata().is_ok() {
        return Err("output already exists; choose a new directory".into());
    }
    stage(bar, 0, "Reading and validating configuration");
    let config = args.config.canonicalize()?;
    let base = config.parent().ok_or("profile has no parent")?;
    let mut spec =
        machine_profile::parse_machine_profile(std::str::from_utf8(&read(&config, 1024 * 1024)?)?)?;
    if !args.startup_keys.is_empty() {
        let keys = args.startup_keys.join("");
        if keys.len() > 15
            || !keys
                .bytes()
                .all(|byte| doslet_config::bios_key_for_ascii(byte).is_some())
        {
            return Err(
                "--startup-keys supports at most 15 printable ASCII or control keys".into(),
            );
        }
        spec.game.startup_keys = keys;
    }
    if spec.mounts.is_empty() {
        let executable = base.join(&spec.game.executable).canonicalize()?;
        let directory = executable.parent().ok_or("executable has no directory")?;
        spec.mounts.push(MountSpec {
            letter: b'C',
            kind: SourceKind::NativeDirectory,
            source: directory.to_str().ok_or("non-UTF8 game directory")?.into(),
            policy: WritePolicy::Temporary,
            store: None,
        });
        spec.game.executable = executable
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("non-UTF8 executable")?
            .into();
    }
    if spec.mounts.len() != 1 || spec.mounts[0].letter != b'C' {
        return Err(
            "web packaging currently requires exactly one C: directory or ZIP mount".into(),
        );
    }
    if !spec.game.patches.is_empty() {
        return Err("browser loader does not support game.patches".into());
    }
    let cover = spec
        .info
        .cover
        .as_ref()
        .map(|path| cover_image(&base.join(path)))
        .transpose()?;
    let mount = &spec.mounts[0];
    let source = base.join(spec.binding(&mount.source)).canonicalize()?;
    let rom_directory = args.rom_dir.clone().or_else(|| {
        spec.output
            .rom_binding
            .as_ref()
            .map(|id| base.join(spec.binding(id)))
    });
    if !matches!(
        mount.kind,
        SourceKind::NativeDirectory | SourceKind::BrowserZip
    ) {
        return Err("C: must be a directory or ZIP".into());
    }
    // Canonicalize the output parent before any tree walk. Never package output
    // back into the game or into a reused frontend distribution.
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let destination = parent.join(output.file_name().ok_or("output needs a directory name")?);
    if source.is_dir() && parent.starts_with(&source) {
        return Err("output cannot be inside the game directory".into());
    }
    let staging_path = parent.join(format!(".doslet-web-{}", std::process::id()));
    fs::create_dir(&staging_path)?;
    let mut staging = Staging(staging_path);
    let bundle = staging.0.join("bundle");
    fs::create_dir(&bundle)?;
    stage(bar, 1, "Packing game files");
    let mut zip = match mount.kind {
        SourceKind::NativeDirectory => zip_directory(&source)?,
        _ => read(&source, archive::MAX_ZIP_BYTES)?,
    };
    let game = archive::GameArchive::new(Arc::new(zip.clone()))?;
    let host_executable = base.join(&spec.game.executable);
    let wanted = if source.is_dir() && host_executable.is_file() {
        host_executable
            .canonicalize()?
            .strip_prefix(&source)
            .map_err(|_| "executable lies outside the C: directory")?
            .to_str()
            .ok_or("non-UTF8 executable")?
            .replace('\\', "/")
            .to_ascii_uppercase()
    } else {
        spec.game.executable.replace('\\', "/").to_ascii_uppercase()
    };
    let selected: Vec<_> = game
        .programs
        .iter()
        .filter(|p| **p == wanted || p.ends_with(&format!("/{wanted}")))
        .collect();
    if selected.len() != 1 {
        return Err(format!(
            "expected one {}, found {} in game archive",
            spec.game.executable,
            selected.len()
        )
        .into());
    }
    let executable = selected[0].clone();
    // Validate the actual browser loader before invoking a long frontend build.
    let mut browser_spec = spec.clone();
    browser_spec.game.executable = executable.clone();
    browser_spec.mounts[0].kind = SourceKind::BrowserZip;
    browser_spec.mounts[0].policy = WritePolicy::Temporary;
    browser_spec.mounts[0].store = None;
    browser_spec.mounts[0].source = "selected-zip".into();
    browser_spec.host.bindings.clear();
    // The imported overlay is validated against the original immutable C:
    // files, then materialized into the only game archive shipped to the web.
    if let Some((payload, directory)) = if args.no_saves {
        None
    } else {
        native_saves(&spec, base)?
    } {
        let mut browser_session = game.load_configured(&executable, &browser_spec)?;
        browser_session.import_overlay(&payload).map_err(|error| {
            format!(
                "{}: saves do not match this game's files ({error:?}); use --no-saves",
                directory.display()
            )
        })?;
        zip = flatten_zip(&zip, &executable, &browser_session)?;
        report(
            bar,
            &format!("Merged saves from {} into game.zip", directory.display()),
        );
    }
    let game = archive::GameArchive::new(Arc::new(zip.clone()))?;
    let exe = game.program_bytes(&executable)?;
    let browser_session = game.load_configured(&executable, &browser_spec)?;
    fs::write(bundle.join("game.zip"), &zip)?;
    let identity = hash(&exe);
    let base_identity = hex_bytes(&browser_session.drive_base_identity());
    let mut manifest = json!({"version":1,"profile":"game.toml","zip":"game.zip","executable":executable,"executableSha256":identity,"bundleRuntime":true});
    manifest["displayFilter"] = json!(spec.output.video_filter.as_str());
    if !spec.game.startup_keys.is_empty() {
        manifest["startupKeys"] = json!(&spec.game.startup_keys);
    }
    if let Some(path) = &args.speed_tiers {
        let bytes = read(path, 64 * 1024 * 1024)?;
        let text = std::str::from_utf8(&bytes)?;
        let value: serde_json::Value = serde_json::from_str(text)?;
        if value["identity"].as_str() != Some(format!("exe-sha256:{identity}").as_str()) {
            return Err(
                "speed-tier profile must identify the selected executable (exe-sha256)".into(),
            );
        }
        let profile = web_speed_tiers::parse_tier_profile(text)?;
        if profile.labels.is_empty() {
            return Err("speed-tier profile contains no labels".into());
        }
        fs::write(bundle.join("speed-tiers.json"), &bytes)?;
        manifest["tierProfile"] = json!("speed-tiers.json");
        report(
            bar,
            &format!(
                "HOT/COLD/GREY: {} labels, rates {:?}.",
                profile.labels.len(),
                profile.rates
            ),
        );
    }
    stage(bar, 2, "Preparing guarded AOT code");
    // The worker runs bundles with the profile's DPMI host, direct by default.
    let dpmi = spec.dpmi.unwrap_or(doslet_config::DpmiHost::Direct);
    let procedure_path = if args.aot == Aot::Off || args.aot_profile.is_some() {
        None
    } else if let Some(path) = &args.wasm_procedures {
        Some(path.clone())
    } else {
        procedures::cached_path(&identity, dpmi.as_str())?
    };
    let procedure_bundle = procedure_path
        .as_deref()
        .map(|path| procedures::load(path, &identity, dpmi.as_str()))
        .transpose()?;
    let prepared = if args.aot == Aot::Off || procedure_bundle.is_some() {
        None
    } else if let Some(path) = args.aot_profile.as_ref().or(args.speed_tiers.as_ref()) {
        let profile = read(path, 64 * 1024 * 1024)?;
        if !profile.starts_with(b"DSJ1") {
            let value: serde_json::Value = serde_json::from_slice(&profile)?;
            if let Some(id) = value["identity"].as_str()
                && id != format!("exe-sha256:{identity}")
            {
                return Err("AOT profile identity does not match the selected executable".into());
            }
        }
        let translation = doslet_x86_wasm::Translation::from_bytes(&profile)?;
        Some((profile, translation))
    } else {
        aot::prepare(&exe)?
    };
    let mut compiled_entries = 0;
    let mut fragmented = false;
    let mut aot_description = "guarded 32-bit register code";
    if let Some(prepared) = procedure_bundle {
        compiled_entries = prepared.entries;
        aot_description = "guarded shared-memory procedures";
        manifest["aot"] = json!({"kind":"procedure-memory","module":"procedures.wasm",
            "moduleSha256":hash(&prepared.module),"entries":compiled_entries});
        fs::write(bundle.join("procedures.wasm"), prepared.module)?;
    } else if let Some((_, translation)) = prepared {
        let blocks = translation.blocks();
        let instructions: usize = blocks.iter().map(|block| block.instructions.len()).sum();
        if args.aot == Aot::Auto
            && args.aot_profile.is_none()
            && instructions < MIN_AUTO_BLOCK_INSTRUCTIONS * blocks.len()
        {
            fragmented = true;
            report(
                bar,
                &format!(
                    "AOT: register code splits into {:.1}-instruction pieces on average (minimum {MIN_AUTO_BLOCK_INSTRUCTIONS}); using the interpreter. --aot required packages it anyway.",
                    instructions as f64 / blocks.len() as f64
                ),
            );
        } else {
            compiled_entries = blocks.len();
        }
        if compiled_entries != 0 {
            let wasm = translation.wasm();
            manifest["aot"] =
                json!({"module":"aot.wasm","moduleSha256":hash(&wasm),"entries":compiled_entries});
            fs::write(bundle.join("aot.wasm"), wasm)?;
        }
    }
    if compiled_entries == 0 {
        if args.aot == Aot::Required {
            return Err(
                "no supported AOT entries; use --aot auto/off for interpreter fallback".into(),
            );
        }
        if !fragmented {
            report(
                bar,
                "AOT: no supported 32-bit register entries; using the interpreter.",
            );
        }
    } else if compiled_entries != 0 {
        report(
            bar,
            &format!("AOT: {compiled_entries} entries prepared ({aot_description})."),
        );
    }
    stage(bar, 3, "Preparing portable settings and audio");
    spec.game.executable = executable;
    let persistent = matches!(
        spec.mounts[0].policy,
        WritePolicy::Persistent | WritePolicy::LegacyWriteback
    );
    spec.mounts[0].kind = SourceKind::BrowserZip;
    spec.mounts[0].source = "selected-zip".into();
    spec.mounts[0].policy = if persistent {
        WritePolicy::Persistent
    } else {
        spec.mounts[0].policy
    };
    spec.mounts[0].store = persistent.then(|| "browser-saves".into());
    spec.host.bindings = vec![HostBinding {
        id: "selected-zip".into(),
        kind: BindingKind::BrowserZip,
        location: "bundled/game.zip".into(),
    }];
    if persistent {
        spec.host.bindings.push(HostBinding {
            id: "browser-saves".into(),
            kind: BindingKind::IndexedDb,
            location: format!("doslet-web-{identity}-{base_identity}"),
        });
    }
    if spec.output.audio && spec.output.midi_output == "moont" {
        let roms = rom_directory.as_ref().ok_or(
            "MT-32 output requires --rom-dir, or choose output.midi_output='rust' in the profile",
        )?;
        let prefix = if roms.join("CM32L_CONTROL.ROM").is_file() {
            "CM32L"
        } else {
            "MT32"
        };
        let names = [format!("{prefix}_CONTROL.ROM"), format!("{prefix}_PCM.ROM")];
        for name in &names {
            fs::write(bundle.join(name), read(&roms.join(name), 4 * 1024 * 1024)?)?;
        }
        manifest["roms"] = json!(names);
        spec.output.rom_binding = Some("bundle".into());
    } else if spec.output.midi_output != "rust" && spec.output.audio {
        return Err("portable audio requires midi_output='rust' or 'moont'; host MIDI ports cannot be bundled".into());
    } else {
        spec.output.rom_binding = None;
    }
    if let Some((bytes, extension)) = &cover {
        let name = format!("cover.{extension}");
        fs::write(bundle.join(&name), bytes)?;
        manifest["cover"] = json!(name);
        spec.info.cover = Some(name);
    }
    fs::write(
        bundle.join("game.toml"),
        machine_profile::export_machine_profile(&spec),
    )?;
    fs::write(
        bundle.join("launch.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    stage(bar, 4, "Building browser runtime with Trunk");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let dist = if let Some(path) = &args.web_dist {
        path.canonicalize()?
    } else {
        let log = fs::File::create(staging.0.join("build.log"))?;
        let mut command = Command::new("trunk");
        command
            .current_dir(root.join("web"))
            .env("NO_COLOR", "true")
            .args(["build", "--release", "--locked"]);
        if args.offline {
            command.arg("--offline");
        }
        let status = command
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()
            .map_err(|e| {
                format!("cannot run Trunk ({e}); install pinned tools with mise install")
            })?;
        if !status.success() {
            let log = fs::read_to_string(staging.0.join("build.log"))?;
            eprintln!(
                "{}",
                log.lines()
                    .rev()
                    .take(80)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return Err(format!("Trunk failed: {status}").into());
        }
        root.join("web/dist").canonicalize()?
    };
    if parent.starts_with(&dist) {
        return Err("output cannot be inside the reused web distribution".into());
    }
    let html = String::from_utf8(read(&dist.join("index.html"), 4 * 1024 * 1024)?)?;
    if !html.contains("</body>") {
        return Err("frontend index.html has no closing body tag".into());
    }
    let mut paths = Vec::new();
    files(&dist, &dist, &mut paths, &mut 0, 1024 * 1024 * 1024, &mut 0)?;
    stage(bar, 5, "Assembling website");
    for path in paths {
        let relative = path.strip_prefix(&dist)?;
        if relative.starts_with("bundle") || relative == Path::new("README.txt") {
            return Err("web distribution is already a game bundle".into());
        }
        let to = staging.0.join(relative);
        fs::create_dir_all(to.parent().ok_or("missing output parent")?)?;
        fs::copy(path, to)?;
    }
    let title = escape(&spec.name);
    let info = &spec.info;
    let strings = i18n::EN;
    let details: String = [
        (
            "released",
            strings.released,
            info.released.map(|year| year.to_string()),
        ),
        (
            "publisher",
            strings.publisher,
            (!info.publishers.is_empty()).then(|| info.publishers.join(", ")),
        ),
        (
            "developer",
            strings.developer,
            (!info.developers.is_empty()).then(|| info.developers.join(", ")),
        ),
        (
            "genre",
            strings.genre,
            (!info.genres.is_empty()).then(|| info.genres.join(", ")),
        ),
        (
            "gameplay",
            strings.gameplay,
            (!info.gameplay.is_empty()).then(|| info.gameplay.join(", ")),
        ),
    ]
    .into_iter()
    .filter_map(|(key, label, value)| {
        Some(format!(
            r#"<dt data-i18n="{key}">{label}</dt><dd>{}</dd>"#,
            escape(&value?)
        ))
    })
    .collect();
    let details = if details.is_empty() {
        details
    } else {
        format!(r#"<dl id="doslet-bundle-info">{details}</dl>"#)
    };
    // Blank lines in [info] description separate paragraphs.
    let paragraphs: Vec<String> = info
        .description
        .iter()
        .flat_map(|text| text.split("\n\n"))
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|paragraph| !paragraph.is_empty())
        .collect();
    let about = if paragraphs.is_empty() {
        String::new()
    } else {
        let body: String = paragraphs
            .iter()
            .map(|paragraph| format!("<p>{}</p>", escape(paragraph)))
            .collect();
        format!(r#"<div id="doslet-bundle-about">{body}</div>"#)
    };
    // The start page shows the cover in the card over a blurred, dimmed copy.
    let art = cover
        .as_ref()
        .map(|(_, extension)| format!("bundle/cover.{extension}"));
    let backdrop = art.as_ref().map_or(String::new(), |src| {
        format!(r#"<div id="doslet-bundle-backdrop" style="background-image:url('{src}')"></div>"#)
    });
    let image = art.as_ref().map_or(String::new(), |src| {
        format!(r#"<img id="doslet-bundle-art" src="{src}" alt="">"#)
    });
    let overlay = format!(
        r#"<style>#doslet-bundle-cover{{position:fixed;inset:0;z-index:1000;display:grid;place-items:center;overflow:hidden;background:#111215;color:#ebedf0;font:16px system-ui}}#doslet-bundle-backdrop{{position:absolute;inset:-48px;background:center/cover no-repeat;filter:blur(28px) brightness(.35);transform:scale(1.08)}}#doslet-bundle-card{{position:relative;box-sizing:border-box;max-width:min(100vw,820px);padding:2rem 1rem;text-align:center}}#doslet-bundle-art{{display:block;width:100%;max-height:60vh;object-fit:contain;margin:0 auto 1.5rem;border-radius:12px;box-shadow:0 18px 60px #000a}}#doslet-bundle-card h1{{margin:0 0 1rem;text-shadow:0 2px 12px #000}}#doslet-bundle-info{{display:grid;grid-template-columns:auto auto;justify-content:center;gap:.25rem 1rem;margin:0 0 1.5rem;font-size:.9rem;text-align:left}}#doslet-bundle-info dt{{color:#9aa1ab}}#doslet-bundle-info dd{{margin:0}}#doslet-bundle-about{{max-width:620px;margin:0 auto 1.5rem;color:#c3c8cf;font-size:.95rem;line-height:1.55;text-align:left}}#doslet-bundle-about p{{margin:0 0 .75rem}}#doslet-bundle-launch{{font:700 1.05rem/1 system-ui;letter-spacing:.02em;padding:.9rem 2.4rem;border:0;border-radius:999px;background:linear-gradient(#f7c860,#e3892b);color:#1b1206;cursor:pointer;box-shadow:0 10px 30px #0009,inset 0 1px #fff6;transition:filter .15s,transform .15s}}#doslet-bundle-launch:hover:not(:disabled){{filter:brightness(1.08);transform:translateY(-1px)}}#doslet-bundle-launch:active:not(:disabled){{transform:translateY(1px)}}#doslet-bundle-launch:focus-visible{{outline:3px solid #f7c860;outline-offset:4px}}#doslet-bundle-launch:disabled{{background:#2e3138;color:#8b929c;cursor:default;box-shadow:none}}#doslet-bundle-status{{margin:1rem 0 0;color:#c3c8cf;font-size:.9rem;text-shadow:0 1px 6px #000}}</style><div id="doslet-bundle-cover">{backdrop}<div id="doslet-bundle-card">{image}<h1>{title}</h1>{details}{about}<button id="doslet-bundle-launch" disabled>{}</button><p id="doslet-bundle-status">{}</p></div></div><script type="module" src="browser/bundle-launch.js"></script>"#,
        strings.play_game, strings.loading_emulator,
    );
    // Name the page after the game and describe it for tabs and shared links.
    let heading = match info.released {
        Some(year) => format!("{} ({year})", spec.name),
        None => spec.name.clone(),
    };
    let summary = [
        info.publishers.join(", "),
        info.genres
            .iter()
            .chain(&info.gameplay)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let description = if let Some(first) = paragraphs.first() {
        first.clone()
    } else if summary.is_empty() {
        format!("{heading} — {}", strings.browser_dos_game_suffix)
    } else {
        format!("{heading}: {summary}. {}", strings.browser_dos_game_suffix)
    };
    let mut head = format!(
        r#"<title>{} · Doslet</title><meta name="description" content="{}"><meta property="og:type" content="website"><meta property="og:title" content="{}"><meta property="og:description" content="{}">"#,
        escape(&heading),
        escape(&description),
        escape(&heading),
        escape(&description)
    );
    if let Some(src) = &art {
        head += &format!(r#"<meta property="og:image" content="{src}">"#);
    }
    let html = match (html.find("<title>"), html.find("</title>")) {
        (Some(start), Some(end)) if start < end => {
            format!(
                "{}{head}{}",
                &html[..start],
                &html[end + "</title>".len()..]
            )
        }
        _ => html.replacen("</head>", &(head + "</head>"), 1),
    };
    fs::write(
        staging.0.join("index.html"),
        html.replace("</body>", &(overlay + "</body>")),
    )?;
    fs::write(
        staging.0.join("README.txt"),
        format!(
            "Doslet: {}\nServe this directory over HTTPS or http://localhost. No backend is required.\nAOT entries: {compiled_entries}; experimental {aot_description}. Live source, context and memory checks must admit each call; unsupported work uses the interpreter. No speedup claim is implied.\nGame media and any explicitly supplied ROMs are included.\n",
            spec.name
        ),
    )?;
    if destination.symlink_metadata().is_ok() {
        return Err("output appeared during build; refusing to replace it".into());
    }
    fs::rename(&staging.0, &destination)?;
    staging.0.clear();
    bar.finish_with_message("Website ready");
    Ok(destination)
}
fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = run(cli);
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let (mut args, port) = match cli.command {
        Action::Build(args) => (args, None),
        Action::Serve { build, port } => (build, Some(port)),
    };
    let temporary = if port.is_some() && args.output.is_none() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        fs::create_dir_all(&root)?;
        let temporary = tempfile::Builder::new()
            .prefix("doslet-preview-")
            .tempdir_in(root)?;
        args.output = Some(temporary.path().join("site"));
        Some(temporary)
    } else {
        None
    };
    let bar = ProgressBar::new(6);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:28.cyan/blue}] {pos}/{len} {msg} [{elapsed_precise}]",
        )
        .expect("progress template")
        .progress_chars("=>-"),
    );
    bar.enable_steady_tick(Duration::from_millis(100));
    match build(&args, &bar) {
        Ok(path) => {
            println!("{}", path.display());
            if let Some(port) = port {
                server::serve(&path, port)?;
            }
            drop(temporary);
            Ok(())
        }
        Err(error) => {
            bar.abandon_with_message("Build failed");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn web_dist(root: &Path) -> PathBuf {
        let dist = root.join("dist");
        fs::create_dir(&dist).unwrap();
        fs::write(dist.join("index.html"), "<html><body></body></html>").unwrap();
        dist
    }

    #[test]
    fn packages_native_memory_procedures_without_rewriting_the_module() {
        use doslet_x86_wasm::static_program::{
            Address, Width, memory,
            procedure_memory::{
                ProcedureCompileOptions, ProcedureFunction, ProcedureId, ProcedureProgram,
                ProcedureReturn, bundle,
            },
        };
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        fs::create_dir(root.join("media")).unwrap();
        let exe = [0xb8, 0, 0x4c, 0xcd, 0x21];
        fs::write(root.join("media/TEST.COM"), exe).unwrap();
        fs::write(
            root.join("game.toml"),
            "version=2\n[game]\nexecutable='media/TEST.COM'\n",
        )
        .unwrap();
        let entry = Address {
            space: 0,
            offset: 0x20000,
        };
        let program = ProcedureProgram {
            functions: vec![ProcedureFunction {
                id: ProcedureId(7),
                entry,
                instructions: vec![],
                terminator: ProcedureReturn {
                    address: entry,
                    width: Width::Dword,
                    pop_bytes: 0,
                },
            }],
        };
        let compiled = program
            .compile(&ProcedureCompileOptions::default())
            .unwrap();
        let metadata = json!({"version":2,"memory_abi_version":memory::VERSION,
            "exe_sha256":hash(&exe),"code_sha256":hash(compiled.wasm()),
            "functions":[{"id":7,"entry":entry.offset,"maximum_work":1,
                "sources":[{"linear":entry.offset,"bytes":[0xc3]}]}]});
        let module =
            bundle::append(compiled.wasm(), &serde_json::to_vec(&metadata).unwrap()).unwrap();
        fs::write(root.join("native.wasm"), &module).unwrap();
        let mut args = Args {
            config: root.join("game.toml"),
            output: Some(root.join("site")),
            web_dist: Some(web_dist(root)),
            aot: Aot::Required,
            aot_profile: None,
            wasm_procedures: Some(root.join("native.wasm")),
            speed_tiers: None,
            rom_dir: None,
            offline: true,
            no_saves: false,
            startup_keys: Vec::new(),
        };
        let site = build(&args, &ProgressBar::hidden()).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(site.join("bundle/launch.json")).unwrap()).unwrap();
        assert_eq!(manifest["aot"]["kind"], "procedure-memory");
        assert_eq!(manifest["aot"]["module"], "procedures.wasm");
        assert_eq!(manifest["aot"]["moduleSha256"], hash(&module));
        assert_eq!(manifest["aot"]["entries"], 1);
        assert_eq!(
            fs::read(site.join("bundle/procedures.wasm")).unwrap(),
            module
        );
        assert!(!site.join("bundle/aot.wasm").exists());
        assert!(!site.join("bundle/aot-profile.bin").exists());
        args.output = Some(root.join("bad-site"));
        args.aot_profile = Some(root.join("observed.json"));
        assert!(
            build(&args, &ProgressBar::hidden())
                .unwrap_err()
                .to_string()
                .contains("different AOT backends")
        );
        args.aot_profile = None;
        args.aot = Aot::Off;
        assert!(
            build(&args, &ProgressBar::hidden())
                .unwrap_err()
                .to_string()
                .contains("--aot off")
        );
        assert!(!root.join("bad-site").exists());
    }

    #[test]
    #[ignore = "requires the local ROTT archive and existing native procedure bundle"]
    fn local_rott_native_memory_bundle_packages_unchanged() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = tempfile::tempdir().unwrap();
        let path = repo.join("target/rott-aot-embedded-before-irq/procedures.wasm");
        let original = read(
            &path,
            doslet_x86_wasm::static_program::procedure_memory::bundle::MAX_BUNDLE_BYTES,
        )
        .unwrap();
        let args = Args {
            config: repo.join("games/ROTT.toml"),
            output: Some(root.path().join("site")),
            web_dist: Some(web_dist(root.path())),
            aot: Aot::Required,
            aot_profile: None,
            wasm_procedures: Some(path),
            speed_tiers: None,
            rom_dir: None,
            offline: true,
            no_saves: false,
            startup_keys: Vec::new(),
        };
        let site = build(&args, &ProgressBar::hidden()).unwrap();
        assert_eq!(
            fs::read(site.join("bundle/procedures.wasm")).unwrap(),
            original
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(site.join("bundle/launch.json")).unwrap()).unwrap();
        assert_eq!(manifest["aot"]["kind"], "procedure-memory");
        assert!(manifest["aot"]["entries"].as_u64().unwrap() > 0);
        assert_eq!(manifest["aot"]["moduleSha256"], hash(&original));
    }

    #[test]
    fn desktop_saves_are_flattened_into_the_game_zip() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut zip = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut zip));
            writer
                .start_file("PACK/TEST.COM", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&[0xb8, 0, 0x4c, 0xcd, 0x21]).unwrap();
            for (name, bytes) in [
                ("PACK/DEL.DAT", b"delete".as_slice()),
                ("PACK/OLD.DAT", b"rename".as_slice()),
                ("PACK/DATA.DAT", b"previous".as_slice()),
                ("PACK/TRUNC.DAT", b"previous".as_slice()),
                ("OTHER/KEEP.DAT", b"outside".as_slice()),
            ] {
                writer
                    .start_file(name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        fs::write(root.join("game.zip"), &zip).unwrap();
        let profile = "version=2\nname='Saves'\n[game]\nexecutable='PACK/TEST.COM'\n[[drives]]\nletter='C'\nkind='zip'\nsource='game.zip'\npolicy='persistent'\nstore='./saves'\n";
        fs::write(root.join("game.toml"), profile).unwrap();
        fs::create_dir(root.join("dist")).unwrap();
        fs::write(root.join("dist/index.html"), "<html><body></body></html>").unwrap();
        // A version-1 directory store, as written by earlier desktop builds.
        let mut drive = doslet_dos::files::ReadOnlyDrive::new().with_write_overlay(1024 * 1024);
        for (name, bytes) in [
            (b"TEST.COM".as_slice(), &[0xb8, 0, 0x4c, 0xcd, 0x21][..]),
            (b"DEL.DAT".as_slice(), b"delete".as_slice()),
            (b"OLD.DAT".as_slice(), b"rename".as_slice()),
            (b"DATA.DAT".as_slice(), b"previous".as_slice()),
            (b"TRUNC.DAT".as_slice(), b"previous".as_slice()),
        ] {
            drive.mount(name, bytes.to_vec()).unwrap();
        }
        let base_identity = drive.base_identity();
        drive.delete(b"DEL.DAT").unwrap();
        drive.rename(b"OLD.DAT", b"NEW.DAT").unwrap();
        let handle = drive.create(b"DATA.DAT", 0).unwrap();
        drive
            .write_buffer(handle, 3)
            .unwrap()
            .copy_from_slice(b"new");
        drive.close(handle).unwrap();
        let handle = drive.create(b"TRUNC.DAT", 0).unwrap();
        drive.seek(handle, 0, 5).unwrap();
        drive.write_buffer(handle, 1).unwrap()[0] = b'Z';
        drive.close(handle).unwrap();
        let handle = drive.create(b"SAVE.DAT", 0).unwrap();
        drive.write_buffer(handle, 50_000).unwrap().fill(b'A');
        drive.write_buffer(handle, 50_000).unwrap().fill(b'B');
        drive.close(handle).unwrap();
        let mut body = Vec::with_capacity(drive.snapshot_capacity());
        drive.encode_snapshot(&mut body).unwrap();
        let mut payload = b"DSV1".to_vec();
        payload.extend_from_slice(&base_identity);
        payload.extend_from_slice(&body);
        let store = |payload: &[u8]| {
            let digest = doslet_storage::payload_digest(payload);
            let manifest = native_store_format::Manifest {
                namespace: [1; 32],
                base: [2; 32],
                fence: 1,
                revision: 1,
                digest,
                ledger: vec![(1, digest, 1)],
            };
            fs::create_dir_all(root.join("saves")).unwrap();
            fs::write(root.join("saves/manifest.bin"), manifest.encode()).unwrap();
            fs::write(
                root.join(format!("saves/snapshot-{:016x}-{digest:016x}.bin", 1)),
                payload,
            )
            .unwrap();
        };
        store(&payload);
        let mut args = Args {
            config: root.join("game.toml"),
            output: Some(root.join("site")),
            web_dist: Some(root.join("dist")),
            aot: Aot::Off,
            aot_profile: None,
            wasm_procedures: None,
            speed_tiers: None,
            rom_dir: None,
            offline: true,
            no_saves: false,
            startup_keys: Vec::new(),
        };
        let bar = ProgressBar::hidden();
        let site = build(&args, &bar).unwrap();
        assert!(!site.join("bundle/saves.dsv").exists());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(site.join("bundle/launch.json")).unwrap()).unwrap();
        assert!(manifest.get("saves").is_none());
        let merged = fs::read(site.join("bundle/game.zip")).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(merged)).unwrap();
        for (name, expected) in [
            ("PACK/TEST.COM", &[0xb8, 0, 0x4c, 0xcd, 0x21][..]),
            ("PACK/NEW.DAT", b"rename".as_slice()),
            ("PACK/DATA.DAT", b"new".as_slice()),
            ("PACK/TRUNC.DAT", b"\0\0\0\0\0Z".as_slice()),
            ("OTHER/KEEP.DAT", b"outside".as_slice()),
        ] {
            let mut bytes = Vec::new();
            archive
                .by_name(name)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, expected, "{name}");
        }
        assert!(archive.by_name("PACK/DEL.DAT").is_err());
        assert!(archive.by_name("PACK/OLD.DAT").is_err());
        let mut saved = Vec::new();
        archive
            .by_name("PACK/SAVE.DAT")
            .unwrap()
            .read_to_end(&mut saved)
            .unwrap();
        assert_eq!(saved.len(), 100_000);
        assert!(saved[..50_000].iter().all(|byte| *byte == b'A'));
        assert!(saved[50_000..].iter().all(|byte| *byte == b'B'));
        let merged_profile = fs::read_to_string(site.join("bundle/game.toml")).unwrap();
        // Saves made for other game files are refused unless explicitly omitted.
        let mut foreign = payload.clone();
        foreign[4] ^= 1;
        store(&foreign);
        args.output = Some(root.join("foreign"));
        assert!(
            build(&args, &bar)
                .unwrap_err()
                .to_string()
                .contains("saves do not match")
        );
        args.no_saves = true;
        let site = build(&args, &bar).unwrap();
        assert!(!site.join("bundle/saves.dsv").exists());
        assert_eq!(fs::read(site.join("bundle/game.zip")).unwrap(), zip);
        assert_ne!(
            merged_profile,
            fs::read_to_string(site.join("bundle/game.toml")).unwrap()
        );
        drive.make_directory(b"EMPTY").unwrap();
        let mut body = Vec::with_capacity(drive.snapshot_capacity());
        drive.encode_snapshot(&mut body).unwrap();
        payload.truncate(36);
        payload.extend_from_slice(&body);
        store(&payload);
        args.no_saves = false;
        args.output = Some(root.join("empty-directory"));
        assert!(
            build(&args, &bar)
                .unwrap_err()
                .to_string()
                .contains("empty DOS directories")
        );
    }

    #[test]
    fn builds_a_portable_directory_game_and_never_overwrites_output() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("media")).unwrap();
        fs::write(root.join("media/TEST.COM"), [0xb8, 0, 0x4c, 0xcd, 0x21]).unwrap();
        fs::write(root.join("game.toml"), "version=2\nname='<Test>'\n[game]\nexecutable='TEST.COM'\n[[drives]]\nletter='C'\nkind='directory'\nsource='media'\npolicy='persistent'\nstore='private-host-saves'\n").unwrap();
        fs::create_dir(root.join("dist")).unwrap();
        fs::write(
            root.join("dist/index.html"),
            "<html><head><title>Doslet</title></head><body></body></html>",
        )
        .unwrap();
        let mut args = Args {
            config: root.join("game.toml"),
            output: Some(root.join("site")),
            web_dist: Some(root.join("dist")),
            aot: Aot::Required,
            aot_profile: None,
            wasm_procedures: None,
            speed_tiers: None,
            rom_dir: None,
            offline: true,
            no_saves: false,
            startup_keys: Vec::new(),
        };
        let bar = ProgressBar::hidden();
        assert!(
            build(&args, &bar)
                .unwrap_err()
                .to_string()
                .contains("no supported AOT")
        );
        assert!(!root.join("site").exists());
        assert!(
            !root
                .join(format!(".doslet-web-{}", std::process::id()))
                .exists()
        );
        args.aot = Aot::Auto;
        // Cover art must be an image; it is copied beside the bundle profile.
        let profile = fs::read_to_string(root.join("game.toml")).unwrap();
        let with_cover = profile
            .replace(
                "[game]\n",
                "[info]\ncover='art/cover.png'\nreleased=1994\npublishers='<Pub> & Co'\ndescription=\"\"\"\nFirst <line>\nwraps.\n\nSecond.\"\"\"\n[game]\n",
            )
            .replace(
                "executable='TEST.COM'",
                "executable='TEST.COM'\nstartup_keys='A?'",
            );
        fs::write(root.join("game.toml"), &with_cover).unwrap();
        fs::create_dir(root.join("art")).unwrap();
        fs::write(root.join("art/cover.png"), b"not an image").unwrap();
        assert!(
            build(&args, &bar)
                .unwrap_err()
                .to_string()
                .contains("JPEG, PNG or WebP")
        );
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        fs::write(root.join("art/cover.png"), png).unwrap();
        args.startup_keys = vec!["1".into(), "5".into()];
        let site = build(&args, &bar).unwrap();
        assert_eq!(fs::read(site.join("bundle/cover.png")).unwrap(), png);
        let index = fs::read_to_string(site.join("index.html")).unwrap();
        assert!(index.contains(r#"<img id="doslet-bundle-art" src="bundle/cover.png""#));
        assert!(index.contains(r#"<dt data-i18n="released">Released</dt><dd>1994</dd>"#));
        assert!(index.contains("<title>&lt;Test&gt; (1994) · Doslet</title>"));
        assert!(index.contains(r#"<meta property="og:image" content="bundle/cover.png">"#));
        assert_eq!(index.matches("<title>").count(), 1);
        assert!(index.contains("<dd>&lt;Pub&gt; &amp; Co</dd>"));
        assert!(index.contains(
            r#"<div id="doslet-bundle-about"><p>First &lt;line&gt; wraps.</p><p>Second.</p></div>"#
        ));
        assert!(index.contains(r#"<meta name="description" content="First &lt;line&gt; wraps.">"#));
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(site.join("bundle/launch.json")).unwrap()).unwrap();
        assert_eq!(manifest["cover"], "cover.png");
        assert_eq!(manifest["startupKeys"], "15");
        assert_eq!(manifest["displayFilter"], "nearest");
        let profile = fs::read_to_string(site.join("bundle/game.toml")).unwrap();
        let spec = machine_profile::parse_machine_profile(&profile).unwrap();
        assert_eq!(spec.game.startup_keys, "15");
        assert_eq!(spec.info.cover.as_deref(), Some("cover.png"));
        assert_eq!(spec.info.released, Some(1994));
        assert_eq!(spec.mounts[0].kind, SourceKind::BrowserZip);
        assert_eq!(spec.mounts[0].policy, WritePolicy::Persistent);
        assert!(!profile.contains("private-host-saves"));
        assert!(!profile.contains(root.to_str().unwrap()));
        // Tier pacing has its own asset. A 16-bit-only input still falls back
        // automatically, while --aot required demands actual compiled entries.
        let tiers = json!({"identity":format!("exe-sha256:{}",hash(&[0xb8,0,0x4c,0xcd,0x21])),
            "unknown":"grey","irq_policy":"static-grey","rates":[133,8,33],
            "nodes":[{"pc":0x10100,"bits":16,"bytes":vec![0u8;15],"tier":"slow"}]});
        fs::write(root.join("tiers.json"), serde_json::to_vec(&tiers).unwrap()).unwrap();
        args.speed_tiers = Some(root.join("tiers.json"));
        args.startup_keys.clear();
        args.output = Some(root.join("tier-site"));
        let tier_site = build(&args, &bar).unwrap();
        let tier_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(tier_site.join("bundle/launch.json")).unwrap())
                .unwrap();
        assert_eq!(tier_manifest["startupKeys"], "A?");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(tier_site.join("bundle/launch.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["tierProfile"], "speed-tiers.json");
        assert!(manifest.get("aot").is_none());
        args.aot = Aot::Required;
        args.output = Some(root.join("required-tier-site"));
        assert!(
            build(&args, &bar)
                .unwrap_err()
                .to_string()
                .contains("no supported AOT")
        );
        let mut wide_tiers = tiers.clone();
        wide_tiers["nodes"][0]["bits"] = json!(32);
        wide_tiers["nodes"][0]["bytes"][0] = json!(0x40);
        fs::write(
            root.join("tiers.json"),
            serde_json::to_vec(&wide_tiers).unwrap(),
        )
        .unwrap();
        let combined = build(&args, &bar).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(combined.join("bundle/launch.json")).unwrap())
                .unwrap();
        assert!(manifest["tierProfile"].is_string());
        assert!(manifest["aot"]["module"].is_string());
        assert!(manifest["aot"].get("profile").is_none());
        assert!(!combined.join("bundle/aot-profile.bin").exists());
        // Only explicit requests package register code split into one-instruction pieces.
        args.aot = Aot::Auto;
        args.output = Some(root.join("fragmented-site"));
        let fragmented = build(&args, &bar).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(fragmented.join("bundle/launch.json")).unwrap())
                .unwrap();
        assert!(manifest["tierProfile"].is_string());
        assert!(manifest.get("aot").is_none());
        assert!(!fragmented.join("bundle/aot.wasm").exists());
        args.output = Some(root.join("invalid-tiers-site"));
        let mut invalid = tiers;
        invalid["identity"] = json!("exe-sha256:wrong");
        fs::write(
            root.join("tiers.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert!(
            build(&args, &bar)
                .unwrap_err()
                .to_string()
                .contains("selected executable")
        );
        args.speed_tiers = None;
        args.output = Some(root.join("site"));
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(site.join("bundle/launch.json")).unwrap()).unwrap();
        assert_eq!(manifest["executable"], "TEST.COM");
        assert!(manifest.get("tierProfile").is_none());
        assert!(manifest.get("aot").is_none());
        assert!(
            fs::read_to_string(site.join("index.html"))
                .unwrap()
                .contains("&lt;Test&gt;")
        );
        let bytes = Arc::new(fs::read(site.join("bundle/game.zip")).unwrap());
        let game = archive::GameArchive::new(bytes).unwrap();
        let mut session = game.load_configured("TEST.COM", &spec).unwrap();
        session.start();
        session.advance(100);
        assert!(session.fault().is_none());
        assert!(
            build(&args, &bar)
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        assert_eq!(
            fs::read(root.join("media/TEST.COM")).unwrap(),
            [0xb8, 0, 0x4c, 0xcd, 0x21]
        );
        // Native profiles can omit mounts and name a profile-relative EXE.
        fs::write(
            root.join("game.toml"),
            "version=2\n[game]\nexecutable='media/TEST.COM'\n",
        )
        .unwrap();
        args.output = Some(root.join("implicit-site"));
        let site = build(&args, &bar).unwrap();
        let profile = fs::read_to_string(site.join("bundle/game.toml")).unwrap();
        assert!(profile.contains("TEST.COM"));
        assert!(!profile.contains(root.to_str().unwrap()));
    }
}
