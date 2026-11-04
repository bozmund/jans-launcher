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
    ensure!(
        kind == "local",
        "Automatic updates need the new local profile, not the old shared/imported modpack. Create a separate local Fabric 26.3 profile first."
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
    Ok(directories::BaseDirs::new()
        .context("Missing user data folder")?
        .data_local_dir()
        .join("Jan's Modpack Updater/jans-modpack-updater.exe"))
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
    let Some(e) = enrollment()? else {
        return Ok(());
    };
    if !e.enabled {
        return Ok(());
    }
    #[cfg(windows)]
    std::thread::spawn(|| {
        if let Err(err) = crate::updater_tray::run() {
            EXIT.store(true, Ordering::Release);
            let _ = save_status(&Status {
                phase: "error".into(),
                message: format!("Tray unavailable: {err:#}"),
                ..Default::default()
            });
        }
    });
    let rt = tokio::runtime::Runtime::new()?;
    let client = util::http();
    let pack = source::PackSource::parse(config::DEFAULT_PACK_SOURCE)?;
    let mut state = status().unwrap_or_default();
    let mut due = Instant::now();
    while !EXIT.load(Ordering::Acquire) {
        let Some(e) = enrollment()? else { break };
        if !e.enabled {
            break;
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
    state.phase = "stopped".into();
    state.message = "Background updater stopped".into();
    save_status(&state)?;
    Ok(())
}
