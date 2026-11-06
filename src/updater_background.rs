//! User-owned pack watcher. No game or browser window, account credentials or elevated service.
use crate::{config, source, sync, transaction, updater_profiles, util};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const INTERVAL: Duration = Duration::from_secs(300);
pub static CHECK_NOW: AtomicBool = AtomicBool::new(false);
pub static EXIT: AtomicBool = AtomicBool::new(false);
static TRAY_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Enrollment {
    pub enabled: bool,
    pub profile: PathBuf,
    pub instance_id: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub phase: String,
    pub message: String,
    pub last_check: Option<String>,
    pub last_update: Option<String>,
    pub revision: Option<String>,
}
fn enrollment_path() -> PathBuf {
    config::data_dir().join("background-profile.json")
}
pub fn status_path() -> PathBuf {
    config::data_dir().join("background-status.json")
}
pub fn enrollment() -> Result<Option<Enrollment>> {
    match fs::read(enrollment_path()) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("Background settings are damaged")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn status() -> Option<Status> {
    serde_json::from_slice(&fs::read(status_path()).ok()?).ok()
}
fn now() -> String {
    time::OffsetDateTime::now_utc().to_string()
}
fn save_status(state: &Status) -> Result<()> {
    util::write_atomic(&status_path(), &serde_json::to_vec_pretty(state)?)
}
pub fn request_exit() -> Result<()> {
    util::write_atomic(&config::data_dir().join("background-exit"), b"exit")
}

/// Wait for the worker to finish its current transaction, never terminate it mid-update.
pub async fn stop_for_app_update() -> Result<bool> {
    stop_worker_in(&config::data_dir(), Duration::from_secs(120)).await
}

async fn stop_worker_in(directory: &Path, timeout: Duration) -> Result<bool> {
    fs::create_dir_all(directory)?;
    let lock = fs::File::options().create(true).truncate(false).read(true).write(true)
        .open(directory.join("background.lock"))?;
    if lock.try_lock().is_ok() {
        return Ok(false);
    }
    let exit = directory.join("background-exit");
    util::write_atomic(&exit, b"exit")?;
    let deadline = Instant::now() + timeout;
    loop {
        if lock.try_lock().is_ok() {
            match fs::remove_file(&exit) {
                Ok(()) => {},
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(e) => return Err(e.into()),
            }
            return Ok(true);
        }
        ensure!(Instant::now() < deadline,
            "The background pack update is still finishing. No files were interrupted. Retry the app update after it finishes");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
pub fn start_resident() -> Result<()> {
    #[cfg(windows)]
    {
        let exe = installed_exe()?;
        fs::create_dir_all(exe.parent().unwrap())?;
        let current = std::env::current_exe()?;
        if fs::canonicalize(&exe).ok() != Some(fs::canonicalize(&current)?) {
            if !exe.exists() {
                fs::copy(&current, &exe)?;
            }
        }
        if enrollment()?.is_none_or(|e| e.enabled) {
            startup(true, &exe)?;
        }
        // Versioned filenames avoid overwriting a running executable.
        spawn_worker(&exe)?;
    }
    Ok(())
}
/// Handle the window X without throwing away an active setup/download.
pub fn handle_close(
    ui: &mut eframe::egui::Ui,
    busy: bool,
    hiding: &mut bool,
    resident: &mut bool,
    error: &mut Option<String>,
) {
    #[cfg(windows)]
    {
        use eframe::egui::ViewportCommand;
        if ui.ctx().input(|i| i.viewport().close_requested()) && !*hiding {
            if !*resident {
                match start_resident() {
                    Ok(()) => *resident = true,
                    Err(e) => {
                        *error = Some(format!("Could not start the tray updater: {e:#}"));
                        ui.ctx().send_viewport_cmd(ViewportCommand::CancelClose);
                        return;
                    }
                }
            }
            if busy {
                *hiding = true;
                ui.ctx().send_viewport_cmd(ViewportCommand::CancelClose);
                ui.ctx().send_viewport_cmd(ViewportCommand::Visible(false));
            }
        }
        if *hiding && !busy {
            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
        }
    }
}

/// A path alone is insufficient: Modrinth may replace an instance at the same path.
pub fn identity(path: &Path) -> Result<String> {
    let profile = updater_profiles::from_directory(path)?;
    let root = profile
        .directory
        .parent()
        .and_then(Path::parent)
        .context("Missing Modrinth root")?;
    let db = rusqlite::Connection::open_with_flags(
        root.join("app.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    db.busy_timeout(Duration::from_secs(2))?;
    let relative = profile
        .directory
        .file_name()
        .context("Missing profile name")?
        .to_string_lossy();
    let (id, kind): (String, String) = db.query_row(
        "SELECT i.id,c.source_kind FROM instances i JOIN instance_content_sets c ON c.id=i.applied_content_set_id WHERE i.path=?1",
        [relative.as_ref()], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("Cannot verify independent profile identity")?;
    if kind == "imported_modpack" {
        let marker=crate::modrinth_setup::marker(&profile.directory).context("This imported profile was not created by Jan's launcher. Use Create Modrinth profile first")?;
        let expected_filename = format!("{}.mrpack", marker.id);
        let independent:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM instance_links l JOIN instances i ON i.id=l.instance_id WHERE l.instance_id=?1 AND l.link_kind='imported_modpack' AND l.imported_filename=?2 AND l.shared_instance_id IS NULL AND l.modrinth_project_id IS NULL AND l.content_project_id IS NULL AND l.server_project_id IS NULL AND l.hosting_server_id IS NULL AND i.install_stage='installed')",rusqlite::params![id,expected_filename],|r|r.get(0)).context("Cannot verify local migration profile")?;
        ensure!(
            independent,
            "Wait for Modrinth installation to finish. This copy must have no shared/project update connection"
        );
        let remote_refs:i64=db.query_row("SELECT COUNT(*) FROM instance_content_set_remote_refs r JOIN instances i ON i.applied_content_set_id=r.content_set_id WHERE i.id=?1",[&id],|r|r.get(0))?;
        ensure!(
            remote_refs == 0,
            "The migrated profile has a remote update association; automatic updates refused"
        );
        return Ok(id);
    }
    ensure!(
        kind == "local",
        "Automatic updates need the new local profile, not the old shared/imported modpack. Use Create Modrinth profile first."
    );
    let links: i64 = db.query_row("SELECT COUNT(*) FROM instance_links WHERE instance_id=?1 AND (link_kind IS NULL OR link_kind != 'unmanaged')", [&id], |r|r.get(0))
        .context("Cannot verify that this profile has no remote update association")?;
    ensure!(
        links == 0,
        "This profile is linked to another updater. Choose the independent local copy."
    );
    Ok(id)
}

pub fn validate(e: &Enrollment) -> Result<()> {
    ensure!(
        identity(&e.profile)? == e.instance_id,
        "The enrolled Modrinth profile was replaced. Select and enable the intended profile again."
    );
    Ok(())
}

pub fn enable(path: &Path) -> Result<()> {
    #[cfg(not(windows))]
    anyhow::bail!("Login startup is currently supported on Windows only");
    #[cfg(windows)]
    {
        let e = Enrollment {
            enabled: true,
            instance_id: identity(path)?,
            profile: fs::canonicalize(path)?,
        };
        let exe = installed_exe()?;
        fs::create_dir_all(exe.parent().unwrap())?;
        let current = std::env::current_exe()?;
        if fs::canonicalize(&exe).ok() != Some(fs::canonicalize(&current)?) {
            // An in-use installed EXE is not overwritten. Stop/replace it via a future app update.
            ensure!(
                !exe.exists() || util::sha512_file(&exe)? == util::sha512_file(&current)?,
                "A different updater is installed. Disable and exit its tray icon before replacing the installed updater."
            );
            if !exe.exists() {
                fs::copy(&current, &exe)?;
            }
        }
        startup(true, &exe)?;
        if let Err(err) = util::write_atomic(&enrollment_path(), &serde_json::to_vec(&e)?) {
            let _ = startup(false, &exe);
            return Err(err);
        }
        spawn_worker(&exe)?;
        Ok(())
    }
}
pub fn disable() -> Result<()> {
    if let Some(mut e) = enrollment()? {
        e.enabled = false;
        util::write_atomic(&enrollment_path(), &serde_json::to_vec(&e)?)?;
    }
    #[cfg(windows)]
    startup(false, &installed_exe()?)?;
    Ok(())
}
#[cfg(windows)]
fn installed_exe() -> Result<PathBuf> {
    let root = directories::BaseDirs::new()
        .context("Missing user data folder")?
        .data_local_dir().to_path_buf();
    Ok(worker_executable(&root, env!("CARGO_PKG_VERSION"), env!("CARGO_BIN_NAME")))
}

fn worker_executable(root: &Path, version: &str, product: &str) -> PathBuf {
    // The launcher's resident copy must never occupy the updater's executable path.
    root.join("Jan's Modpack Updater").join(version).join(format!("{product}.exe"))
}
#[cfg(windows)]
fn spawn_worker(exe: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(exe)
        .arg("--background")
        .creation_flags(0x08000000)
        .spawn()?;
    Ok(())
}
#[cfg(windows)]
fn startup(enable: bool, exe: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("reg.exe");
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    if enable {
        cmd.args([
            "add",
            key,
            "/v",
            "JansModpackUpdater",
            "/t",
            "REG_SZ",
            "/d",
            &format!("\"{}\" --background", exe.display()),
            "/f",
        ]);
    } else {
        // A missing entry already means startup is disabled; do not hide other errors.
        let query = std::process::Command::new("reg.exe")
            .args(["query", key, "/v", "JansModpackUpdater"])
            .creation_flags(0x08000000)
            .output()?;
        if !query.status.success() {
            return Ok(());
        }
        cmd.args(["delete", key, "/v", "JansModpackUpdater", "/f"]);
    }
    let output = cmd.creation_flags(0x08000000).output()?;
    ensure!(
        output.status.success(),
        "Cannot change login startup: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// One cycle is separately testable without installing a startup entry or touching real profiles.
pub async fn cycle(
    e: &Enrollment,
    client: &reqwest::Client,
    pack: &source::PackSource,
    state: &mut Status,
) -> Result<()> {
    validate(e)?;
    if let Err(error) = transaction::ensure_game_stopped(&e.profile) {
        state.phase = "waiting".into();
        state.message = format!("Waiting for Minecraft to close: {error}");
        return Ok(());
    }
    state.last_check = Some(now());
    let fetched = pack.fetch(client).await?;
    updater_profiles::from_directory(&e.profile)?.compatible(&fetched.manifest)?;
    // Avoid scanning 700MB every five minutes. A pending transaction always gets recovered.
    if fetched.revision != "local"
        && sync::State::load(&e.profile).is_some_and(|s| s.revision == fetched.revision)
        && !e
            .profile
            .join(".jans-launcher/transaction/journal.json")
            .exists()
    {
        state.phase = "current".into();
        state.message = "Your pack is up to date".into();
        state.revision = Some(fetched.revision);
        return Ok(());
    }
    let dir = e.profile.clone();
    let changes = tokio::task::spawn_blocking(move || sync::plan(&dir, fetched)).await??;
    if !changes.up_to_date() {
        validate(e)?;
        crate::apply(client, &e.profile, &changes, Arc::new(|_, _, _| {})).await?;
        state.last_update = Some(now());
    }
    state.phase = "current".into();
    state.message = format!("Ready to play: {}", changes.new_version);
    state.revision = Some(changes.revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_and_updater_cannot_replace_each_others_resident_copy() {
        let root = Path::new("user data with spaces");
        let launcher = worker_executable(root, "0.4.4", "jans-launcher");
        let updater = worker_executable(root, "0.4.4", "jans-modpack-updater");
        assert_ne!(launcher, updater);
        assert_eq!(launcher.file_name().unwrap(), "jans-launcher.exe");
        assert_eq!(updater.file_name().unwrap(), "jans-modpack-updater.exe");
        assert_ne!(updater, worker_executable(root, "0.4.3", "jans-modpack-updater"));
    }
    #[tokio::test]
    async fn app_update_waits_for_worker_transaction_without_killing_it() {
        let root = std::env::temp_dir().join(format!("jans-app-worker-stop-{}-{}",
            std::process::id(), time::OffsetDateTime::now_utc().unix_timestamp_nanos()));
        fs::create_dir_all(&root).unwrap();
        assert!(!stop_worker_in(&root, Duration::from_secs(1)).await.unwrap());
        let lock = fs::File::options().read(true).write(true).open(root.join("background.lock")).unwrap();
        lock.try_lock().unwrap();
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker_root.join("background-exit").exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            fs::write(worker_root.join("transaction-finished"), b"complete").unwrap();
            drop(lock);
        });
        assert!(stop_worker_in(&root, Duration::from_secs(5)).await.unwrap());
        worker.join().unwrap();
        assert_eq!(fs::read(root.join("transaction-finished")).unwrap(), b"complete");
        assert!(!root.join("background-exit").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn app_update_timeout_preserves_active_worker_and_data() {
        let root = std::env::temp_dir().join(format!("jans-app-worker-timeout-{}-{}",
            std::process::id(), time::OffsetDateTime::now_utc().unix_timestamp_nanos()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("profile-data"), b"keep").unwrap();
        let lock = fs::File::options().create(true).truncate(false).read(true).write(true).open(root.join("background.lock")).unwrap();
        lock.try_lock().unwrap();
        assert!(stop_worker_in(&root, Duration::from_millis(10)).await.is_err());
        assert_eq!(fs::read(root.join("profile-data")).unwrap(), b"keep");
        assert!(root.join("background-exit").exists());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
    struct Fixture {
        root: PathBuf,
        profile: PathBuf,
        source: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "jans-watcher-{}-{}",
                std::process::id(),
                time::OffsetDateTime::now_utc().unix_timestamp_nanos()
            ));
            let profile = root.join("profiles/Independent");
            let source = root.join("published");
            fs::create_dir_all(profile.join("saves/Personal")).unwrap();
            fs::create_dir_all(profile.join("config")).unwrap();
            fs::create_dir_all(&source).unwrap();
            fs::write(profile.join("saves/Personal/level.dat"), b"world").unwrap();
            fs::write(profile.join("config/custom.json"), b"player config").unwrap();
            let db = rusqlite::Connection::open(root.join("app.db")).unwrap();
            db.execute_batch("CREATE TABLE instances(id TEXT,name TEXT,path TEXT,applied_content_set_id TEXT);CREATE TABLE instance_content_sets(id TEXT,source_kind TEXT,game_version TEXT,loader TEXT,loader_version TEXT);CREATE TABLE instance_links(instance_id TEXT,link_kind TEXT);INSERT INTO instances VALUES('local:one','Independent','Independent','set');INSERT INTO instance_content_sets VALUES('set','local','26.3','fabric','0.20.1');").unwrap();
            Self {
                root,
                profile,
                source,
            }
        }
        fn enrolled(&self) -> Enrollment {
            Enrollment {
                enabled: true,
                profile: self.profile.clone(),
                instance_id: "local:one".into(),
            }
        }
        fn db(&self) -> rusqlite::Connection {
            rusqlite::Connection::open(self.root.join("app.db")).unwrap()
        }
        fn publish(&self, missing: bool) {
            fs::write(self.source.join("mod.jar"), b"fixture mod").unwrap();
            fs::write(self.source.join("config.json"), b"default config").unwrap();
            fs::write(self.source.join("pack.json"),serde_json::to_vec(&serde_json::json!({"format":1,"name":"fixture","version":"one","build":1,"minecraft":"26.3","fabric_loader":"0.20.1","files":[{"path":"mods/mod.jar","url":if missing{"missing.jar"}else{"mod.jar"},"size":11,"sha512":util::sha512_file(&self.source.join("mod.jar")).unwrap()},{"path":"config/custom.json","url":"config.json","size":14,"sha512":util::sha512_file(&self.source.join("config.json")).unwrap(),"policy":"once"}]})).unwrap()).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn accepts_only_our_completed_native_import_without_remote_links() {
        let f = Fixture::new();
        f.db().execute_batch("ALTER TABLE instances ADD COLUMN install_stage TEXT DEFAULT 'installed';ALTER TABLE instance_links ADD COLUMN imported_filename TEXT;ALTER TABLE instance_links ADD COLUMN shared_instance_id TEXT;ALTER TABLE instance_links ADD COLUMN modrinth_project_id TEXT;ALTER TABLE instance_links ADD COLUMN content_project_id TEXT;ALTER TABLE instance_links ADD COLUMN server_project_id TEXT;ALTER TABLE instance_links ADD COLUMN hosting_server_id TEXT;CREATE TABLE instance_content_set_remote_refs(content_set_id TEXT);UPDATE instance_content_sets SET source_kind='imported_modpack';INSERT INTO instance_links(instance_id,link_kind,imported_filename) VALUES('local:one','imported_modpack','jans-local-fixture.mrpack');").unwrap();
        fs::create_dir_all(f.profile.join(".jans-launcher")).unwrap();
        fs::write(
            f.profile.join(crate::modrinth_setup::MARKER),
            serde_json::to_vec(&crate::modrinth_setup::Marker {
                format: 1,
                channel: config::DEFAULT_PACK_SOURCE.into(),
                id: "jans-local-fixture".into(),
            })
            .unwrap(),
        )
        .unwrap();
        validate(&f.enrolled()).unwrap();
        f.db()
            .execute("UPDATE instances SET install_stage='not_installed'", [])
            .unwrap();
        assert!(validate(&f.enrolled()).is_err());
        f.db()
            .execute("UPDATE instances SET install_stage='installed'", [])
            .unwrap();
        f.db()
            .execute(
                "UPDATE instance_links SET shared_instance_id='old-link'",
                [],
            )
            .unwrap();
        assert!(validate(&f.enrolled()).is_err());
        f.db()
            .execute("UPDATE instance_links SET shared_instance_id=NULL", [])
            .unwrap();
        f.db()
            .execute(
                "INSERT INTO instance_content_set_remote_refs VALUES('set')",
                [],
            )
            .unwrap();
        assert!(validate(&f.enrolled()).is_err());
    }
    #[test]
    fn rejects_shared_linked_and_replaced_targets() {
        let f = Fixture::new();
        validate(&f.enrolled()).unwrap();
        f.db()
            .execute(
                "UPDATE instance_content_sets SET source_kind='shared_instance'",
                [],
            )
            .unwrap();
        assert!(
            validate(&f.enrolled())
                .unwrap_err()
                .to_string()
                .contains("local profile")
        );
        f.db()
            .execute("UPDATE instance_content_sets SET source_kind='local'", [])
            .unwrap();
        f.db()
            .execute(
                "INSERT INTO instance_links VALUES('local:one','shared_instance')",
                [],
            )
            .unwrap();
        assert!(validate(&f.enrolled()).is_err());
        f.db().execute("DELETE FROM instance_links", []).unwrap();
        f.db()
            .execute("UPDATE instances SET id='local:replaced'", [])
            .unwrap();
        assert!(
            validate(&f.enrolled())
                .unwrap_err()
                .to_string()
                .contains("replaced")
        );
    }
    #[test]
    fn automatic_cycle_updates_only_managed_files_and_retries_failed_download() {
        let f = Fixture::new();
        f.publish(true);
        let before = util::sha512_file(&f.root.join("app.db")).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pack = source::PackSource::Base(f.source.display().to_string());
        let client = util::http();
        let mut status = Status::default();
        assert!(
            rt.block_on(cycle(&f.enrolled(), &client, &pack, &mut status))
                .is_err()
        );
        assert!(!f.profile.join("mods/mod.jar").exists());
        assert!(status.last_update.is_none());
        f.publish(false);
        rt.block_on(cycle(&f.enrolled(), &client, &pack, &mut status))
            .unwrap();
        assert_eq!(status.phase, "current");
        assert!(status.last_update.is_some());
        assert_eq!(
            fs::read(f.profile.join("mods/mod.jar")).unwrap(),
            b"fixture mod"
        );
        assert_eq!(
            fs::read(f.profile.join("config/custom.json")).unwrap(),
            b"player config"
        );
        assert_eq!(
            fs::read(f.profile.join("saves/Personal/level.dat")).unwrap(),
            b"world"
        );
        assert_eq!(util::sha512_file(&f.root.join("app.db")).unwrap(), before);
    }
}

pub fn run() -> Result<()> {
    fs::create_dir_all(config::data_dir())?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(config::data_dir().join("background.lock"))?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    #[cfg(windows)]
    std::thread::spawn(|| {
        if let Err(err) = crate::updater_tray::run() {
            *TRAY_ERROR.lock().unwrap() = Some(format!("Tray unavailable: {err:#}"));
            EXIT.store(true, Ordering::Release);
        }
    });
    let rt = tokio::runtime::Runtime::new()?;
    let client = util::http();
    let pack = source::PackSource::parse(config::DEFAULT_PACK_SOURCE)?;
    let mut state = status().unwrap_or_default();
    let mut due = Instant::now();
    while !EXIT.load(Ordering::Acquire) {
        if config::data_dir().join("background-exit").exists() {
            fs::remove_file(config::data_dir().join("background-exit"))?;
            break;
        }
        let mut enrolled = enrollment()?;
        if enrolled.is_none() && Instant::now() >= due {
            let owned: Vec<_> = updater_profiles::discover()?
                .into_iter()
                .filter(|p| crate::modrinth_setup::marker(&p.directory).is_some())
                .filter_map(|p| {
                    identity(&p.directory).ok().map(|id| Enrollment {
                        enabled: true,
                        profile: p.directory,
                        instance_id: id,
                    })
                })
                .collect();
            if owned.len() == 1 {
                let e = owned.into_iter().next().unwrap();
                util::write_atomic(&enrollment_path(), &serde_json::to_vec(&e)?)?;
                enrolled = Some(e);
            } else {
                state.phase = "setup".into();
                state.message="Waiting for your independent profile. Finish installation in Modrinth; then updates start automatically.".into();
                save_status(&state)?;
                due = Instant::now() + Duration::from_secs(10);
            }
        }
        let Some(e) = enrolled else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        if !e.enabled {
            if state.phase != "paused" {
                state.phase = "paused".into();
                state.message =
                    "Automatic updates are paused. Open the updater to enable them.".into();
                save_status(&state)?;
            }
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        if CHECK_NOW.swap(false, Ordering::AcqRel) || Instant::now() >= due {
            state.phase = "checking".into();
            state.message = "Checking for pack updates…".into();
            save_status(&state)?;
            if let Err(err) = rt.block_on(cycle(&e, &client, &pack, &mut state)) {
                state.phase = "error".into();
                state.message = format!("Update deferred; retrying automatically: {err:#}");
            }
            save_status(&state)?;
            due = Instant::now()
                + if state.phase == "waiting" {
                    Duration::from_secs(30)
                } else {
                    INTERVAL
                };
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if let Some(error) = TRAY_ERROR.lock().unwrap().take() {
        state.phase = "error".into();
        state.message = error;
    } else {
        state.phase = "stopped".into();
        state.message = "Background updater stopped".into();
    }
    save_status(&state)?;
    Ok(())
}
