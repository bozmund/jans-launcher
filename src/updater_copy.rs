//! Copy player data into a native, independently registered Modrinth profile.
//! Registration stays with Modrinth: its private database is never edited here.
use crate::{transaction, updater_background, updater_profiles, util};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn inventory(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() != 1 || e.file_name() != ".jans-launcher")
    {
        let e = entry?;
        if e.depth() == 0 {
            continue;
        }
        let rel = e
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("Profile contains a non-Unicode filename")?
            .replace('\\', "/");
        let path = util::safe_join(root, &rel)?; // also refuses Windows junctions/reparse points
        ensure!(
            !e.file_type().is_symlink(),
            "Cannot copy linked file safely: {}",
            path.display()
        );
        if e.file_type().is_file() {
            files.insert(rel, util::sha512_file(&path)?);
        } else {
            ensure!(
                e.file_type().is_dir(),
                "Unsupported profile file: {}",
                path.display()
            );
        }
    }
    Ok(files)
}
pub fn copy(source: &Path, destination: &Path) -> Result<usize> {
    let source = updater_profiles::from_directory(source)?;
    let dest = updater_profiles::from_directory(destination)?;
    ensure!(
        source.directory != dest.directory,
        "Choose a different, new profile as the destination"
    );
    updater_background::identity(&dest.directory)?;
    ensure!(
        source.game_version == dest.game_version && source.loader_version == dest.loader_version,
        "Set the new profile to the same Fabric loader version as the old profile before copying"
    );
    // Refuse to merge worlds/mods from two played profiles. Destination must be a fresh native profile.
    for folder in ["saves", "mods", "screenshots"] {
        let path = util::safe_join(&dest.directory, folder)?;
        ensure!(
            !path.exists() || fs::read_dir(path)?.next().is_none(),
            "Destination already contains {folder}. Create a new empty local profile in Modrinth instead"
        );
    }
    let _source_lock = transaction::InstanceLock::acquire(&source.directory)?;
    let _dest_lock = transaction::InstanceLock::acquire(&dest.directory)?;
    transaction::ensure_game_stopped(&source.directory)?;
    transaction::ensure_game_stopped(&dest.directory)?;
    transaction::recover(&dest.directory)?;
    ensure!(
        !source
            .directory
            .join(".jans-launcher/transaction/journal.json")
            .exists(),
        "The source has an unfinished update. Recover that update before copying"
    );
    let before = inventory(&source.directory)?;
    let stage = util::safe_join(&dest.directory, ".jans-launcher/profile-copy")?;
    fs::create_dir_all(&stage)?;
    let mut replacements: Vec<(String, Option<PathBuf>)> = vec![];
    for (relative, hash) in &before {
        let from = util::safe_join(&source.directory, relative)?;
        let to = util::safe_join(&stage, relative)?;
        fs::create_dir_all(to.parent().unwrap())?;
        fs::copy(&from, &to)
            .with_context(|| format!("Copy {relative}; check available disk space"))?;
        ensure!(
            util::sha512_file(&to)? == *hash,
            "Source changed while copying {relative}; no files installed. Close Minecraft and retry"
        );
        replacements.push((relative.clone(), Some(to)));
    }
    ensure!(
        inventory(&source.directory)? == before,
        "Source profile changed while copying. Nothing installed; retry after closing Minecraft"
    );
    transaction::ensure_game_stopped(&source.directory)?;
    transaction::ensure_game_stopped(&dest.directory)?;
    updater_background::identity(&dest.directory)?;
    let marker = dest
        .directory
        .join(".jans-launcher/profile-copy-record.json");
    util::write_atomic(
        &marker,
        &serde_json::to_vec(
            &serde_json::json!({"source":source.directory,"created":time::OffsetDateTime::now_utc().to_string(),"file_count":before.len()}),
        )?,
    )?;
    replacements.push((".jans-launcher/copied-profile.json".into(), Some(marker)));
    transaction::commit(&dest.directory, &replacements)?;
    // The source and all its worlds remain intact. Empty staging directories are harmless.
    Ok(before.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_personal_data_without_copying_the_shared_link_or_changing_source() {
        let root = std::env::temp_dir().join(format!(
            "jans-copy-{}-{}",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let source = root.join("profiles/Old");
        let dest = root.join("profiles/New");
        fs::create_dir_all(source.join("saves/My world")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(source.join("saves/My world/level.dat"), b"world").unwrap();
        fs::write(source.join("options.txt"), b"settings").unwrap();
        let db = rusqlite::Connection::open(root.join("app.db")).unwrap();
        db.execute_batch("CREATE TABLE instances(id TEXT,name TEXT,path TEXT,applied_content_set_id TEXT);CREATE TABLE instance_content_sets(id TEXT,source_kind TEXT,game_version TEXT,loader TEXT,loader_version TEXT);CREATE TABLE instance_links(instance_id TEXT,link_kind TEXT);INSERT INTO instances VALUES('old','Old','Old','s1'),('new','New','New','s2');INSERT INTO instance_content_sets VALUES('s1','shared_instance','26.3','fabric','0.20.1'),('s2','local','26.3','fabric','0.20.1');INSERT INTO instance_links VALUES('old','shared_instance');").unwrap();
        drop(db);
        let hash = util::sha512_file(&root.join("app.db")).unwrap();
        let old = inventory(&source).unwrap();
        assert_eq!(copy(&source, &dest).unwrap(), 2);
        assert_eq!(inventory(&dest).unwrap(), old);
        assert_eq!(inventory(&source).unwrap(), old);
        assert_eq!(util::sha512_file(&root.join("app.db")).unwrap(), hash);
        assert!(
            copy(&source, &dest)
                .unwrap_err()
                .to_string()
                .contains("already contains")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
