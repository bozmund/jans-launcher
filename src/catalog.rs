//! Modrinth catalog and explicit, recoverable installation of optional mods.
use crate::{manifest::FileEntry, sync, transaction, util};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

const API: &str = "https://api.modrinth.com/v2";

#[derive(Clone, Debug, Deserialize)]
pub struct Project {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Search {
    pub hits: Vec<Project>,
    pub total_hits: u64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub date_published: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<Asset>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub filename: String,
    pub url: String,
    pub hashes: HashMap<String, String>,
    pub size: u64,
    #[serde(default)]
    pub primary: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Dependency {
    pub version_id: Option<String>,
    pub project_id: Option<String>,
    pub dependency_type: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub path: String,
    pub name: String,
    pub version: String,
    pub sha512: String,
    pub size: u64,
    pub project: Option<String>,
    pub mod_id: Option<String>,
    pub pack_owned: bool,
}
#[derive(Clone, Debug)]
pub struct AvailableUpdate {
    pub installed: Installed,
    pub version: Version,
}
#[derive(Clone, Debug)]
pub struct InstallPlan {
    pub minecraft: String,
    pub versions: Vec<Version>,
    pub previous: Vec<Installed>,
}

pub async fn search(
    client: &reqwest::Client,
    query: &str,
    game: &str,
    offset: usize,
) -> Result<Search> {
    let facets = serde_json::json!([
        ["project_type:mod"],
        ["categories:fabric"],
        [format!("versions:{game}")]
    ])
    .to_string();
    Ok(client
        .get(format!("{API}/search"))
        .query(&[
            ("query", query.to_owned()),
            ("facets", facets),
            ("limit", "20".into()),
            ("offset", offset.to_string()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn compatible(version: &Version, game: &str) -> bool {
    version.game_versions.iter().any(|v| v == game) && version.loaders.iter().any(|v| v == "fabric")
}

pub async fn versions(client: &reqwest::Client, project: &str, game: &str) -> Result<Vec<Version>> {
    ensure!(
        project
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid project ID"
    );
    let mut versions: Vec<Version> = client
        .get(format!("{API}/project/{project}/version"))
        .query(&[
            ("loaders", "[\"fabric\"]".to_string()),
            ("game_versions", serde_json::json!([game]).to_string()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    versions.retain(|v| compatible(v, game));
    versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    Ok(versions)
}

pub fn inventory(instance: &Path) -> Result<Vec<Installed>> {
    let pack = sync::installed_manifest(instance);
    let known: HashMap<_, _> = pack
        .as_ref()
        .map(|m| m.files.iter().map(|f| (f.path.as_str(), f)).collect())
        .unwrap_or_default();
    let registry: Vec<Installed> =
        std::fs::read(util::safe_join(instance, ".jans-launcher/catalog.json")?)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
    let folder = util::safe_join(instance, "mods")?;
    if !folder.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        if entry.path().extension().is_none_or(|s| s != "jar") {
            continue;
        }
        let rel = format!("mods/{}", entry.file_name().to_string_lossy());
        let path = util::safe_join(instance, &rel)?;
        let hash = util::sha512_file(&path)?;
        let info = crate::publish::fabric_mod_info(&path);
        let packaged = known.get(rel.as_str()).copied();
        let extra = registry.iter().find(|r| r.path == rel && r.sha512 == hash);
        let identity: Option<&FileEntry> = packaged.filter(|f| f.sha512 == hash);
        out.push(Installed {
            name: info
                .as_ref()
                .and_then(|i| i.name.clone())
                .or_else(|| identity.and_then(|f| f.name.clone()))
                .unwrap_or_else(|| entry.file_name().to_string_lossy().into()),
            version: info
                .as_ref()
                .and_then(|i| i.version.clone())
                .unwrap_or_default(),
            sha512: hash,
            size: entry.metadata()?.len(),
            project: extra.and_then(|e| e.project.clone()).or_else(|| {
                identity
                    .filter(|f| f.source == Some(crate::manifest::Source::Modrinth))
                    .and_then(|f| f.project.clone())
            }),
            mod_id: info.map(|i| i.id),
            pack_owned: packaged.is_some(),
            path: rel,
        });
    }
    out.sort_by_key(|m| m.name.to_lowercase());
    Ok(out)
}

pub async fn updates(
    client: &reqwest::Client,
    mods: &[Installed],
    game: &str,
) -> Result<Vec<AvailableUpdate>> {
    let mut result = vec![];
    for batch in mods.chunks(200) {
        let response: HashMap<String,Version>=client.post(format!("{API}/version_files/update"))
            .json(&serde_json::json!({"hashes":batch.iter().map(|m|&m.sha512).collect::<Vec<_>>(),"algorithm":"sha512","loaders":["fabric"],"game_versions":[game]}))
            .send().await?.error_for_status()?.json().await?;
        for m in batch {
            if let Some(v) = response.get(&m.sha512) {
                if compatible(v, game)
                    && !v
                        .files
                        .iter()
                        .any(|f| f.hashes.get("sha512") == Some(&m.sha512))
                {
                    result.push(AvailableUpdate {
                        installed: m.clone(),
                        version: v.clone(),
                    });
                }
            }
        }
    }
    Ok(result)
}

/// Resolve required dependencies, pinned version IDs and cycles before asking for installation.
pub async fn prepare(
    client: &reqwest::Client,
    version: Version,
    game: &str,
    previous: Vec<Installed>,
) -> Result<InstallPlan> {
    let mut pending = vec![version];
    let mut selected: HashMap<String, Version> = HashMap::new();
    let mut incompatible = HashSet::new();
    while let Some(v) = pending.pop() {
        ensure!(
            compatible(&v, game),
            "{} is not compatible with Fabric {game}",
            v.name
        );
        if let Some(other) = selected.get(&v.project_id) {
            ensure!(
                other.id == v.id,
                "Required dependencies disagree on versions for {}",
                v.project_id
            );
            continue;
        }
        ensure!(selected.len() < 64, "Dependency graph exceeds 64 projects");
        for d in &v.dependencies {
            if d.dependency_type == "incompatible" {
                if let Some(project) = &d.project_id {
                    incompatible.insert(project.clone());
                }
                if let Some(id) = &d.version_id {
                    let bad: Version = client
                        .get(format!("{API}/version/{id}"))
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    incompatible.insert(bad.project_id);
                }
                continue;
            }
            if d.dependency_type != "required" {
                continue;
            }
            let dep = if let Some(id) = &d.version_id {
                client
                    .get(format!("{API}/version/{id}"))
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?
            } else if let Some(id) = &d.project_id {
                if previous.iter().any(|m| m.project.as_ref() == Some(id)) {
                    continue;
                }
                if selected.contains_key(id) {
                    continue;
                }
                versions(client, id, game)
                    .await?
                    .into_iter()
                    .next()
                    .context("Required dependency has no compatible version")?
            } else {
                bail!("Required dependency has no identifiable version/project");
            };
            if !previous.iter().any(|m| {
                dep.files
                    .iter()
                    .any(|f| f.hashes.get("sha512") == Some(&m.sha512))
            }) {
                pending.push(dep);
            }
        }
        selected.insert(v.project_id.clone(), v);
    }
    ensure!(
        !selected.keys().any(|p| incompatible.contains(p))
            && !previous
                .iter()
                .any(|m| m.project.as_ref().is_some_and(|p| incompatible.contains(p))),
        "An incompatible dependency is installed or selected"
    );
    let mut versions: Vec<_> = selected.into_values().collect();
    versions.sort_by(|a, b| a.project_id.cmp(&b.project_id));
    for v in &versions {
        asset(v)?;
    }
    Ok(InstallPlan {
        minecraft: game.into(),
        versions,
        previous,
    })
}

fn asset(version: &Version) -> Result<&Asset> {
    let a = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
        .context("Version has no download")?;
    ensure!(
        a.filename.ends_with(".jar") && !a.filename.contains(['/', '\\']),
        "Expected a portable JAR filename"
    );
    util::safe_join(Path::new("x"), &format!("mods/{}", a.filename))?;
    let hash = a.hashes.get("sha512").context("Download lacks SHA512")?;
    ensure!(
        hash.len() == 128 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid SHA512"
    );
    let url = reqwest::Url::parse(&a.url)?;
    ensure!(
        url.scheme() == "https" && url.host_str() == Some("cdn.modrinth.com"),
        "Unexpected mod download host"
    );
    Ok(a)
}

/// One transaction for all dependency files and the local catalog record. No Git mutations.
pub async fn install(
    client: &reqwest::Client,
    instance: &Path,
    plan: InstallPlan,
    progress: sync::ProgressFn,
) -> Result<()> {
    let _lock = transaction::InstanceLock::acquire(instance)?;
    transaction::ensure_game_stopped(instance)?;
    transaction::recover(instance)?;
    let now = inventory(instance)?;
    ensure!(
        now.len() == plan.previous.len()
            && now.iter().all(|m| plan
                .previous
                .iter()
                .any(|old| old.path == m.path && old.sha512 == m.sha512)),
        "Mods changed since preview; prepare the installation again"
    );
    if let Some(m) = sync::installed_manifest(instance) {
        ensure!(
            m.minecraft == plan.minecraft,
            "Minecraft version changed since preview"
        );
    }
    let stage = util::safe_join(instance, ".jans-launcher/catalog-stage")?;
    std::fs::create_dir_all(&stage)?;
    let mut replacements = vec![];
    let mut registry: Vec<Installed> =
        std::fs::read(util::safe_join(instance, ".jans-launcher/catalog.json")?)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
    let mut used = HashSet::new();
    let mut identities = HashSet::new();
    let total = plan
        .versions
        .iter()
        .try_fold(0u64, |sum, v| Ok::<_, anyhow::Error>(sum + asset(v)?.size))?;
    let received = std::sync::atomic::AtomicU64::new(0);
    progress("mod", 0, total);
    for v in &plan.versions {
        let a = asset(v)?;
        let hash = &a.hashes["sha512"];
        let cached = stage.join(hash);
        util::download(
            client,
            &a.url,
            &cached,
            &util::Expect::Sha512(hash.clone()),
            &|n| {
                let done = received.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n;
                progress("mod", done, total);
            },
        )
        .await?;
        ensure!(
            std::fs::metadata(&cached)?.len() == a.size,
            "Mod size mismatch"
        );
        let info = crate::publish::fabric_mod_info(&cached)
            .context("Downloaded file has no Fabric mod identity")?;
        ensure!(
            identities.insert(info.id.clone()),
            "Selected projects contain the same Fabric mod ID"
        );
        let rel = format!("mods/{}", a.filename);
        ensure!(
            used.insert(rel.to_lowercase()),
            "Two dependencies have the same filename"
        );
        let old: Vec<_> = now
            .iter()
            .filter(|m| {
                m.project.as_ref() == Some(&v.project_id) || m.mod_id.as_ref() == Some(&info.id)
            })
            .collect();
        if util::safe_join(instance, &rel)?.exists() {
            ensure!(
                old.iter().any(|m| m.path.eq_ignore_ascii_case(&rel)),
                "Refusing to overwrite an unrelated mod"
            );
        }
        for m in old {
            if !m.path.eq_ignore_ascii_case(&rel) {
                replacements.push((m.path.clone(), None));
            }
        }
        registry.retain(|m| {
            m.project.as_ref() != Some(&v.project_id) && m.mod_id.as_ref() != Some(&info.id)
        });
        registry.push(Installed {
            path: rel.clone(),
            name: info.name.unwrap_or_else(|| v.name.clone()),
            version: v.version_number.clone(),
            sha512: hash.clone(),
            size: a.size,
            project: Some(v.project_id.clone()),
            mod_id: Some(info.id),
            pack_owned: false,
        });
        replacements.push((rel, Some(cached)));
    }
    // A removed path must not be a different selected dependency's destination.
    replacements.retain(|(p, source)| source.is_some() || !used.contains(&p.to_lowercase()));
    let record = stage.join("catalog.json");
    util::write_atomic(&record, &serde_json::to_vec_pretty(&registry)?)?;
    replacements.push((".jans-launcher/catalog.json".into(), Some(record)));
    transaction::commit(instance, &replacements)?;
    progress("mod", total, total);
    // Only our staging files are removed; backups remain in transaction trash.
    std::fs::remove_dir_all(stage)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn version() -> Version {
        serde_json::from_value(serde_json::json!({"id":"v","project_id":"p","name":"Example","version_number":"1","date_published":"2026","game_versions":["26.3"],"loaders":["fabric"],"files":[{"filename":"example.jar","url":"https://cdn.modrinth.com/data/example.jar","size":1,"hashes":{"sha512":"a".repeat(128)}}]})).unwrap()
    }
    #[test]
    fn restrict_versions_and_downloads() {
        let mut v = version();
        assert!(compatible(&v, "26.3"));
        assert!(!compatible(&v, "1.20.1"));
        assert!(asset(&v).is_ok());
        v.files[0].filename = "../evil.jar".into();
        assert!(asset(&v).is_err());
        v.files[0].filename = "fine.jar".into();
        v.files[0].url = "https://example.org/evil.jar".into();
        assert!(asset(&v).is_err());
    }

    #[test]
    fn registry_identity_requires_matching_installed_content() {
        use std::io::Write;
        let root =
            std::env::temp_dir().join(format!("jans-catalog-registry-{}", std::process::id()));
        assert!(!root.exists());
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let jar = root.join("mods/example.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"id":"example","name":"Example","version":"1"}"#)
            .unwrap();
        zip.finish().unwrap();
        let mut record = Installed {
            path: "mods/example.jar".into(),
            name: "Example".into(),
            version: "1".into(),
            sha512: util::sha512_file(&jar).unwrap(),
            size: std::fs::metadata(&jar).unwrap().len(),
            project: Some("project".into()),
            mod_id: Some("example".into()),
            pack_owned: false,
        };
        let meta = root.join(".jans-launcher/catalog.json");
        util::write_atomic(&meta, &serde_json::to_vec(&vec![record.clone()]).unwrap()).unwrap();
        assert_eq!(
            inventory(&root).unwrap()[0].project.as_deref(),
            Some("project")
        );
        record.sha512 = "0".repeat(128);
        util::write_atomic(&meta, &serde_json::to_vec(&vec![record]).unwrap()).unwrap();
        assert!(inventory(&root).unwrap()[0].project.is_none());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let plan = InstallPlan {
            minecraft: "26.3".into(),
            versions: vec![version()],
            previous: vec![],
        };
        let before = util::sha512_file(&jar).unwrap();
        let failure = rt
            .block_on(install(
                &util::http(),
                &root,
                plan,
                std::sync::Arc::new(|_, _, _| {}),
            ))
            .unwrap_err();
        assert!(failure.to_string().contains("Mods changed since preview"));
        assert_eq!(util::sha512_file(&jar).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
