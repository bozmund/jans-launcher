//! Recoverable instance mutations. Downloads finish before a journal is committed.
use crate::util;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub struct InstanceLock(File);

impl InstanceLock {
    pub fn acquire(instance: &Path) -> Result<Self> {
        let meta = util::safe_join(instance, ".jans-launcher")?;
        fs::create_dir_all(&meta)?;
        let file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(util::safe_join(&meta, "instance.lock")?)?;
        file.try_lock()
            .context("another launcher is updating or running this instance; close it and retry")?;
        Ok(Self(file))
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Other launchers do not share our lock. Do not change their running game's files.
pub fn ensure_game_stopped(instance: &Path) -> Result<()> {
    let target = fs::canonicalize(instance).unwrap_or_else(|_| instance.to_path_buf());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let target = target
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
        let script = r#"$ErrorActionPreference='Stop'; $target=$env:JANS_INSTANCE_PROBE.Replace('\','/'); $found=@(Get-CimInstance Win32_Process -Filter "Name='java.exe' OR Name='javaw.exe'" | Where-Object { $c=$_.CommandLine; $c -and ($c.Contains('KnotClient') -or $c.Contains('net.minecraft.client.main.Main')) -and $c.Replace('\','/').IndexOf($target,[StringComparison]::OrdinalIgnoreCase) -ge 0 }); if($found.Count){'running'}else{'clear'}"#;
        let result = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("JANS_INSTANCE_PROBE", target)
            .creation_flags(0x08000000)
            .output()
            .context("could not check whether Minecraft is running")?;
        anyhow::ensure!(
            result.status.success(),
            "could not check running games; close Minecraft and retry"
        );
        anyhow::ensure!(
            String::from_utf8_lossy(&result.stdout).trim() == "clear",
            "Minecraft is running in this instance. Save and close it before updating or starting another game."
        );
    }
    #[cfg(target_os = "linux")]
    {
        for proc in fs::read_dir("/proc")? {
            let proc = proc?;
            let Ok(bytes) = fs::read(proc.path().join("cmdline")) else {
                continue;
            };
            let cmd = String::from_utf8_lossy(&bytes);
            if !(cmd.contains("KnotClient") || cmd.contains("net.minecraft.client.main.Main")) {
                continue;
            }
            let same_cwd = fs::read_link(proc.path().join("cwd"))
                .ok()
                .is_some_and(|p| p == target);
            if same_cwd || cmd.contains(target.to_string_lossy().as_ref()) {
                bail!("Minecraft is running in this instance. Save and close it first.");
            }
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Entry {
    path: String,
    existed: bool,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    entries: Vec<Entry>,
    committed: bool,
}

fn directory(instance: &Path) -> Result<PathBuf> {
    util::safe_join(instance, ".jans-launcher/transaction")
}

fn write_journal(tx: &Path, journal: &Journal) -> Result<()> {
    util::write_atomic(&tx.join("journal.json"), &serde_json::to_vec(journal)?)
}

/// Restore interrupted commits before allowing either Update or Play.
pub fn recover(instance: &Path) -> Result<()> {
    let tx = directory(instance)?;
    let journal_path = tx.join("journal.json");
    if !journal_path.exists() {
        if tx.exists() {
            fs::remove_dir_all(tx)?;
        }
        return Ok(());
    }
    let journal: Journal = serde_json::from_slice(&fs::read(journal_path)?)
        .context("update journal is damaged; preserve .jans-launcher/transaction for recovery")?;
    if journal.committed {
        finish(instance, &tx)?;
        return Ok(());
    }
    for entry in journal.entries.iter().rev() {
        let dest = util::safe_join(instance, &entry.path)?;
        let backup = util::safe_join(&tx.join("backup"), &entry.path)?;
        if backup.exists() {
            if dest.exists() {
                fs::remove_file(&dest)?;
            }
            fs::create_dir_all(dest.parent().unwrap())?;
            fs::rename(backup, dest)?;
        } else if !entry.existed && dest.exists() {
            fs::remove_file(dest)?;
        }
    }
    fs::remove_dir_all(tx)?;
    Ok(())
}

fn finish(instance: &Path, tx: &Path) -> Result<()> {
    let backup = tx.join("backup");
    if backup.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let trash = util::safe_join(instance, &format!(".jans-launcher/trash/{stamp}"))?;
        fs::create_dir_all(trash.parent().unwrap())?;
        fs::rename(backup, trash)?;
    }
    fs::remove_dir_all(tx)?;
    Ok(())
}

/// `replacements` are verified staged files; None means move a removed pack file to trash.
pub fn commit(instance: &Path, replacements: &[(String, Option<PathBuf>)]) -> Result<()> {
    let tx = directory(instance)?;
    fs::create_dir_all(&tx)?;
    let journal = Journal {
        entries: replacements
            .iter()
            .map(|(path, _)| {
                let dest = util::safe_join(instance, path)?;
                if dest.exists() && !dest.is_file() {
                    bail!("expected a file: {}", dest.display());
                }
                Ok(Entry {
                    path: path.clone(),
                    existed: dest.exists(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        committed: false,
    };
    write_journal(&tx, &journal)?;
    let result = (|| -> Result<()> {
        for (path, source) in replacements {
            let dest = util::safe_join(instance, path)?;
            if dest.exists() {
                let backup = util::safe_join(&tx.join("backup"), path)?;
                fs::create_dir_all(backup.parent().unwrap())?;
                fs::rename(&dest, backup)?;
            }
            if let Some(source) = source {
                fs::create_dir_all(dest.parent().unwrap())?;
                fs::rename(source, &dest).with_context(|| format!("install {}", dest.display()))?;
            }
        }
        write_journal(
            &tx,
            &Journal {
                committed: true,
                ..journal
            },
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        recover(instance).context(
            "update failed and automatic rollback also failed; close Minecraft and retry",
        )?;
        return Err(error.context("update failed; previous version restored"));
    }
    // A completed update remains valid if cleaning up its backups fails.
    let _ = finish(instance, &tx);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "jans-tx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn failed_install_restores_every_previous_file() {
        let root = fixture();
        fs::create_dir(root.join("mods")).unwrap();
        fs::write(root.join("mods/a.jar"), b"old").unwrap();
        let staged = root.join("staged");
        fs::write(&staged, b"new").unwrap();
        let result = commit(
            &root,
            &[
                ("mods/a.jar".into(), Some(staged)),
                ("mods/b.jar".into(), Some(root.join("missing-source"))),
            ],
        );
        assert!(result.is_err());
        assert_eq!(fs::read(root.join("mods/a.jar")).unwrap(), b"old");
        assert!(!root.join("mods/b.jar").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_commit_restores_content_and_metadata() {
        let root = fixture();
        let tx = directory(&root).unwrap();
        fs::create_dir_all(tx.join("backup/mods")).unwrap();
        fs::write(tx.join("backup/mods/a.jar"), b"old").unwrap();
        fs::create_dir(root.join("mods")).unwrap();
        fs::write(root.join("mods/a.jar"), b"new").unwrap();
        write_journal(
            &tx,
            &Journal {
                entries: vec![Entry {
                    path: "mods/a.jar".into(),
                    existed: true,
                }],
                committed: false,
            },
        )
        .unwrap();
        recover(&root).unwrap();
        assert_eq!(fs::read(root.join("mods/a.jar")).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn competing_updaters_cannot_acquire_the_same_instance() {
        let root = fixture();
        let lock = InstanceLock::acquire(&root).unwrap();
        assert!(InstanceLock::acquire(&root).is_err());
        drop(lock);
        drop(InstanceLock::acquire(&root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
