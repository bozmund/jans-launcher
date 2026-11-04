//! Mojang's Java runtimes (the same ones the official launcher downloads).

use crate::sync::ProgressFn;
use crate::util::{self, Expect};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const ALL_RUNTIMES: &str = "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-x64"),
        ("windows", "aarch64") => Some("windows-arm64"),
        ("windows", "x86") => Some("windows-x86"),
        ("linux", "x86_64") => Some("linux"),
        ("linux", "x86") => Some("linux-i386"),
        ("macos", "x86_64") => Some("mac-os"),
        ("macos", "aarch64") => Some("mac-os-arm64"),
        _ => None,
    }
}

#[derive(Deserialize)]
struct RuntimeEntry {
    manifest: Download,
}

#[derive(Deserialize)]
struct Download {
    sha1: String,
    #[serde(default)]
    size: u64,
    url: String,
}

#[derive(Deserialize)]
struct RuntimeManifest {
    files: HashMap<String, RuntimeFile>,
}

#[derive(Deserialize)]
struct RuntimeFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    downloads: Option<HashMap<String, Download>>,
    #[serde(default)]
    target: Option<String>,
}

pub fn java_binary(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("bin").join("javaw.exe")
    } else {
        home.join("bin").join("java")
    }
}

/// Make sure the runtime `component` (e.g. "java-runtime-epsilon") is installed under
/// `<data>/runtimes/<component>` and return the java executable.
pub async fn ensure(
    client: &reqwest::Client,
    data: &Path,
    component: &str,
    progress: ProgressFn,
) -> Result<PathBuf> {
    let home = data.join("runtimes").join(component);
    let marker = home.join(".installed");
    let java = java_binary(&home);

    let Some(platform) = platform() else {
        bail!("no Mojang Java runtime for this platform; set a Java path in the settings");
    };
    let all: HashMap<String, HashMap<String, Vec<RuntimeEntry>>> =
        match client.get(ALL_RUNTIMES).send().await {
            Ok(r) => r.error_for_status()?.json().await?,
            // Offline: use whatever is installed.
            Err(e) if java.exists() => {
                eprintln!("java runtime list unavailable ({e}), using installed runtime");
                return Ok(java);
            }
            Err(e) => return Err(e.into()),
        };
    let entry = all
        .get(platform)
        .and_then(|p| p.get(component))
        .and_then(|v| v.first())
        .with_context(|| format!("Mojang has no {component} for {platform}"))?;
    if java.exists()
        && std::fs::read_to_string(&marker).ok().as_deref() == Some(entry.manifest.sha1.as_str())
    {
        return Ok(java);
    }

    let manifest: RuntimeManifest = client
        .get(&entry.manifest.url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut files = Vec::new();
    for (path, f) in &manifest.files {
        let dest = util::safe_join(&home, path)?;
        match f.kind.as_str() {
            "directory" => std::fs::create_dir_all(&dest)?,
            "file" => {
                let raw = f
                    .downloads
                    .as_ref()
                    .and_then(|d| d.get("raw"))
                    .context("runtime file without download")?;
                let ok = std::fs::metadata(&dest)
                    .map(|m| m.len() == raw.size)
                    .unwrap_or(false)
                    && util::sha1_file(&dest)
                        .map(|h| h == raw.sha1)
                        .unwrap_or(false);
                if !ok {
                    files.push((
                        dest,
                        raw.url.clone(),
                        raw.sha1.clone(),
                        raw.size,
                        f.executable,
                    ));
                }
            }
            "link" => {
                #[cfg(unix)]
                if let Some(t) = &f.target {
                    std::fs::create_dir_all(dest.parent().unwrap())?;
                    let _ = std::fs::remove_file(&dest);
                    std::os::unix::fs::symlink(t, &dest)?;
                }
                #[cfg(not(unix))]
                let _ = &f.target;
            }
            _ => {}
        }
    }
    let total: u64 = files.iter().map(|f| f.3).sum();
    let done = Arc::new(AtomicU64::new(0));
    let results: Vec<Result<()>> =
        futures::stream::iter(files.into_iter().map(|(dest, url, sha1, _, exec)| {
            let done = done.clone();
            let progress = progress.clone();
            let client = client.clone();
            async move {
                let on = |n| progress("java", done.fetch_add(n, Ordering::Relaxed) + n, total);
                util::download(&client, &url, &dest, &Expect::Sha1(sha1), &on).await?;
                #[cfg(unix)]
                if exec {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
                }
                #[cfg(not(unix))]
                let _ = exec;
                Ok(())
            }
        }))
        .buffer_unordered(16)
        .collect()
        .await;
    for r in results {
        r?;
    }
    std::fs::write(&marker, &entry.manifest.sha1)?;
    Ok(java)
}
