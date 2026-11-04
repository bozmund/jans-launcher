//! Fetching the newest manifest from the pack repository.

use crate::manifest::Manifest;
use crate::util;
use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackSource {
    /// `github:owner/repo@branch`
    GitHub {
        owner: String,
        repo: String,
        branch: String,
    },
    /// Any URL or local folder containing pack.json (used for testing and self-hosting).
    Base(String),
}

pub struct Fetched {
    pub manifest: Manifest,
    /// Base that relative URLs in the manifest resolve against.
    pub base: String,
    /// Git commit (or "local") the manifest came from.
    pub revision: String,
}

impl PackSource {
    pub fn parse(s: &str) -> Result<PackSource> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("github:") {
            let (path, branch) = rest.split_once('@').unwrap_or((rest, "main"));
            let Some((owner, repo)) = path.split_once('/') else {
                bail!("expected github:owner/repo[@branch], got {s}");
            };
            return Ok(PackSource::GitHub {
                owner: owner.into(),
                repo: repo.into(),
                branch: branch.into(),
            });
        }
        if s.is_empty() {
            bail!("pack source is empty");
        }
        Ok(PackSource::Base(
            s.trim_end_matches(['/', '\\']).to_string(),
        ))
    }

    pub async fn fetch(&self, client: &reqwest::Client) -> Result<Fetched> {
        let (base, revision) = match self {
            PackSource::GitHub {
                owner,
                repo,
                branch,
            } => {
                let sha = latest_commit(client, owner, repo, branch)
                    .await
                    .context("could not pin the pack to a Git commit; retry Check for changes")?;
                (
                    format!("https://raw.githubusercontent.com/{owner}/{repo}/{sha}"),
                    sha,
                )
            }
            PackSource::Base(b) => (b.clone(), "local".to_string()),
        };
        let url = util::resolve_url(&base, "pack.json");
        let bytes = match util::local_path(&url) {
            Some(p) => std::fs::read(&p).with_context(|| format!("read {}", p.display()))?,
            None => client
                .get(&url)
                .send()
                .await?
                .error_for_status()
                .with_context(|| format!("fetch {url}"))?
                .bytes()
                .await?
                .to_vec(),
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).context("pack.json is not valid")?;
        manifest.validate()?;
        Ok(Fetched {
            manifest,
            base,
            revision,
        })
    }
}

/// Resolve a branch to a commit so every file of one update comes from the same commit.
async fn latest_commit(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Result<String> {
    let sha = client
        .get(format!(
            "https://api.github.com/repos/{owner}/{repo}/commits/{branch}"
        ))
        .header("Accept", "application/vnd.github.sha")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let sha = sha.trim().to_string();
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("unexpected commit response");
    }
    Ok(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sources() {
        assert_eq!(
            PackSource::parse("github:bozmund/jans-modpack").unwrap(),
            PackSource::GitHub {
                owner: "bozmund".into(),
                repo: "jans-modpack".into(),
                branch: "main".into()
            }
        );
        assert_eq!(
            PackSource::parse("github:a/b@dev").unwrap(),
            PackSource::GitHub {
                owner: "a".into(),
                repo: "b".into(),
                branch: "dev".into()
            }
        );
        assert_eq!(
            PackSource::parse("C:\\packs\\x\\").unwrap(),
            PackSource::Base("C:\\packs\\x".into())
        );
    }
}
