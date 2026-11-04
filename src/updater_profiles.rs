//! Read-only discovery of Modrinth App instances. Never read account tables.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub directory: PathBuf,
    pub game_version: String,
    pub loader: String,
    pub loader_version: String,
}

impl Profile {
    pub fn compatible(&self, manifest: &crate::manifest::Manifest) -> Result<()> {
        ensure!(
            manifest.minecraft == crate::config::PACK_MINECRAFT,
            "Only Jan's Fabric 26.3 pack is supported"
        );
        ensure!(
            self.game_version == manifest.minecraft && self.loader.eq_ignore_ascii_case("fabric"),
            "Choose an existing Fabric {} Modrinth profile",
            manifest.minecraft
        );
        ensure!(
            self.loader_version == manifest.fabric_loader,
            "This profile uses Fabric loader {}. Set its Fabric loader to {} in Modrinth before updating the pack",
            self.loader_version,
            manifest.fabric_loader
        );
        Ok(())
    }
}

pub fn from_app_root(root: &Path) -> Result<Vec<Profile>> {
    let db = root.join("app.db");
    ensure!(
        db.is_file(),
        "No Modrinth app.db found at {}",
        root.display()
    );
    let connection = Connection::open_with_flags(
        &db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("Cannot read the Modrinth profile list")?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let mut statement=connection.prepare("SELECT i.name, i.path, c.game_version, c.loader, c.loader_version FROM instances i JOIN instance_content_sets c ON c.id = i.applied_content_set_id")
        .context("This updater needs Modrinth's instances/content-sets database format (tested with Modrinth App 0.22). No database changes were made")?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut profiles = vec![];
    for row in rows {
        let (name, path, game_version, loader, loader_version) = row?;
        if game_version != crate::config::PACK_MINECRAFT || !loader.eq_ignore_ascii_case("fabric") {
            continue;
        }
        let directory = crate::util::safe_join(&root.join("profiles"), &path)?;
        if !directory.is_dir() {
            continue;
        }
        profiles.push(Profile {
            name,
            directory: std::fs::canonicalize(directory)?,
            game_version,
            loader,
            loader_version: loader_version.unwrap_or_default(),
        });
    }
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(profiles)
}

pub fn discover() -> Result<Vec<Profile>> {
    let base =
        directories::BaseDirs::new().context("Cannot locate your application data directory")?;
    let root = base.data_dir().join("ModrinthApp");
    if !root.join("app.db").is_file() {
        return Ok(vec![]);
    }
    from_app_root(&root)
}

pub fn from_directory(directory: &Path) -> Result<Profile> {
    let directory =
        std::fs::canonicalize(directory).context("Choose an existing Modrinth profile folder")?;
    let profiles = directory.parent().context("Profile folder has no parent")?;
    ensure!(
        profiles
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("profiles")),
        "Choose the Fabric 26.3 folder inside ModrinthApp/profiles"
    );
    let root = profiles
        .parent()
        .context("Cannot find the Modrinth data directory")?;
    from_app_root(root)?.into_iter().find(|p|p.directory==directory)
        .context("This folder is not a registered Fabric 26.3 profile. Select Fabric 26.3 in Modrinth first")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> crate::manifest::Manifest {
        serde_json::from_value(serde_json::json!({"format":1,"name":"fixture","build":1,"version":"1","minecraft":"26.3","fabric_loader":"0.20.1","files":[]})).unwrap()
    }
    #[test]
    fn reads_only_registered_matching_instances_without_changing_database() {
        let root =
            std::env::temp_dir().join(format!("jans-updater-profiles-{}", std::process::id()));
        assert!(!root.exists());
        std::fs::create_dir_all(root.join("profiles/Fabric 26.3")).unwrap();
        std::fs::create_dir_all(root.join("profiles/Other")).unwrap();
        let db = root.join("app.db");
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE instances(name TEXT,path TEXT,applied_content_set_id TEXT);CREATE TABLE instance_content_sets(id TEXT,game_version TEXT,loader TEXT,loader_version TEXT);INSERT INTO instances VALUES('Friends','Fabric 26.3','one'),('Other','Other','two');INSERT INTO instance_content_sets VALUES('one','26.3','fabric','0.20.1'),('two','1.20.1','forge','47.4.20');").unwrap();
        drop(c);
        let before = crate::util::sha512_file(&db).unwrap();
        let profiles = from_app_root(&root).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Friends");
        profiles[0].compatible(&manifest()).unwrap();
        assert!(from_directory(&root.join("profiles/Other")).is_err());
        let profile = from_directory(&root.join("profiles/Fabric 26.3")).unwrap();
        let mut wrong = profile.clone();
        wrong.loader_version = "0.19.3".into();
        assert!(wrong.compatible(&manifest()).is_err());
        assert_eq!(crate::util::sha512_file(&db).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
