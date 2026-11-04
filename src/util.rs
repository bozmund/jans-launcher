//! Small shared helpers: HTTP client, hashing, safe paths, atomic writes.

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sha2::Digest;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub const USER_AGENT: &str = concat!(
    "jans-launcher/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/bozmund/jans-launcher)"
);

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

pub fn sha512_file(path: &Path) -> Result<String> {
    hash_file::<sha2::Sha512>(path)
}

pub fn sha1_file(path: &Path) -> Result<String> {
    hash_file::<sha1::Sha1>(path)
}

pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

fn hash_file<D: Digest>(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = D::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Which hash a download is checked against.
#[derive(Clone, Debug)]
pub enum Expect {
    Sha512(String),
    Sha1(String),
    None,
}

/// Download `url` (http(s), file:// or a plain local path) to `dest`, verifying the hash.
/// Writes to a temp file first and renames, so a half-finished download never replaces a good file.
/// `on_bytes` is called with every chunk length for progress reporting.
pub async fn download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expect: &Expect,
    on_bytes: &(dyn Fn(u64) + Sync),
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = dest.with_file_name(format!(
        ".{}.part",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
    ));
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(800 * attempt)).await;
        }
        match download_once(client, url, &tmp, expect, on_bytes).await {
            Ok(()) => {
                replace_file(&tmp, dest)?;
                return Ok(());
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                // A hash mismatch will not fix itself on retry.
                if e.to_string().contains("hash mismatch") {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap()).with_context(|| format!("download {url}"))
}

async fn download_once(
    client: &reqwest::Client,
    url: &str,
    tmp: &Path,
    expect: &Expect,
    on_bytes: &(dyn Fn(u64) + Sync),
) -> Result<()> {
    let mut sha512 = sha2::Sha512::new();
    let mut sha1 = sha1::Sha1::new();
    let mut out = tokio::fs::File::create(tmp).await?;
    if let Some(local) = local_path(url) {
        let data = tokio::fs::read(&local)
            .await
            .with_context(|| format!("read {}", local.display()))?;
        sha512.update(&data);
        sha1.update(&data);
        out.write_all(&data).await?;
        on_bytes(data.len() as u64);
    } else {
        let resp = client.get(url).send().await?.error_for_status()?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            match expect {
                Expect::Sha512(_) => sha512.update(&chunk),
                Expect::Sha1(_) => sha1.update(&chunk),
                Expect::None => {}
            }
            out.write_all(&chunk).await?;
            on_bytes(chunk.len() as u64);
        }
    }
    out.flush().await?;
    drop(out);
    let (got, want) = match expect {
        Expect::Sha512(w) => (hex::encode(sha512.finalize()), w),
        Expect::Sha1(w) => (hex::encode(sha1.finalize()), w),
        Expect::None => return Ok(()),
    };
    if !got.eq_ignore_ascii_case(want) {
        bail!("hash mismatch for {url}: expected {want}, got {got}");
    }
    Ok(())
}

/// `file:///x`, or a plain filesystem path, used for local test packs.
pub fn local_path(url: &str) -> Option<PathBuf> {
    if let Some(rest) = url.strip_prefix("file://") {
        let rest = rest
            .strip_prefix('/')
            .filter(|r| r.as_bytes().get(1) == Some(&b':'))
            .unwrap_or(rest);
        return Some(PathBuf::from(percent_decode(rest)));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return None;
    }
    // Relative manifest URLs are percent-encoded; a local base joined with them must be decoded.
    Some(PathBuf::from(percent_decode(url)))
}

pub fn replace_file(from: &Path, to: &Path) -> Result<()> {
    // rename over an existing file works on both Windows (MoveFileEx REPLACE) and Unix.
    std::fs::rename(from, to)
        .with_context(|| format!("move {} -> {}", from.display(), to.display()))
}

pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp-write");
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    replace_file(&tmp, path)
}

/// Join a manifest-relative path ("mods/x.jar") onto `root`, refusing anything that could escape it.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if rel.is_empty()
        || rel.contains('\\')
        || rel
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        bail!("invalid path in manifest: {rel:?}");
    }
    let mut out = root.to_path_buf();
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                let name = s.to_string_lossy();
                let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
                if name.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c))
                    || name.ends_with(['.', ' '])
                    || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    || (stem.len() == 4
                        && (stem.starts_with("COM") || stem.starts_with("LPT"))
                        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
                {
                    bail!("non-portable path in manifest: {rel:?}");
                }
                out.push(s);
                if let Ok(meta) = std::fs::symlink_metadata(&out) {
                    #[cfg(windows)]
                    let link = {
                        use std::os::windows::fs::MetadataExt;
                        meta.file_attributes() & 0x400 != 0
                    };
                    #[cfg(not(windows))]
                    let link = meta.file_type().is_symlink();
                    if link {
                        bail!(
                            "refusing to follow a link inside the instance: {}",
                            out.display()
                        );
                    }
                }
            }
            _ => bail!("unsafe path in manifest: {rel:?}"),
        }
    }
    Ok(out)
}

/// Percent-encode each segment of a relative path for use in a URL.
pub fn encode_path(rel: &str) -> String {
    rel.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~+".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve a manifest URL: absolute URLs stay, relative ones are joined onto the pack base.
pub fn resolve_url(base: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://") {
        return url.to_string();
    }
    let base = base.trim_end_matches('/');
    format!("{base}/{url}")
}

pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn now_string() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(time::macros::format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second]"
    ))
    .unwrap_or_default()
}

pub fn today() -> String {
    now_string()[..10].to_string()
}

pub fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_escapes() {
        let root = Path::new("/r");
        assert!(safe_join(root, "mods/a.jar").is_ok());
        assert!(safe_join(root, "../x").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "mods/../../x").is_err());
        assert!(safe_join(root, "C:\\x").is_err());
        for path in [
            "C:/x",
            "mods/a.jar:stream",
            "mods/CON.jar",
            "mods/a.jar.",
            "mods//a.jar",
            "mods/./a.jar",
        ] {
            assert!(safe_join(root, path).is_err(), "{path}");
        }
    }

    #[test]
    fn encode_and_decode() {
        assert_eq!(
            encode_path("config/my mod/a#b.json"),
            "config/my%20mod/a%23b.json"
        );
        assert_eq!(percent_decode("a%20b%23"), "a b#");
    }
}
