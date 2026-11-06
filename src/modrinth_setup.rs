//! One-button onboarding through Modrinth's supported local mrpack installer.
//! No private database writes and no Minecraft authentication bypass.
use crate::{
    config,
    manifest::{Manifest, Source},
    publish, source, sync, transaction, updater_profiles, util,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
const LEGACY_SHARED_ID: &str = "NKvb031t";
#[cfg(windows)]
#[path = "modrinth_windows.rs"]
mod windows_installer;
pub const MARKER: &str = ".jans-launcher/modrinth-import.json";
pub struct SetupLease(fs::File);
impl SetupLease {
    pub fn acquire(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let file = fs::File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("modrinth-setup.lock"))?;
        file.try_lock().context(
            "Profile setup is already running in another window. Wait for that operation to finish",
        )?;
        Ok(Self(file))
    }
}
impl Drop for SetupLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Serialize, Deserialize)]
struct PendingArchive {
    source: PathBuf,
    loader: String,
    archive: PathBuf,
    size: u64,
    modified: u128,
}
fn archive_stamp(path: &Path) -> Result<(u64, u128)> {
    let m = fs::metadata(path)?;
    Ok((
        m.len(),
        m.modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    ))
}
fn pending_in(output: &Path) -> Option<PendingArchive> {
    let p: PendingArchive =
        serde_json::from_slice(&fs::read(output.join("pending.json")).ok()?).ok()?;
    let output = fs::canonicalize(output).ok()?;
    let archive = fs::canonicalize(&p.archive).ok()?;
    if archive.parent() != Some(output.as_path())
        || archive.extension()?.to_str()? != "mrpack"
        || archive_stamp(&archive).ok()? != (p.size, p.modified)
    {
        return None;
    }
    let mut zip = zip::ZipArchive::new(fs::File::open(&archive).ok()?).ok()?;
    let index: serde_json::Value =
        serde_json::from_reader(zip.by_name("modrinth.index.json").ok()?).ok()?;
    if index["name"] != "Jan's Fabric 26.3" {
        return None;
    }
    Some(p)
}
pub fn pending_archive() -> Option<PathBuf> {
    pending_in(&config::data_dir().join("modrinth-imports")).map(|p| p.archive)
}
pub fn completed_archive(archive: &Path) -> bool {
    let Some(id) = archive.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    updater_profiles::discover().ok().is_some_and(|profiles| {
        profiles.iter().any(|p| {
            marker(&p.directory).is_some_and(|m| m.id == id) && ensure_import_complete(p).is_ok()
        })
    })
}

pub fn setup_progress(
    ui: &mut eframe::egui::Ui,
    stage: &str,
    done: u64,
    total: u64,
    elapsed: std::time::Duration,
) {
    use eframe::egui::{self, Color32, RichText};
    egui::Frame::new()
        .fill(Color32::from_rgb(34, 46, 58))
        .corner_radius(8)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new(if stage.is_empty() {
                        "Starting profile setup…"
                    } else {
                        stage
                    })
                    .strong(),
                );
            });
            ui.label("Please wait. Keep this window open; you do not need to click again.");
            if total > 0 {
                ui.add(
                    egui::ProgressBar::new((done as f32 / total as f32).clamp(0.0, 1.0))
                        .show_percentage()
                        .text(format!(
                            "{} / {}",
                            util::human_bytes(done),
                            util::human_bytes(total)
                        )),
                );
            }
            ui.small(format!(
                "Elapsed: {}m {:02}s",
                elapsed.as_secs() / 60,
                elapsed.as_secs() % 60
            ));
        });
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(150));
}
#[derive(Serialize, Deserialize)]
pub struct Marker {
    pub format: u32,
    pub channel: String,
    pub id: String,
}
pub fn marker(profile: &Path) -> Option<Marker> {
    let path = util::safe_join(profile, MARKER).ok()?;
    let m: Marker = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (m.format == 1
        && m.channel == config::DEFAULT_PACK_SOURCE
        && m.id.starts_with("jans-local-")
        && m.id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
    .then_some(m)
}
pub fn belongs(profile: &updater_profiles::Profile, pack: &Manifest) -> Result<bool> {
    if marker(&profile.directory).is_some() {
        return Ok(true);
    }
    if sync::State::load(&profile.directory).is_some_and(|s| s.pack_name == pack.name) {
        return Ok(true);
    }
    let root = profile
        .directory
        .parent()
        .and_then(Path::parent)
        .context("Missing Modrinth root")?;
    let db = rusqlite::Connection::open_with_flags(
        root.join("app.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let matching:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM instances i JOIN instance_links l ON l.instance_id=i.id WHERE i.path=?1 AND l.shared_instance_id=?2)",rusqlite::params![profile.directory.file_name().unwrap().to_string_lossy().as_ref(),LEGACY_SHARED_ID],|r|r.get(0)).unwrap_or(false);
    if matching {
        return Ok(true);
    }
    // A copied/imported old version may have no link. Match stable mod identities, not its name.
    let expected: HashSet<_> = pack
        .files
        .iter()
        .filter_map(|f| f.mod_id.as_deref())
        .collect();
    if expected.len() < 20 {
        return Ok(false);
    }
    let custom: HashSet<_> = pack
        .files
        .iter()
        .filter(|f| f.source == Some(Source::Github))
        .filter_map(|f| f.mod_id.as_deref())
        .collect();
    let mut actual = HashSet::new();
    let mods = util::safe_join(&profile.directory, "mods")?;
    if !mods.is_dir() {
        return Ok(false);
    }
    for e in fs::read_dir(mods)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "jar") {
            if let Some(info) = publish::fabric_mod_info(&p) {
                actual.insert(info.id);
            }
        }
    }
    let hits = expected.iter().filter(|id| actual.contains(**id)).count();
    let custom_hits = custom.iter().filter(|id| actual.contains(**id)).count();
    Ok(hits * 100 >= expected.len() * 70 && custom_hits >= 2)
}
pub fn matching(pack: &Manifest) -> Result<Vec<updater_profiles::Profile>> {
    updater_profiles::discover()?
        .into_iter()
        .filter_map(|p| match belongs(&p, pack) {
            Ok(true) => Some(Ok(p)),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        })
        .collect()
}
fn ensure_import_complete(profile: &updater_profiles::Profile) -> Result<()> {
    let root = profile
        .directory
        .parent()
        .and_then(Path::parent)
        .context("Missing Modrinth root")?;
    let db = rusqlite::Connection::open_with_flags(
        root.join("app.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let stage: String = db.query_row(
        "SELECT install_stage FROM instances WHERE path=?1",
        [profile
            .directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .as_ref()],
        |r| r.get(0),
    )?;
    ensure!(
        stage == "installed",
        "Your independent profile exists, but Modrinth installation is unfinished. Finish or retry installation in Modrinth; the old profile remains available"
    );
    Ok(())
}
fn inventory(root: &Path) -> Result<BTreeMap<String, (u64, String)>> {
    let progress: sync::ProgressFn = std::sync::Arc::new(|_, _, _| {});
    inventory_progress(root, "Checking profile files", &progress)
}
fn inventory_progress(
    root: &Path,
    stage: &str,
    progress: &sync::ProgressFn,
) -> Result<BTreeMap<String, (u64, String)>> {
    let mut files = BTreeMap::new();
    let mut paths = Vec::new();
    let mut total = 0;
    progress(stage, 0, 0);
    for e in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() != 1 || e.file_name() != ".jans-launcher")
    {
        let e = e?;
        if e.depth() == 0 {
            continue;
        }
        let rel = e
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("A profile filename is not Unicode")?
            .replace('\\', "/");
        let path = util::safe_join(root, &rel)?;
        ensure!(
            !e.file_type().is_symlink(),
            "Linked profile path cannot be copied safely: {}",
            path.display()
        );
        if e.file_type().is_file() {
            let size = e.metadata()?.len();
            total += size;
            paths.push((rel, path, size));
        } else {
            ensure!(
                e.file_type().is_dir(),
                "Unsupported file: {}",
                path.display()
            );
        }
    }
    let mut done = 0;
    let mut buffer = vec![0u8; 1024 * 1024];
    for (relative, path, size) in paths {
        let mut file = fs::File::open(&path)?;
        let mut hash = Sha512::new();
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            done += n as u64;
            progress(stage, done, total);
        }
        files.insert(relative, (size, hex::encode(hash.finalize())));
    }
    Ok(files)
}
/// Pure file export, independently testable with disposable fixtures and no Modrinth UI.
pub fn export(
    profile: &Path,
    loader: &str,
    output: &Path,
    progress: sync::ProgressFn,
) -> Result<PathBuf> {
    let root = fs::canonicalize(profile)?;
    fs::create_dir_all(output)?;
    let output = fs::canonicalize(output)?;
    ensure!(
        !output.starts_with(&root),
        "Migration archives must be outside the source profile"
    );
    let _export = SetupLease::acquire(&output)?;
    let _lock = transaction::InstanceLock::acquire(&root)?;
    transaction::ensure_game_stopped(&root)?;
    if let Some(pending) = pending_in(&output) {
        if pending.source == root && pending.loader == loader {
            progress(
                "Your copy is already prepared. Reopening the same Modrinth installer…",
                0,
                0,
            );
            return Ok(pending.archive);
        }
    }
    ensure!(
        !root
            .join(".jans-launcher/transaction/journal.json")
            .exists(),
        "Finish the source profile's interrupted update before copying it"
    );
    let files = inventory_progress(
        &root,
        "Checking your existing mods, settings and worlds",
        &progress,
    )?;
    let total = files.values().map(|x| x.0).sum();
    let id = format!(
        "jans-local-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let path = output.join(format!("{id}.mrpack"));
    let partial = output.join(format!("{id}.partial"));
    let result = (|| -> Result<()> {
        let file = fs::File::options()
            .create_new(true)
            .write(true)
            .open(&partial)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        zip.start_file("modrinth.index.json", options)?;
        zip.write_all(&serde_json::to_vec_pretty(&serde_json::json!({"formatVersion":1,"game":"minecraft","versionId":id,"name":"Jan's Fabric 26.3","summary":"Independent local copy. Pack updates come from Jan's updater; old profile remains intact.","files":[],"dependencies":{"minecraft":config::PACK_MINECRAFT,"fabric-loader":loader}}))?)?;
        let mut done = 0;
        let mut buffer = vec![0u8; 128 * 1024];
        for (relative, (size, hash)) in &files {
            zip.start_file(format!("overrides/{relative}"), options)?;
            let mut source = fs::File::open(util::safe_join(&root, relative)?)?;
            let mut digest = Sha512::new();
            let mut copied = 0;
            loop {
                let n = source.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                zip.write_all(&buffer[..n])?;
                digest.update(&buffer[..n]);
                copied += n as u64;
                done += n as u64;
                progress("Copying your mods, settings and worlds", done, total);
            }
            ensure!(
                copied == *size && hex::encode(digest.finalize()) == *hash,
                "Source changed while copying {relative}; close Minecraft and retry"
            );
        }
        zip.start_file(format!("overrides/{MARKER}"), options)?;
        zip.write_all(&serde_json::to_vec(&Marker {
            format: 1,
            channel: config::DEFAULT_PACK_SOURCE.into(),
            id: id.clone(),
        })?)?;
        progress("Saving the prepared copy to disk", 0, 0);
        zip.finish()?.sync_all()?;
        ensure!(
            inventory_progress(
                &root,
                "Verifying that your original files are unchanged",
                &progress
            )? == files,
            "The old profile changed during export; retry after closing Minecraft"
        );
        transaction::ensure_game_stopped(&root)?;
        fs::rename(&partial, &path)?;
        let (size, modified) = archive_stamp(&path)?;
        util::write_atomic(
            &output.join("pending.json"),
            &serde_json::to_vec(&PendingArchive {
                source: root.clone(),
                loader: loader.into(),
                archive: path.clone(),
                size,
                modified,
            })?,
        )?;
        progress("Copy ready. Opening Modrinth—confirm Install there", 0, 0);
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&partial);
        return Err(e.context("No new profile installed; original files preserved"));
    }
    Ok(path)
}

/// Populate the independent app instance before exporting a first-time Modrinth install.
async fn populate_new_copy(
    client: &reqwest::Client,
    own: PathBuf,
    fetched: source::Fetched,
    progress: sync::ProgressFn,
) -> Result<(PathBuf, String)> {
    fs::create_dir_all(&own)?;
    let loader = fetched.manifest.fabric_loader.clone();
    let target = own.clone();
    let changes = tokio::task::spawn_blocking(move || sync::plan(&target, fetched)).await??;
    sync::apply(client, &own, &changes, progress).await?;
    Ok((own, loader))
}

/// An existing Modrinth profile is optional: download the pack when there is nothing to copy.
pub async fn prepare(
    client: &reqwest::Client,
    explicit: Option<PathBuf>,
    progress: sync::ProgressFn,
) -> Result<Option<PathBuf>> {
    let _setup = SetupLease::acquire(&config::data_dir())?;
    progress("Checking the published pack version", 0, 0);
    let fetched = source::PackSource::parse(config::DEFAULT_PACK_SOURCE)?
        .fetch(client)
        .await?;
    let (source, loader) = if let Some(path) = explicit {
        let p = updater_profiles::from_directory(&path)?;
        ensure!(
            belongs(&p, &fetched.manifest)?,
            "This profile does not match Jan's modpack. Select the old Jan's pack profile"
        );
        if marker(&p.directory).is_some() {
            ensure_import_complete(&p)?;
            return Ok(None);
        }
        (p.directory, p.loader_version)
    } else {
        progress("Looking for your existing Jan's pack", 0, 0);
        let pack = fetched.manifest.clone();
        let matches = tokio::task::spawn_blocking(move || matching(&pack)).await??;
        if let Some(existing) = matches.iter().find(|p| marker(&p.directory).is_some()) {
            ensure_import_complete(existing)?;
            return Ok(None);
        }
        let selected = if matches.len() == 1 {
            matches.into_iter().next()
        } else if matches.len() > 1 {
            let root = matches[0].directory.parent().unwrap().to_path_buf();
            let chosen=tokio::task::spawn_blocking(move||rfd::FileDialog::new().set_title("Multiple Jan's pack profiles found: choose the one with your settings and worlds").set_directory(root).pick_folder()).await?.context("Profile creation cancelled")?;
            Some(
                matches
                    .into_iter()
                    .find(|p| p.directory == fs::canonicalize(&chosen).unwrap_or(chosen.clone()))
                    .context("Choose one of the detected Jan's pack profiles")?,
            )
        } else {
            progress("No existing copy found. Preparing the pack download", 0, 0);
            None
        };
        if let Some(p) = selected {
            (p.directory, p.loader_version)
        } else {
            let own = config::Settings::load().single_pack().instance_dir();
            populate_new_copy(client, own, fetched, progress.clone()).await?
        }
    };
    let output = config::data_dir().join("modrinth-imports");
    tokio::task::spawn_blocking(move || export(&source, &loader, &output, progress).map(Some))
        .await?
}

pub fn open_installer(archive: Option<&Path>) -> Result<()> {
    if archive.is_some_and(completed_archive) {
        return open::that("modrinth://").context("Cannot open the installed Modrinth profile");
    }
    let Some(archive) = archive else {
        return open::that("modrinth://").context("Cannot open Modrinth");
    };
    #[cfg(windows)]
    {
        // Use Modrinth's registered executable directly: do not accidentally import into another launcher.
        let exe = windows_installer::executable().with_context(|| format!(
            "Install and open Modrinth App first, then click Continue installation. Your prepared pack is safe at {}",
            archive.display()
        ))?;
        ensure!(
            exe.is_file(),
            "Modrinth is missing. Local archive: {}",
            archive.display()
        );
        std::process::Command::new(exe).arg(archive).spawn()
            .context("Cannot start Modrinth with the prepared pack")?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        open::that(archive).context("Open this local mrpack file using Modrinth App")
    }
}

pub fn show_prepared_pack(archive: &Path) -> Result<()> {
    ensure!(archive.is_file(), "The prepared pack is missing; create it again");
    let folder = archive.parent().context("Prepared pack has no folder")?;
    open::that(folder).context("Cannot open the prepared pack folder")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[tokio::test]
    async fn fresh_install_with_empty_modrinth_and_unicode_windows_paths() {
        let root = std::env::temp_dir().join(format!(
            "jans-fresh-Božac-šđčćž-{}-{}",
            std::process::id(), time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let app = root.join("Empty Modrinth");
        fs::create_dir_all(&app).unwrap();
        let db = rusqlite::Connection::open(app.join("app.db")).unwrap();
        db.execute_batch("CREATE TABLE instances(name TEXT,path TEXT,applied_content_set_id TEXT);CREATE TABLE instance_content_sets(id TEXT,game_version TEXT,loader TEXT,loader_version TEXT);").unwrap();
        drop(db);
        let db_hash = util::sha512_file(&app.join("app.db")).unwrap();
        assert!(updater_profiles::from_app_root(&app).unwrap().is_empty());
        let source = root.join("Published pack");
        fs::create_dir_all(&source).unwrap();
        let mut jar = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        jar.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default()).unwrap();
        jar.write_all(br#"{"schemaVersion":1,"id":"fresh_fixture","version":"1.0"}"#).unwrap();
        let content = jar.finish().unwrap().into_inner();
        fs::write(source.join("mod.jar"), &content).unwrap();
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "format":1,"name":"Jan's Fabric 26.3","build":1,"version":"fixture",
            "minecraft":"26.3","fabric_loader":"0.20.1","files":[{
                "path":"mods/fixture.jar","url":"mod.jar","size":content.len(),
                "sha512":hex::encode(Sha512::digest(&content)),"policy":"managed"
            }]
        })).unwrap();
        manifest.validate().unwrap();
        let progress: sync::ProgressFn = Arc::new(|_,_,_|{});
        let (own, loader) = populate_new_copy(&util::http(), root.join("App instance"),
            source::Fetched { manifest, base: source.to_string_lossy().into_owned(), revision:"fixture".into() },
            progress.clone()).await.unwrap();
        let archive = export(&own, &loader, &root.join("Prepared packs"), progress).unwrap();
        let mut zip = zip::ZipArchive::new(fs::File::open(&archive).unwrap()).unwrap();
        let index: serde_json::Value = serde_json::from_reader(zip.by_name("modrinth.index.json").unwrap()).unwrap();
        assert_eq!(index["name"], "Jan's Fabric 26.3");
        assert_eq!(index["dependencies"]["minecraft"], "26.3");
        assert_eq!(index["dependencies"]["fabric-loader"], "0.20.1");
        assert_eq!(index["files"], serde_json::json!([]));
        let mut installed = vec![];
        zip.by_name("overrides/mods/fixture.jar").unwrap().read_to_end(&mut installed).unwrap();
        assert_eq!(installed, content);
        assert!(zip.by_name(&format!("overrides/{MARKER}")).is_ok());
        assert_eq!(pending_in(&root.join("Prepared packs")).unwrap().archive, archive);
        assert_eq!(util::sha512_file(&app.join("app.db")).unwrap(), db_hash);
        assert!(updater_profiles::from_app_root(&app).unwrap().is_empty(), "Modrinth alone registers the import after confirmation");
        drop(zip);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn only_one_setup_window_can_prepare_a_profile() {
        let root = std::env::temp_dir().join(format!(
            "jans-setup-lock-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let lock = SetupLease::acquire(&root).unwrap();
        assert!(SetupLease::acquire(&root).is_err());
        drop(lock);
        drop(SetupLease::acquire(&root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn migration_archive_preserves_worlds_settings_and_mods_without_shared_metadata() {
        let root = std::env::temp_dir().join(format!(
            "jans-native-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let source = root.join("old");
        fs::create_dir_all(source.join("saves/My world")).unwrap();
        fs::create_dir_all(source.join("config")).unwrap();
        fs::create_dir_all(source.join("mods")).unwrap();
        fs::write(source.join("options.txt"), b"user settings").unwrap();
        fs::write(source.join("config/custom.json"), b"user config").unwrap();
        fs::write(source.join("saves/My world/level.dat"), b"world").unwrap();
        fs::write(source.join("mods/own.jar"), b"mod").unwrap();
        fs::create_dir_all(source.join(".jans-launcher")).unwrap();
        fs::write(
            source.join(".jans-launcher/state.json"),
            b"old update metadata",
        )
        .unwrap();
        let before = inventory(&source).unwrap();
        let stages = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorded = stages.clone();
        let path = export(
            &source,
            "0.20.1",
            &root.join("archives"),
            Arc::new(move |phase, _, _| {
                let mut phases = recorded.lock().unwrap();
                if phases.last().map(String::as_str) != Some(phase) {
                    phases.push(phase.into());
                }
            }),
        )
        .unwrap();
        let second = export(
            &source,
            "0.20.1",
            &root.join("archives"),
            Arc::new(|_, _, _| {}),
        )
        .unwrap();
        assert_eq!(
            path, second,
            "a second click must reuse the prepared archive"
        );
        assert_eq!(
            fs::read_dir(root.join("archives"))
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "mrpack"))
                .count(),
            1
        );
        let phases = stages.lock().unwrap();
        assert!(phases.iter().any(|s| s.starts_with("Checking")));
        assert!(phases.iter().any(|s| s.starts_with("Copying")));
        assert!(phases.iter().any(|s| s.starts_with("Verifying")));
        drop(phases);
        let mut zip = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
        let index: serde_json::Value =
            serde_json::from_reader(zip.by_name("modrinth.index.json").unwrap()).unwrap();
        assert_eq!(index["dependencies"]["fabric-loader"], "0.20.1");
        assert_eq!(index["files"], serde_json::json!([]));
        assert!(zip.by_name("overrides/.jans-launcher/state.json").is_err());
        let imported = root.join("imported");
        fs::create_dir_all(&imported).unwrap();
        for (relative, (_, hash)) in &before {
            let mut data = vec![];
            zip.by_name(&format!("overrides/{relative}"))
                .unwrap()
                .read_to_end(&mut data)
                .unwrap();
            assert_eq!(hex::encode(Sha512::digest(&data)), *hash);
            let dest = util::safe_join(&imported, relative).unwrap();
            fs::create_dir_all(dest.parent().unwrap()).unwrap();
            fs::write(dest, data).unwrap();
        }
        let mut data = vec![];
        zip.by_name(&format!("overrides/{MARKER}"))
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        fs::create_dir_all(imported.join(".jans-launcher")).unwrap();
        fs::write(imported.join(MARKER), data).unwrap();
        assert!(marker(&imported).is_some());
        assert_eq!(inventory(&source).unwrap(), before);
        assert_eq!(inventory(&imported).unwrap(), before);
        drop(zip);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pack_detection_uses_identity_instead_of_the_profile_name() {
        let root = std::env::temp_dir().join(format!(
            "jans-detect-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let directory = root.join("profiles/Looks identical");
        fs::create_dir_all(&directory).unwrap();
        let db = rusqlite::Connection::open(root.join("app.db")).unwrap();
        db.execute_batch("CREATE TABLE instances(id TEXT,path TEXT);CREATE TABLE instance_links(instance_id TEXT,shared_instance_id TEXT);INSERT INTO instances VALUES('one','Looks identical');INSERT INTO instance_links VALUES('one','other-pack');").unwrap();
        let profile = updater_profiles::Profile {
            name: "Fabric 26.3".into(),
            directory,
            game_version: "26.3".into(),
            loader: "fabric".into(),
            loader_version: "0.20.1".into(),
        };
        let pack:Manifest=serde_json::from_value(serde_json::json!({"format":1,"name":"Jan's Fabric 26.3","build":1,"version":"test","minecraft":"26.3","fabric_loader":"0.20.1","files":[]})).unwrap();
        assert!(!belongs(&profile, &pack).unwrap());
        db.execute(
            "UPDATE instance_links SET shared_instance_id=?1",
            [LEGACY_SHARED_ID],
        )
        .unwrap();
        assert!(belongs(&profile, &pack).unwrap());
        db.execute(
            "ALTER TABLE instances ADD COLUMN install_stage TEXT DEFAULT 'installing'",
            [],
        )
        .unwrap();
        assert!(ensure_import_complete(&profile).is_err());
        db.execute("UPDATE instances SET install_stage='installed'", [])
            .unwrap();
        ensure_import_complete(&profile).unwrap();
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
}
