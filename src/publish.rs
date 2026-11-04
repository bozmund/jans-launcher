//! `jans-launcher publish`: turn Jan's live profile into a new pack version.
//!
//! Mods found on Modrinth (by sha512) are referenced on the Modrinth CDN. Other jars and large
//! files are uploaded once (by hash) to a GitHub release of the pack repo with `gh`. Small files
//! (configs) are copied into `overrides/` inside the pack repo. Nothing is committed or pushed.

use crate::manifest::{ChangelogEntry, FORMAT, FileEntry, Manifest, Policy, Source};
use crate::util;
use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_CONFIG: &str = r#"# jans-launcher publish settings (lives in the pack repository root)
name = "Jan's Fabric 26.3"
minecraft = "26.3"
fabric_loader = "0.20.1"
recommended_memory_mb = 8192

# GitHub repository of this pack; own jars are uploaded as assets of release `assets_release`.
github_repo = "bozmund/jans-modpack"
assets_release = "files"

# What is shipped from the profile (paths relative to the profile, / separators).
include = [
  "mods/*.jar",
  "config/**",
  "defaultconfigs/**",
  "shaderpacks/*",
  "resourcepacks/*",
  "global_packs/**",
]
exclude = [
  # Machine-local web administration credentials must never be distributed.
  "config/resourceful-config-web.json",
  "config/jei/world/**",
  "**/*.bak*",
  "**/*.disabled",
  "**/*.old",
  "**/*.tmp",
  "**/.DS_Store",
]
# Copied to players only when missing, so their own changes survive updates.
once = ["config/**", "defaultconfigs/**"]
# Non-jar files larger than this go to release assets instead of the Git repo.
asset_min_size_mb = 5
"#;

#[derive(Debug, Deserialize)]
struct PublishConfig {
    name: String,
    minecraft: String,
    fabric_loader: String,
    #[serde(default)]
    recommended_memory_mb: Option<u32>,
    github_repo: String,
    #[serde(default = "default_release")]
    assets_release: String,
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    once: Vec<String>,
    #[serde(default = "default_asset_min")]
    asset_min_size_mb: u64,
}

fn default_release() -> String {
    "files".into()
}
fn default_asset_min() -> u64 {
    5
}

pub struct Options {
    pub profile: PathBuf,
    pub pack: PathBuf,
    pub message: Option<String>,
    pub version: Option<String>,
    /// Put own jars into `<pack>/assets/` instead of uploading (testing / self-hosting).
    pub local_assets: bool,
    pub dry_run: bool,
}

struct Scanned {
    rel: String,
    abs: PathBuf,
    size: u64,
    sha512: String,
}

fn globset(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(
            GlobBuilder::new(p)
                .literal_separator(true)
                .build()
                .with_context(|| format!("bad pattern {p}"))?,
        );
    }
    Ok(b.build()?)
}

pub async fn run(opts: Options) -> Result<()> {
    let cfg_path = opts.pack.join("publish.toml");
    if !cfg_path.exists() {
        if opts.dry_run {
            bail!(
                "publish.toml is missing; dry run wrote nothing. Run publish once to create its template."
            );
        }
        std::fs::create_dir_all(&opts.pack)?;
        std::fs::write(&cfg_path, DEFAULT_CONFIG)?;
        bail!(
            "created {} - check it (name, fabric_loader, github_repo) and run publish again",
            cfg_path.display()
        );
    }
    let cfg: PublishConfig =
        toml::from_str(&std::fs::read_to_string(&cfg_path)?).context("publish.toml")?;
    let include = globset(&cfg.include)?;
    let exclude = globset(&cfg.exclude)?;
    let roots = globset(
        &cfg.include
            .iter()
            .map(|p| p.split('/').next().unwrap_or(p).to_string())
            .collect::<Vec<_>>(),
    )?;
    let once = globset(&cfg.once)?;
    if !opts.profile.is_dir() {
        bail!("profile folder {} does not exist", opts.profile.display());
    }

    // 1. Scan the profile.
    println!("Scanning {} ...", opts.profile.display());
    let mut scanned = Vec::new();
    for e in walkdir::WalkDir::new(&opts.profile)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let Ok(rel) = e.path().strip_prefix(&opts.profile) else {
                return false;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            roots.is_match(rel.split('/').next().unwrap_or("")) && !exclude.is_match(&rel)
        })
    {
        let e = e?;
        if !e.file_type().is_file() {
            continue;
        }
        let rel = e
            .path()
            .strip_prefix(&opts.profile)?
            .to_string_lossy()
            .replace('\\', "/");
        if !include.is_match(&rel) || exclude.is_match(&rel) {
            continue;
        }
        util::safe_join(Path::new("x"), &rel)?;
        scanned.push(Scanned {
            size: e.metadata()?.len(),
            sha512: util::sha512_file(e.path())?,
            abs: e.path().to_path_buf(),
            rel,
        });
    }
    scanned.sort_by(|a, b| a.rel.cmp(&b.rel));
    println!("  {} files", scanned.len());

    // 2. Look jars and zips up on Modrinth.
    let client = util::http();
    let lookup: Vec<&Scanned> = scanned
        .iter()
        .filter(|s| s.rel.ends_with(".jar") || s.rel.ends_with(".zip"))
        .collect();
    println!("Looking up {} jars/zips on Modrinth ...", lookup.len());
    let modrinth = modrinth_lookup(
        &client,
        &lookup.iter().map(|s| s.sha512.clone()).collect::<Vec<_>>(),
    )
    .await?;
    println!("  {} found on Modrinth", modrinth.len());

    // 3. Classify.
    let asset_min = cfg.asset_min_size_mb * 1024 * 1024;
    let mut files = Vec::new();
    let mut assets: Vec<(&Scanned, String)> = Vec::new();
    let mut overrides: Vec<&Scanned> = Vec::new();
    for s in &scanned {
        let policy = if once.is_match(&s.rel) {
            Policy::Once
        } else {
            Policy::Managed
        };
        let mut entry = FileEntry {
            path: s.rel.clone(),
            sha512: s.sha512.clone(),
            size: s.size,
            url: String::new(),
            policy,
            name: None,
            version: None,
            project: None,
            mod_id: if s.rel.ends_with(".jar") {
                fabric_mod_info(&s.abs).map(|m| m.id)
            } else {
                None
            },
            source: None,
        };
        if let Some(m) = modrinth.get(&s.sha512) {
            entry.url = m.url.clone();
            entry.name = Some(m.title.clone());
            entry.version = Some(m.version.clone());
            entry.project = Some(m.project_id.clone());
            entry.source = Some(Source::Modrinth);
        } else if s.rel.ends_with(".jar") || s.size >= asset_min {
            let file_name = s.rel.rsplit('/').next().unwrap();
            let asset = format!("{}-{}", &s.sha512[..16], sanitize_asset_name(file_name));
            entry.url = if opts.local_assets {
                format!("assets/{}", util::encode_path(&asset))
            } else {
                format!(
                    "https://github.com/{}/releases/download/{}/{}",
                    cfg.github_repo, cfg.assets_release, asset
                )
            };
            entry.source = Some(Source::Github);
            if s.rel.ends_with(".jar") {
                if let Some(info) = fabric_mod_info(&s.abs) {
                    entry.name = Some(info.name.unwrap_or_else(|| info.id.clone()));
                    entry.version = info.version;
                    entry.project = Some(info.id);
                }
            }
            assets.push((s, asset));
        } else {
            entry.url = format!("overrides/{}", util::encode_path(&s.rel));
            entry.source = Some(Source::Repo);
            overrides.push(s);
        }
        files.push(entry);
    }
    let asset_bytes: u64 = assets.iter().map(|(s, _)| s.size).sum();
    println!(
        "  Modrinth: {}   own files (release assets): {} ({})   overrides in repo: {}",
        files
            .iter()
            .filter(|f| f.source == Some(Source::Modrinth))
            .count(),
        assets.len(),
        util::human_bytes(asset_bytes),
        overrides.len()
    );

    // 4. Version / changelog.
    let old: Option<Manifest> = std::fs::read(opts.pack.join("pack.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let build = old.as_ref().map(|m| m.build + 1).unwrap_or(1);
    let version = opts.version.clone().unwrap_or_else(|| {
        let today = util::today().replace('-', ".");
        let same_day = old.as_ref().map(|m| {
            m.changelog
                .iter()
                .filter(|c| c.version.starts_with(&today))
                .count()
        });
        match same_day.unwrap_or(0) {
            0 => today,
            n => format!("{today}-{}", n + 1),
        }
    });
    let mut changelog = old
        .as_ref()
        .map(|m| m.changelog.clone())
        .unwrap_or_default();
    changelog.insert(
        0,
        ChangelogEntry {
            build,
            version: version.clone(),
            date: util::today(),
            text: opts.message.clone().unwrap_or_default(),
        },
    );
    let manifest = Manifest {
        format: FORMAT,
        name: cfg.name.clone(),
        build,
        version: version.clone(),
        minecraft: cfg.minecraft.clone(),
        fabric_loader: cfg.fabric_loader.clone(),
        recommended_memory_mb: cfg.recommended_memory_mb,
        files,
        changelog,
    };
    manifest.validate()?;
    print_diff(old.as_ref(), &manifest);

    if opts.dry_run {
        println!("\nDry run: nothing written or uploaded.");
        return Ok(());
    }

    // 5. Own files: upload (or copy) the ones not there yet.
    if opts.local_assets {
        let dir = opts.pack.join("assets");
        std::fs::create_dir_all(&dir)?;
        for (s, name) in &assets {
            let dst = dir.join(name);
            if !dst.exists() {
                std::fs::copy(&s.abs, &dst)?;
            }
            verify_copy(s, &dst)?;
        }
    } else {
        upload_assets(&cfg, &assets)?;
    }

    // 6. Overrides: rebuild the folder from scratch so deleted configs disappear.
    let ov = opts.pack.join("overrides");
    if ov.exists() {
        std::fs::remove_dir_all(&ov)?;
    }
    for s in &overrides {
        let dst = util::safe_join(&ov, &s.rel)?;
        std::fs::create_dir_all(dst.parent().unwrap())?;
        std::fs::copy(&s.abs, &dst)?;
        verify_copy(s, &dst)?;
    }

    // 7. Manifest + human changelog.
    util::write_atomic(
        &opts.pack.join("pack.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::write(opts.pack.join("CHANGELOG.md"), changelog_md(&manifest))?;
    println!("\nWrote pack.json (build {build}, version {version}).");
    println!("Now publish it:");
    println!("  cd {}", opts.pack.display());
    println!("  git add -A && git commit -m \"Pack {version}\" && git push");
    Ok(())
}

struct ModrinthHit {
    url: String,
    title: String,
    version: String,
    project_id: String,
}

async fn modrinth_lookup(
    client: &reqwest::Client,
    hashes: &[String],
) -> Result<HashMap<String, ModrinthHit>> {
    #[derive(Deserialize)]
    struct Version {
        project_id: String,
        version_number: String,
        files: Vec<VFile>,
    }
    #[derive(Deserialize)]
    struct VFile {
        hashes: HashMap<String, String>,
        url: String,
    }
    #[derive(Deserialize)]
    struct Project {
        id: String,
        title: String,
    }
    let mut versions: HashMap<String, Version> = HashMap::new();
    for chunk in hashes.chunks(200) {
        let resp: HashMap<String, Version> = client
            .post("https://api.modrinth.com/v2/version_files")
            .json(&serde_json::json!({ "hashes": chunk, "algorithm": "sha512" }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        versions.extend(resp);
    }
    let ids: Vec<String> = versions
        .values()
        .map(|v| v.project_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut titles = HashMap::new();
    for chunk in ids.chunks(100) {
        let projects: Vec<Project> = client
            .get("https://api.modrinth.com/v2/projects")
            .query(&[("ids", serde_json::to_string(chunk)?)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        titles.extend(projects.into_iter().map(|p| (p.id, p.title)));
    }
    let mut out = HashMap::new();
    for (hash, v) in versions {
        // The CDN file whose sha512 is exactly ours (a version can have several files).
        let Some(file) = v.files.iter().find(|f| {
            f.hashes
                .get("sha512")
                .is_some_and(|h| h.eq_ignore_ascii_case(&hash))
        }) else {
            continue;
        };
        out.insert(
            hash,
            ModrinthHit {
                url: file.url.clone(),
                title: titles
                    .get(&v.project_id)
                    .cloned()
                    .unwrap_or_else(|| v.project_id.clone()),
                version: v.version_number,
                project_id: v.project_id,
            },
        );
    }
    Ok(out)
}

pub(crate) struct FabricInfo {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
}

pub(crate) fn fabric_mod_info(jar: &Path) -> Option<FabricInfo> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(jar).ok()?).ok()?;
    let mut s = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("fabric.mod.json").ok()?, &mut s).ok()?;
    // Some mods put raw newlines inside strings; serde_json rejects those, so retry flattened.
    let v: serde_json::Value = serde_json::from_str(&s)
        .or_else(|_| serde_json::from_str(&s.replace(['\n', '\r'], " ")))
        .ok()?;
    Some(FabricInfo {
        id: v.get("id")?.as_str()?.to_string(),
        name: v.get("name").and_then(|n| n.as_str()).map(str::to_string),
        version: v
            .get("version")
            .and_then(|n| n.as_str())
            .map(str::to_string),
    })
}

fn verify_copy(source: &Scanned, copy: &Path) -> Result<()> {
    if std::fs::metadata(copy)?.len() != source.size || util::sha512_file(copy)? != source.sha512 {
        bail!(
            "{} changed during publishing; nothing should be committed. Retry after the profile is stable.",
            source.rel
        );
    }
    Ok(())
}

/// GitHub mangles unusual characters in asset names; keep names predictable.
fn sanitize_asset_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-+".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn upload_assets(cfg: &PublishConfig, assets: &[(&Scanned, String)]) -> Result<()> {
    let repo = &cfg.github_repo;
    let tag = &cfg.assets_release;
    let gh = |args: &[&str]| Command::new("gh").args(args).output();
    let view = gh(&[
        "release",
        "view",
        tag,
        "--repo",
        repo,
        "--json",
        "assets",
        "-q",
        ".assets[].name",
    ])
    .context("running `gh` (GitHub CLI) - is it installed and logged in?")?;
    let existing: HashSet<String> = if view.status.success() {
        String::from_utf8_lossy(&view.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    } else {
        println!("Creating release '{tag}' in {repo} for pack files ...");
        let out = gh(&[
            "release",
            "create",
            tag,
            "--repo",
            repo,
            "--title",
            "Pack files",
            "--notes",
            "Binary files used by the pack (managed by jans-launcher publish). Do not delete.",
        ])?;
        if !out.status.success() {
            bail!(
                "gh release create failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        HashSet::new()
    };
    let missing: Vec<&(&Scanned, String)> = assets
        .iter()
        .filter(|(_, n)| !existing.contains(n))
        .collect();
    if missing.is_empty() {
        println!("All own files are already uploaded.");
        return Ok(());
    }
    let staging = std::env::temp_dir().join(format!("jans-publish-{}", std::process::id()));
    std::fs::create_dir_all(&staging)?;
    let total: u64 = missing.iter().map(|(s, _)| s.size).sum();
    println!(
        "Uploading {} new file(s), {} ...",
        missing.len(),
        util::human_bytes(total)
    );
    for (i, (s, name)) in missing.iter().enumerate() {
        let staged = staging.join(name);
        std::fs::copy(&s.abs, &staged)?;
        verify_copy(s, &staged)?;
        println!("  [{}/{}] {}", i + 1, missing.len(), name);
        let out = Command::new("gh")
            .args(["release", "upload", tag, "--repo", repo, "--clobber"])
            .arg(&staged)
            .output()?;
        let _ = std::fs::remove_file(&staged);
        if !out.status.success() {
            bail!(
                "upload of {name} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

fn print_diff(old: Option<&Manifest>, new: &Manifest) {
    let Some(old) = old else {
        println!("\nFirst version of the pack.");
        return;
    };
    let old_by_key: HashMap<String, &FileEntry> = old.files.iter().map(|f| (f.key(), f)).collect();
    let new_keys: HashSet<String> = new.files.iter().map(|f| f.key()).collect();
    println!("\nChanges since {}:", old.version);
    let mut any = false;
    for f in new.files.iter().filter(|f| f.is_content()) {
        match old_by_key.get(&f.key()) {
            None => println!(
                "  + {} {}",
                f.display_name(),
                f.version.as_deref().unwrap_or("")
            ),
            Some(o) if o.sha512 != f.sha512 => println!(
                "  ~ {} {} -> {}",
                f.display_name(),
                o.version.as_deref().unwrap_or("?"),
                f.version.as_deref().unwrap_or("?")
            ),
            _ => continue,
        }
        any = true;
    }
    for f in old
        .files
        .iter()
        .filter(|f| f.is_content() && !new_keys.contains(&f.key()))
    {
        println!("  - {}", f.display_name());
        any = true;
    }
    if !any {
        println!("  (no mod / shader / resource pack changes)");
    }
}

fn changelog_md(m: &Manifest) -> String {
    let mut s = format!("# {} changelog\n", m.name);
    for c in &m.changelog {
        s.push_str(&format!(
            "\n## {} (build {}, {})\n\n",
            c.version, c.build, c.date
        ));
        s.push_str(if c.text.trim().is_empty() {
            "_No notes._"
        } else {
            c.text.trim()
        });
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("jans-publish-{stamp}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    #[tokio::test]
    async fn dry_run_without_configuration_does_not_write_anything() {
        let root = fixture();
        let pack = root.join("missing-pack");
        assert!(
            run(Options {
                profile: root.clone(),
                pack: pack.clone(),
                message: None,
                version: None,
                local_assets: true,
                dry_run: true
            })
            .await
            .is_err()
        );
        assert!(!pack.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn publishing_does_not_walk_unselected_development_folders() {
        let root = fixture();
        let profile = root.join("profile");
        let pack = root.join("pack");
        std::fs::create_dir_all(profile.join("config")).unwrap();
        std::fs::create_dir_all(profile.join("local-mods/dev")).unwrap();
        std::os::unix::fs::symlink(
            "/nonexistent/jans-fixture.jar",
            profile.join("local-mods/dev/broken.jar"),
        )
        .unwrap();
        std::fs::write(profile.join("config/example.json"), b"{}").unwrap();
        std::fs::create_dir(&pack).unwrap();
        std::fs::write(pack.join("publish.toml"), DEFAULT_CONFIG).unwrap();
        run(Options {
            profile,
            pack: pack.clone(),
            message: None,
            version: None,
            local_assets: true,
            dry_run: true,
        })
        .await
        .unwrap();
        assert!(!pack.join("pack.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
