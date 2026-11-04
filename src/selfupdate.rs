//! Updating the launcher itself from its GitHub releases.

use crate::util;
use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct AppRelease {
    pub version: String,
    pub notes: String,
    binary_url: String,
    sums_url: Option<String>,
    asset_name: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Name of the release asset for this platform (matches .github/workflows/release.yml).
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("jans-launcher-windows-x86_64.exe"),
        ("linux", "x86_64") => Some("jans-launcher-linux-x86_64"),
        _ => None,
    }
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split(['.', '-'])
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

/// Returns the newest release if it is newer than this build.
pub async fn check(client: &reqwest::Client, repo: &str) -> Result<Option<AppRelease>> {
    // Declarative Nix installations are immutable and must be updated through Nix.
    if std::env::current_exe()
        .ok()
        .is_some_and(|p| p.starts_with("/nix/store"))
    {
        return Ok(None);
    }
    let Some(asset) = asset_name() else {
        return Ok(None);
    };
    let rel: GhRelease = client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if !is_newer(&rel.tag_name, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let Some(bin) = rel.assets.iter().find(|a| a.name == asset) else {
        return Ok(None);
    };
    Ok(Some(AppRelease {
        version: rel.tag_name.trim_start_matches('v').to_string(),
        notes: rel.body.unwrap_or_default(),
        binary_url: bin.browser_download_url.clone(),
        sums_url: rel
            .assets
            .iter()
            .find(|a| a.name == "SHA256SUMS")
            .map(|a| a.browser_download_url.clone()),
        asset_name: asset.to_string(),
    }))
}

/// Download, verify against SHA256SUMS, and swap the running executable. The caller restarts.
pub async fn install(client: &reqwest::Client, rel: &AppRelease) -> Result<()> {
    let sums_url = rel
        .sums_url
        .as_ref()
        .context("release has no SHA256SUMS, refusing to update")?;
    let sums = client
        .get(sums_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let want = sums
        .lines()
        .find_map(|l| {
            let (hash, name) = l.split_once(char::is_whitespace)?;
            (name.trim().trim_start_matches('*') == rel.asset_name).then(|| hash.to_lowercase())
        })
        .context("binary missing from SHA256SUMS")?;
    let data = client
        .get(&rel.binary_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    if util::sha256_bytes(&data) != want {
        bail!("downloaded update does not match its checksum");
    }
    let tmp = std::env::current_exe()?.with_extension("new");
    std::fs::write(&tmp, &data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    self_replace::self_replace(&tmp).context("replace executable")?;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

pub fn restart() -> ! {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::is_newer("v0.2.0", "0.1.9"));
        assert!(super::is_newer("0.1.10", "0.1.9"));
        assert!(!super::is_newer("v0.1.0", "0.1.0"));
    }
}
