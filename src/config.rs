//! App settings and on-disk layout.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where the pack comes from unless the settings say otherwise. Can be baked in at build time with
/// `JANS_PACK_SOURCE=github:owner/repo@branch`.
pub const DEFAULT_PACK_SOURCE: &str = match option_env!("JANS_PACK_SOURCE") {
    Some(s) => s,
    None => "github:bozmund/jans-modpack@main",
};

/// Azure application (client) id used for Microsoft login. Baked in with `JANS_MSA_CLIENT_ID`, or
/// set in settings.json.
pub const DEFAULT_MSA_CLIENT_ID: Option<&str> = option_env!("JANS_MSA_CLIENT_ID");

/// GitHub repository the app updates itself from.
pub const APP_REPO: &str = match option_env!("JANS_APP_REPO") {
    Some(s) => s,
    None => "bozmund/jans-launcher",
};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Lang {
    #[default]
    Hr,
    En,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub pack_source: String,
    /// Instance (game) folder. None = `<data>/instance`. Can point at a Modrinth/Prism profile.
    pub instance_dir: Option<PathBuf>,
    pub memory_mb: u32,
    pub language: Lang,
    pub extra_jvm_args: String,
    /// Use this Java instead of Mojang's runtime (needed on NixOS without nix-ld).
    pub java_path: Option<PathBuf>,
    pub msa_client_id: Option<String>,
    pub check_app_updates: bool,
    pub imported_from: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            pack_source: DEFAULT_PACK_SOURCE.to_string(),
            instance_dir: None,
            memory_mb: 8192,
            language: Lang::Hr,
            extra_jvm_args: String::new(),
            java_path: None,
            msa_client_id: None,
            check_app_updates: true,
            imported_from: None,
        }
    }
}

/// The launcher's data folder: settings, shared game files (libraries, assets, Java) and the default
/// instance. `JANS_LAUNCHER_HOME` overrides it (used by tests).
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("JANS_LAUNCHER_HOME") {
        return PathBuf::from(p);
    }
    directories::ProjectDirs::from("", "", "jans-launcher")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("jans-launcher-data"))
}

impl Settings {
    pub fn path() -> PathBuf {
        data_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        crate::util::write_atomic(&Self::path(), &serde_json::to_vec_pretty(self)?)
    }

    pub fn instance_dir(&self) -> PathBuf {
        self.instance_dir
            .clone()
            .unwrap_or_else(|| data_dir().join("instance"))
    }

    pub fn client_id(&self) -> Option<String> {
        self.msa_client_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                DEFAULT_MSA_CLIENT_ID
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            })
    }
}
