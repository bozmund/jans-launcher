//! Comparing the installed pack with the newest one and applying the difference.
//!
//! Safety rules: only files the pack lists are ever written; a file is only removed (moved to
//! `.jans-launcher/trash`) if an earlier pack version installed it and the new one no longer has it;
//! `once` files (configs) are only written when missing. Worlds, options.txt and the player's own
//! extra mods are never touched because no manifest lists them.

use crate::manifest::{ChangelogEntry, FileEntry, Manifest, Policy};
use crate::source::Fetched;
use crate::util::{self, Expect};
use anyhow::{Context, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Progress callback: (what, done, total).
pub type ProgressFn = Arc<dyn Fn(&str, u64, u64) + Send + Sync>;

const META_DIR: &str = ".jans-launcher";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub pack_name: String,
    pub build: u64,
    pub version: String,
    pub revision: String,
    pub updated_at: String,
    pub files: BTreeMap<String, InstalledFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledFile {
    /// Empty when the file was left as the player had it (a `once` file that already existed).
    pub sha512: String,
    pub size: u64,
    pub mtime: u64,
    pub policy: Policy,
}

impl State {
    pub fn load(instance: &Path) -> Option<State> {
        let b = std::fs::read(instance.join(META_DIR).join("state.json")).ok()?;
        serde_json::from_slice(&b).ok()
    }
}

pub fn installed_manifest(instance: &Path) -> Option<Manifest> {
    let b = std::fs::read(instance.join(META_DIR).join("installed-pack.json")).ok()?;
    serde_json::from_slice(&b).ok()
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub version: Option<String>,
    pub folder: String,
}

impl Item {
    fn of(f: &FileEntry) -> Item {
        Item {
            name: f.display_name(),
            version: f.version.clone(),
            folder: f.folder().to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Download(FileEntry),
    /// Move an old pack file out of the way.
    Trash(String),
}

/// Everything the "check for changes" view shows, plus the work an update would do.
#[derive(Clone, Debug)]
pub struct Changes {
    pub pack_name: String,
    pub installed_version: Option<String>,
    pub new_version: String,
    pub new_build: u64,
    pub first_install: bool,
    pub changelog: Vec<ChangelogEntry>,
    pub added: Vec<Item>,
    pub removed: Vec<Item>,
    pub updated: Vec<(Item, Item)>,
    pub other_files_changed: usize,
    pub actions: Vec<Action>,
    pub download_bytes: u64,
    pub revision: String,
    pub base: String,
    pub needs_record: bool,
    pub manifest: Manifest,
}

impl Changes {
    pub fn up_to_date(&self) -> bool {
        self.actions.is_empty() && !self.needs_record
    }
}

/// Compare what is installed in `instance` with `fetched`. Reads the disk (and hashes files whose
/// size/mtime are unknown) but changes nothing.
pub fn plan(instance: &Path, fetched: Fetched) -> Result<Changes> {
    let state = State::load(instance);
    let old = installed_manifest(instance);
    let new = fetched.manifest;
    new.validate()?;
    let needs_record = state.as_ref().is_none_or(|s| {
        s.build != new.build || s.version != new.version || s.revision != fetched.revision
    }) || util::safe_join(instance, ".jans-launcher/transaction/journal.json")?
        .exists();

    // What the player sees: content differences between the two manifests.
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut updated = Vec::new();
    let mut other_files_changed = 0;
    let old_by_key: HashMap<String, &FileEntry> = old
        .as_ref()
        .map(|m| m.files.iter().map(|f| (f.key(), f)).collect())
        .unwrap_or_default();
    let new_keys: std::collections::HashSet<String> = new.files.iter().map(|f| f.key()).collect();
    for f in &new.files {
        match old_by_key.get(&f.key()) {
            None if f.is_content() => added.push(Item::of(f)),
            Some(o) if o.sha512 != f.sha512 && f.is_content() => {
                updated.push((Item::of(o), Item::of(f)))
            }
            None => other_files_changed += 1,
            Some(o) if o.sha512 != f.sha512 => other_files_changed += 1,
            _ => {}
        }
    }
    if let Some(o) = &old {
        for f in &o.files {
            if !new_keys.contains(&f.key()) {
                if f.is_content() {
                    removed.push(Item::of(f));
                } else {
                    other_files_changed += 1;
                }
            }
        }
    }
    let sort = |v: &mut Vec<Item>| v.sort_by_key(|i| i.name.to_lowercase());
    sort(&mut added);
    sort(&mut removed);
    updated.sort_by_key(|(_, n)| n.name.to_lowercase());

    // What actually has to happen on disk.
    let known = state.as_ref().map(|s| &s.files);
    let mut actions = Vec::new();
    let mut download_bytes = 0;
    for f in &new.files {
        let dest = util::safe_join(instance, &f.path)?;
        let needed = match f.policy {
            Policy::Once => !dest.exists(),
            Policy::Managed => !matches_on_disk(&dest, f, known.and_then(|k| k.get(&f.path)))?,
        };
        if needed {
            if f.policy == Policy::Managed
                && dest.exists()
                && !known.is_some_and(|k| {
                    k.keys()
                        .any(|p| filesystem_key(p) == filesystem_key(&f.path))
                })
            {
                let same_mod = f.mod_id.as_ref().is_some_and(|id| {
                    crate::publish::fabric_mod_info(&dest).is_some_and(|m| &m.id == id)
                });
                anyhow::ensure!(
                    same_mod,
                    "Untracked file conflicts with the pack: {}. Move it aside before updating; your file was not overwritten.",
                    f.path
                );
            }
            download_bytes += f.size;
            actions.push(Action::Download(f.clone()));
        }
    }
    let new_paths: std::collections::HashSet<String> =
        new.files.iter().map(|f| filesystem_key(&f.path)).collect();
    if let Some(s) = &state {
        for (path, inst) in &s.files {
            let folder = path.split('/').next().unwrap_or("");
            anyhow::ensure!(
                matches!(
                    folder,
                    "mods"
                        | "config"
                        | "defaultconfigs"
                        | "shaderpacks"
                        | "resourcepacks"
                        | "global_packs"
                        | "datapacks"
                ),
                "Installed state references a protected location: {path}"
            );
            if inst.policy == Policy::Managed
                && !new_paths.contains(&filesystem_key(path))
                && util::safe_join(instance, path)?.exists()
            {
                actions.push(Action::Trash(path.clone()));
            }
        }
    }
    // Adopt only previous variants of exactly the pack's Fabric IDs. Extra mods stay untouched.
    let mods = util::safe_join(instance, "mods")?;
    if mods.is_dir() {
        let desired: HashMap<&str, &FileEntry> = new
            .files
            .iter()
            .filter(|f| f.folder() == "mods")
            .filter_map(|f| f.mod_id.as_deref().map(|id| (id, f)))
            .collect();
        for file in std::fs::read_dir(mods)? {
            let file = file?;
            if file.path().extension().and_then(|x| x.to_str()) != Some("jar") {
                continue;
            }
            let path = format!("mods/{}", file.file_name().to_string_lossy());
            let abs = util::safe_join(instance, &path)?;
            let Some(info) = crate::publish::fabric_mod_info(&abs) else {
                continue;
            };
            let Some(target) = desired.get(info.id.as_str()) else {
                continue;
            };
            if filesystem_key(&target.path) != filesystem_key(&path)
                && !actions
                    .iter()
                    .any(|a| matches!(a, Action::Trash(p) if p == &path))
            {
                removed.push(Item {
                    name: info.name.unwrap_or(info.id),
                    version: info.version,
                    folder: "mods".into(),
                });
                actions.push(Action::Trash(path));
            }
        }
    }

    let installed_build = state.as_ref().map(|s| s.build).unwrap_or(0);
    let changelog = new
        .changelog
        .iter()
        .filter(|c| c.build > installed_build)
        .cloned()
        .collect();
    Ok(Changes {
        pack_name: new.name.clone(),
        installed_version: state.as_ref().map(|s| s.version.clone()),
        new_version: new.version.clone(),
        new_build: new.build,
        first_install: state.is_none(),
        changelog,
        added,
        removed,
        updated,
        other_files_changed,
        actions,
        download_bytes,
        revision: fetched.revision,
        base: fetched.base,
        needs_record,
        manifest: new,
    })
}

fn matches_on_disk(dest: &Path, f: &FileEntry, _known: Option<&InstalledFile>) -> Result<bool> {
    let Ok(meta) = std::fs::metadata(dest) else {
        return Ok(false);
    };
    Ok(meta.is_file()
        && meta.len() == f.size
        && util::sha512_file(dest)?.eq_ignore_ascii_case(&f.sha512))
}

fn filesystem_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// Stage every download before touching the instance; journal file and state replacements together.
pub async fn apply(
    client: &reqwest::Client,
    instance: &Path,
    changes: &Changes,
    progress: ProgressFn,
) -> Result<()> {
    let _lock = crate::transaction::InstanceLock::acquire(instance)?;
    crate::transaction::ensure_game_stopped(instance)?;
    crate::transaction::recover(instance)?;
    // A preview is not authority to overwrite a newer local state: recalculate under the lock.
    let changes = plan(
        instance,
        Fetched {
            manifest: changes.manifest.clone(),
            base: changes.base.clone(),
            revision: changes.revision.clone(),
        },
    )?;
    let cache = util::safe_join(instance, ".jans-launcher/cache")?;
    std::fs::create_dir_all(&cache)?;
    let downloads: Vec<FileEntry> = changes
        .actions
        .iter()
        .filter_map(|a| {
            if let Action::Download(f) = a {
                Some(f.clone())
            } else {
                None
            }
        })
        .collect();
    let total = changes.download_bytes;
    let done = Arc::new(AtomicU64::new(0));
    progress("download", 0, total);
    // A content hash can be shared by several files. Download it only once per operation.
    let mut unique = BTreeMap::new();
    for f in &downloads {
        unique
            .entry(f.sha512.to_lowercase())
            .or_insert_with(|| f.clone());
    }
    let results: Vec<Result<()>> = futures::stream::iter(unique.into_values().map(|f| {
        let done = done.clone();
        let progress = progress.clone();
        let client = client.clone();
        let url = util::resolve_url(&changes.base, &f.url);
        let dest = cache.join(f.sha512.to_lowercase());
        async move {
            let on_bytes = |n: u64| {
                let d = done.fetch_add(n, Ordering::Relaxed) + n;
                progress("download", d.min(total), total);
            };
            if dest.exists()
                && std::fs::metadata(&dest)?.len() == f.size
                && util::sha512_file(&dest)?.eq_ignore_ascii_case(&f.sha512)
            {
                on_bytes(f.size);
                return Ok(());
            }
            util::download(
                &client,
                &url,
                &dest,
                &Expect::Sha512(f.sha512.clone()),
                &on_bytes,
            )
            .await
            .with_context(|| f.path.clone())?;
            anyhow::ensure!(
                std::fs::metadata(&dest)?.len() == f.size,
                "wrong file size for {}",
                f.path
            );
            Ok(())
        }
    }))
    .buffer_unordered(8)
    .collect()
    .await;
    let errors: Vec<_> = results
        .into_iter()
        .filter_map(|r| r.err())
        .map(|e| format!("{e:#}"))
        .collect();
    if !errors.is_empty() {
        anyhow::bail!(
            "Update not installed; previous files are unchanged. Verified downloads are cached.\n{}",
            errors.join("\n")
        );
    }
    let tx = util::safe_join(instance, ".jans-launcher/transaction/new")?;
    let mut staged = HashMap::new();
    let mut replacements = Vec::new();
    for f in &downloads {
        let path = util::safe_join(&tx, &f.path)?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::copy(cache.join(f.sha512.to_lowercase()), &path)?;
        staged.insert(f.path.clone(), path.clone());
        replacements.push((f.path.clone(), Some(path)));
    }
    for action in &changes.actions {
        if let Action::Trash(path) = action {
            replacements.push((path.clone(), None));
        }
    }
    let state = make_state(instance, &changes, &staged)?;
    for (name, bytes) in [
        (
            ".jans-launcher/installed-pack.json",
            serde_json::to_vec_pretty(&changes.manifest)?,
        ),
        (
            ".jans-launcher/state.json",
            serde_json::to_vec_pretty(&state)?,
        ),
    ] {
        let path = util::safe_join(&tx, name)?;
        util::write_atomic(&path, &bytes)?;
        replacements.push((name.into(), Some(path)));
    }
    progress("install", 0, replacements.len() as u64);
    crate::transaction::ensure_game_stopped(instance)?;
    crate::transaction::commit(instance, &replacements)?;
    progress("done", total, total);
    Ok(())
}

fn make_state(
    instance: &Path,
    changes: &Changes,
    staged: &HashMap<String, PathBuf>,
) -> Result<State> {
    let m = &changes.manifest;
    let mut files = BTreeMap::new();
    for f in &m.files {
        let dest = util::safe_join(instance, &f.path)?;
        let p = staged.get(&f.path).unwrap_or(&dest);
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        let ours = f.policy == Policy::Managed || staged.contains_key(&f.path);
        files.insert(
            f.path.clone(),
            InstalledFile {
                sha512: if ours {
                    f.sha512.clone()
                } else {
                    String::new()
                },
                size: meta.len(),
                mtime: util::mtime_secs(&meta),
                policy: f.policy,
            },
        );
    }
    Ok(State {
        pack_name: m.name.clone(),
        build: m.build,
        version: m.version.clone(),
        revision: changes.revision.clone(),
        updated_at: util::now_string(),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    fn fixture() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "jans-sync-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    fn entry(path: &str, bytes: &[u8], url: &str, policy: Policy) -> FileEntry {
        FileEntry {
            path: path.into(),
            sha512: hex::encode(sha2::Sha512::digest(bytes)),
            size: bytes.len() as u64,
            url: url.into(),
            policy,
            name: None,
            version: None,
            project: None,
            mod_id: None,
            source: None,
        }
    }
    fn manifest(build: u64, files: Vec<FileEntry>) -> Manifest {
        Manifest {
            format: 1,
            name: "fixture".into(),
            build,
            version: build.to_string(),
            minecraft: "26.3".into(),
            fabric_loader: "0.20.1".into(),
            recommended_memory_mb: None,
            files,
            changelog: vec![],
        }
    }
    fn fetched(m: Manifest, base: &Path) -> Fetched {
        Fetched {
            revision: m.build.to_string(),
            manifest: m,
            base: base.to_string_lossy().into(),
        }
    }
    #[tokio::test]
    async fn failed_download_is_atomic_and_retry_reuses_verified_cache() {
        let root = fixture();
        let source = root.join("source");
        let instance = root.join("game");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("old"), b"old").unwrap();
        let old = manifest(
            1,
            vec![
                entry("mods/a.jar", b"old", "old", Policy::Managed),
                entry("mods/removed.jar", b"old", "old", Policy::Managed),
            ],
        );
        let progress: ProgressFn = Arc::new(|_, _, _| {});
        let client = util::http();
        let changes = plan(&instance, fetched(old, &source)).unwrap();
        apply(&client, &instance, &changes, progress.clone())
            .await
            .unwrap();
        let old_state = std::fs::read(instance.join(".jans-launcher/state.json")).unwrap();
        std::fs::write(source.join("a"), b"new-a").unwrap();
        std::fs::write(source.join("b"), b"bad").unwrap();
        let new = manifest(
            2,
            vec![
                entry("mods/a.jar", b"new-a", "a", Policy::Managed),
                entry("mods/b.jar", b"new-b", "b", Policy::Managed),
                entry(
                    "config/user.json",
                    b"author",
                    "missing-config",
                    Policy::Once,
                ),
            ],
        );
        std::fs::create_dir_all(instance.join("config")).unwrap();
        std::fs::write(instance.join("config/user.json"), b"player").unwrap();
        std::fs::write(instance.join("mods/player-extra.jar"), b"extra").unwrap();
        std::fs::create_dir_all(instance.join("saves/precious")).unwrap();
        std::fs::write(instance.join("saves/precious/level.dat"), b"world").unwrap();
        let changes = plan(&instance, fetched(new.clone(), &source)).unwrap();
        assert!(
            apply(&client, &instance, &changes, progress.clone())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(instance.join("mods/a.jar")).unwrap(), b"old");
        assert!(instance.join("mods/removed.jar").exists());
        assert_eq!(
            std::fs::read(instance.join(".jans-launcher/state.json")).unwrap(),
            old_state
        );
        std::fs::remove_file(source.join("a")).unwrap(); // retry must use its verified cached copy
        std::fs::write(source.join("b"), b"new-b").unwrap();
        apply(&client, &instance, &changes, progress).await.unwrap();
        assert_eq!(
            std::fs::read(instance.join("mods/a.jar")).unwrap(),
            b"new-a"
        );
        assert_eq!(
            std::fs::read(instance.join("mods/b.jar")).unwrap(),
            b"new-b"
        );
        assert!(!instance.join("mods/removed.jar").exists());
        assert_eq!(
            std::fs::read(instance.join("mods/player-extra.jar")).unwrap(),
            b"extra"
        );
        assert_eq!(
            std::fs::read(instance.join("config/user.json")).unwrap(),
            b"player"
        );
        assert_eq!(
            std::fs::read(instance.join("saves/precious/level.dat")).unwrap(),
            b"world"
        );
        assert_eq!(State::load(&instance).unwrap().build, 2);
        assert!(plan(&instance, fetched(new, &source)).unwrap().up_to_date());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn metadata_only_releases_are_not_silently_skipped() {
        let root = fixture();
        let m = manifest(1, vec![]);
        let changes = plan(&root, fetched(m, &root)).unwrap();
        assert!(changes.actions.is_empty());
        assert!(!changes.up_to_date());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn manifests_cannot_target_worlds_or_launcher_metadata() {
        for path in [
            "saves/world/level.dat",
            "options.txt",
            ".jans-launcher/state.json",
        ] {
            assert!(
                manifest(1, vec![entry(path, b"data", "source", Policy::Managed)])
                    .validate()
                    .is_err()
            );
        }
    }
    fn jar(path: &Path, id: &str, version: &str) -> Vec<u8> {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            serde_json::json!({"schemaVersion":1,"id":id,"name":id,"version":version})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
        std::fs::read(path).unwrap()
    }
    #[tokio::test]
    async fn first_modrinth_profile_update_replaces_only_matching_mod_ids() {
        let root = fixture();
        let source = root.join("source");
        let game = root.join("game");
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::create_dir(&source).unwrap();
        jar(&game.join("mods/fixture-1.jar"), "fixturemod", "1");
        let extra = jar(&game.join("mods/personal.jar"), "personalmod", "1");
        let bytes = jar(&source.join("new.jar"), "fixturemod", "2");
        let mut file = entry("mods/fixture-2.jar", &bytes, "new.jar", Policy::Managed);
        file.mod_id = Some("fixturemod".into());
        let next = manifest(1, vec![file]);
        let changes = plan(&game, fetched(next, &source)).unwrap();
        assert!(
            changes
                .actions
                .iter()
                .any(|a| matches!(a, Action::Trash(p) if p == "mods/fixture-1.jar"))
        );
        apply(&util::http(), &game, &changes, Arc::new(|_, _, _| {}))
            .await
            .unwrap();
        assert!(!game.join("mods/fixture-1.jar").exists());
        assert_eq!(
            std::fs::read(game.join("mods/fixture-2.jar")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(game.join("mods/personal.jar")).unwrap(),
            extra
        );
        assert!(
            std::fs::read_dir(game.join(".jans-launcher/trash"))
                .unwrap()
                .any(|x| x.unwrap().path().join("mods/fixture-1.jar").exists())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn first_sync_never_overwrites_an_unrelated_file_at_the_same_path() {
        let root = fixture();
        std::fs::create_dir(root.join("resourcepacks")).unwrap();
        std::fs::write(root.join("resourcepacks/personal.zip"), b"player's file").unwrap();
        assert!(
            plan(
                &root,
                fetched(
                    manifest(
                        1,
                        vec![entry(
                            "resourcepacks/personal.zip",
                            b"author",
                            "x",
                            Policy::Managed
                        )]
                    ),
                    &root
                )
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(root.join("resourcepacks/personal.zip")).unwrap(),
            b"player's file"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
