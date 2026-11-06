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

#[derive(Clone, Copy, Debug)]
pub enum AppKind {
    Launcher,
    Updater,
}

fn asset_for(kind: AppKind, os: &str, arch: &str) -> Option<&'static str> {
    match (kind, os, arch) {
        (AppKind::Launcher, "windows", "x86_64") => Some("jans-launcher-windows-x86_64.exe"),
        (AppKind::Launcher, "linux", "x86_64") => Some("jans-launcher-linux-x86_64"),
        (AppKind::Updater, "windows", "x86_64") => Some("jans-modpack-updater-windows-x86_64.exe"),
        (AppKind::Updater, "linux", "x86_64") => Some("jans-modpack-updater-linux-x86_64"),
        _ => None,
    }
}

/// Name of the release asset for this platform (matches .github/workflows/release.yml).
pub fn asset_name() -> Option<&'static str> {
    asset_for(AppKind::Launcher, std::env::consts::OS, std::env::consts::ARCH)
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
    check_for(client, repo, AppKind::Launcher).await
}

pub async fn check_for(client: &reqwest::Client, repo: &str, kind: AppKind) -> Result<Option<AppRelease>> {
    // Declarative Nix installations are immutable and must be updated through Nix.
    if std::env::current_exe()
        .ok()
        .is_some_and(|p| p.starts_with("/nix/store"))
    {
        return Ok(None);
    }
    let Some(asset) = asset_for(kind, std::env::consts::OS, std::env::consts::ARCH) else {
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
    select_release(rel, asset, env!("CARGO_PKG_VERSION"))
}

fn select_release(rel: GhRelease, asset: &str, current: &str) -> Result<Option<AppRelease>> {
    if !is_newer(&rel.tag_name, current) {
        return Ok(None);
    }
    let bin = rel.assets.iter().find(|a| a.name == asset)
        .with_context(|| format!("The new release is missing {asset}; retry when publication finishes"))?;
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
    let data = download_verified(client, rel).await?;
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

async fn download_verified(client: &reqwest::Client, rel: &AppRelease) -> Result<Vec<u8>> {
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
    Ok(data.to_vec())
}

pub fn restart() -> ! {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn updater_selects_its_own_asset_on_both_platforms() {
        for (os, expected) in [("windows", "jans-modpack-updater-windows-x86_64.exe"), ("linux", "jans-modpack-updater-linux-x86_64")] {
            assert_eq!(asset_for(AppKind::Updater, os, "x86_64"), Some(expected));
            let release: GhRelease = serde_json::from_value(serde_json::json!({"tag_name":"v0.4.4","assets":[
                {"name":"jans-launcher-windows-x86_64.exe","browser_download_url":"https://example.invalid/launcher"},
                {"name":expected,"browser_download_url":"https://example.invalid/updater"},
                {"name":"SHA256SUMS","browser_download_url":"https://example.invalid/sums"}
            ]})).unwrap();
            let selected = select_release(release, expected, "0.4.3").unwrap().unwrap();
            assert_eq!(selected.asset_name, expected);
            assert!(selected.binary_url.ends_with("/updater"));
            assert_eq!(selected.version, "0.4.4");
        }
    }

    #[test]
    fn missing_updater_asset_is_not_reported_as_up_to_date() {
        let release: GhRelease = serde_json::from_value(serde_json::json!({"tag_name":"v0.4.4","assets":[]})).unwrap();
        assert!(select_release(release, "jans-modpack-updater-windows-x86_64.exe", "0.4.3").is_err());
        let release: GhRelease = serde_json::from_value(serde_json::json!({"tag_name":"v0.4.3","assets":[]})).unwrap();
        assert!(select_release(release, "jans-modpack-updater-windows-x86_64.exe", "0.4.4").unwrap().is_none());
    }

    #[tokio::test]
    async fn updater_download_verifies_the_selected_asset_and_rejects_corruption() {
        use std::io::{Read, Write};
        for corrupt in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let asset = "jans-modpack-updater-windows-x86_64.exe";
            let bytes = b"disposable updater download fixture";
            let checksum = util::sha256_bytes(if corrupt { b"wrong" } else { bytes });
            let server = std::thread::spawn(move || {
                for expected in ["/sums", "/updater"] {
                    let (mut connection, _) = listener.accept().unwrap();
                    connection.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                    let mut buf = [0u8; 4096];
                    let n = connection.read(&mut buf).unwrap();
                    assert!(String::from_utf8_lossy(&buf[..n]).starts_with(&format!("GET {expected} ")));
                    let body = if expected == "/sums" { format!("{checksum}  {asset}\n").into_bytes() } else { bytes.to_vec() };
                    write!(connection, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    connection.write_all(&body).unwrap();
                }
            });
            let release = AppRelease { version:"0.4.4".into(), notes:String::new(), binary_url:format!("{base}/updater"), sums_url:Some(format!("{base}/sums")), asset_name:asset.into() };
            let result = download_verified(&util::http(), &release).await;
            server.join().unwrap();
            if corrupt { assert!(result.unwrap_err().to_string().contains("checksum")); }
            else { assert_eq!(result.unwrap(), bytes); }
        }
    }
    #[test]
    fn versions() {
        assert!(super::is_newer("v0.2.0", "0.1.9"));
        assert!(super::is_newer("0.1.10", "0.1.9"));
        assert!(!super::is_newer("v0.1.0", "0.1.0"));
    }
}
