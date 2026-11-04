//! One-time import of a player's own files from an existing launcher profile.

use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

/// Copied from the old profile; never overwrites anything that already exists.
const PERSONAL: &[&str] = &[
    "saves",
    "options.txt",
    "servers.dat",
    "screenshots",
    "schematics",
    "journeymap",
    "xaero",
];

/// Existing Modrinth App / Prism profiles on this machine, as (label, folder).
pub fn find_profiles() -> Vec<(String, PathBuf)> {
    let mut roots: Vec<(&str, PathBuf, bool)> = Vec::new();
    if let Some(base) = directories::BaseDirs::new() {
        // Modrinth App keeps profiles in its data dir on every OS.
        roots.push((
            "Modrinth",
            base.data_dir().join("ModrinthApp").join("profiles"),
            false,
        ));
        roots.push((
            "Prism",
            base.data_dir().join("PrismLauncher").join("instances"),
            true,
        ));
    }
    let mut out = Vec::new();
    for (label, root, prism) in roots {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in rd.flatten() {
            let mut dir = e.path();
            if prism {
                dir = dir.join("minecraft");
            }
            if dir.join("mods").is_dir() || dir.join("saves").is_dir() {
                out.push((format!("{label}: {}", e.file_name().to_string_lossy()), dir));
            }
        }
    }
    out.sort();
    out
}

/// Copy worlds, options and similar personal files. Returns how many files were copied.
pub fn import(from: &Path, to: &Path) -> Result<usize> {
    std::fs::create_dir_all(to)?;
    let from = std::fs::canonicalize(from)?;
    let to = std::fs::canonicalize(to)?;
    ensure!(
        !from.starts_with(&to) && !to.starts_with(&from),
        "import folders must be separate, not nested"
    );
    let _lock = crate::transaction::InstanceLock::acquire(&to)?;
    crate::transaction::ensure_game_stopped(&from)?;
    crate::transaction::ensure_game_stopped(&to)?;
    let mut n = 0;
    for name in PERSONAL {
        let src = from.join(name);
        if src.exists() {
            if *name == "saves" {
                std::fs::create_dir_all(to.join("saves"))?;
                for world in std::fs::read_dir(&src)? {
                    let world = world?;
                    let dest = to.join("saves").join(world.file_name());
                    if dest.exists() {
                        continue;
                    } // Never merge chunks from different worlds.
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_nanos();
                    let temp =
                        crate::util::safe_join(&to, &format!(".jans-launcher/import/{stamp}"))?;
                    let result = (|| -> Result<usize> {
                        let count = copy_missing(&world.path(), &temp)?;
                        crate::transaction::ensure_game_stopped(&from)?;
                        std::fs::rename(&temp, &dest).context("finish world import")?;
                        Ok(count)
                    })();
                    if result.is_err() && temp.exists() {
                        if temp.is_dir() {
                            let _ = std::fs::remove_dir_all(&temp);
                        } else {
                            let _ = std::fs::remove_file(&temp);
                        }
                    }
                    n += result?;
                }
            } else {
                n += copy_missing(&src, &to.join(name))?;
            }
        }
    }
    Ok(n)
}

fn copy_missing(src: &Path, dst: &Path) -> Result<usize> {
    if src.is_file() {
        if dst.exists() {
            return Ok(0);
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
        return Ok(1);
    }
    let mut n = 0;
    for e in walkdir::WalkDir::new(src) {
        let e = e?;
        let rel = e.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if e.file_type().is_file() && !target.exists() {
            std::fs::copy(e.path(), &target)?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_worlds_are_not_merged_and_nested_imports_are_rejected() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("jans-import-{stamp}"));
        let from = root.join("from");
        let to = root.join("to");
        std::fs::create_dir_all(from.join("saves/Same World")).unwrap();
        std::fs::create_dir_all(to.join("saves/Same World")).unwrap();
        std::fs::write(from.join("saves/Same World/foreign-region"), b"foreign").unwrap();
        std::fs::write(to.join("saves/Same World/level.dat"), b"mine").unwrap();
        import(&from, &to).unwrap();
        assert!(!to.join("saves/Same World/foreign-region").exists());
        assert_eq!(
            std::fs::read(to.join("saves/Same World/level.dat")).unwrap(),
            b"mine"
        );
        assert!(import(&from, &from.join("child")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
