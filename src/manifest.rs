//! The pack manifest (`pack.json`) that lives in the pack's Git repository.

use serde::{Deserialize, Serialize};

pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// Display name of the pack.
    pub name: String,
    /// Monotonically increasing build number; decides what is "newer".
    pub build: u64,
    /// Human version label, e.g. "2026.11.03".
    pub version: String,
    pub minecraft: String,
    pub fabric_loader: String,
    /// Recommended memory for the game in MiB.
    #[serde(default)]
    pub recommended_memory_mb: Option<u32>,
    pub files: Vec<FileEntry>,
    /// Newest first.
    #[serde(default)]
    pub changelog: Vec<ChangelogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative to the instance folder, forward slashes ("mods/foo.jar").
    pub path: String,
    pub sha512: String,
    pub size: u64,
    /// Absolute URL, or relative to the pack base (repo root at the published commit).
    pub url: String,
    #[serde(default)]
    pub policy: Policy,
    /// Display name (mod name), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Version label, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Modrinth project id, or the fabric mod id for own jars. Used to match old and new versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Fabric identity, including CDN files; used for safe first-time profile migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mod_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Always kept identical to the pack.
    #[default]
    Managed,
    /// Copied only when the file does not exist yet (configs the player may tweak).
    Once,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Modrinth,
    Github,
    Repo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangelogEntry {
    pub build: u64,
    pub version: String,
    pub date: String,
    pub text: String,
}

impl FileEntry {
    /// Key that identifies "the same thing" across versions (a mod keeps its project even when the
    /// file name changes).
    pub fn key(&self) -> String {
        match &self.project {
            Some(p) => format!("{}:{}", self.folder(), p),
            None => self.path.clone(),
        }
    }

    pub fn folder(&self) -> &str {
        self.path.split('/').next().unwrap_or("")
    }

    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            self.path
                .rsplit('/')
                .next()
                .unwrap_or(&self.path)
                .to_string()
        })
    }

    /// Mods, shaders and resource packs are listed one by one in the change view; config files are
    /// only counted.
    pub fn is_content(&self) -> bool {
        matches!(
            self.folder(),
            "mods" | "shaderpacks" | "resourcepacks" | "global_packs" | "datapacks"
        )
    }
}

impl Manifest {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.format != FORMAT {
            anyhow::bail!(
                "this pack needs a newer jans-launcher (pack format {}, app supports {FORMAT})",
                self.format
            );
        }
        let mut seen = std::collections::HashSet::new();
        let mut mods = std::collections::HashSet::new();
        for f in &self.files {
            crate::util::safe_join(std::path::Path::new("x"), &f.path)?;
            if !matches!(
                f.folder(),
                "mods"
                    | "config"
                    | "defaultconfigs"
                    | "shaderpacks"
                    | "resourcepacks"
                    | "global_packs"
                    | "datapacks"
            ) {
                anyhow::bail!("pack files cannot manage this location: {}", f.path);
            }
            if !seen.insert(f.path.to_lowercase()) {
                anyhow::bail!("duplicate path in manifest: {}", f.path);
            }
            if f.sha512.len() != 128 || !f.sha512.bytes().all(|b| b.is_ascii_hexdigit()) {
                anyhow::bail!("bad sha512 for {}", f.path);
            }
            if f.url.trim().is_empty() {
                anyhow::bail!("missing download URL for {}", f.path);
            }
            if f.folder() == "mods"
                && let Some(id) = &f.mod_id
            {
                if id.is_empty() || !mods.insert(id) {
                    anyhow::bail!("duplicate or empty Fabric mod identity: {id}");
                }
            }
        }
        Ok(())
    }
}
