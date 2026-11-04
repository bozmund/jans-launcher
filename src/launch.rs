//! Installing Minecraft + Fabric and starting the game.

use crate::auth::Account;
use crate::sync::ProgressFn;
use crate::util::{self, Expect};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const VERSION_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const RESOURCES: &str = "https://resources.download.minecraft.net";

pub struct LaunchOptions {
    pub data: PathBuf,
    pub game_dir: PathBuf,
    pub minecraft: String,
    pub fabric_loader: String,
    pub memory_mb: u32,
    pub extra_jvm_args: String,
    /// Use this java instead of Mojang's runtime.
    pub java_override: Option<PathBuf>,
    pub account: Account,
}

/// One downloadable file of the game installation.
struct Job {
    url: String,
    dest: PathBuf,
    sha1: Option<String>,
    size: u64,
}

struct Prepared {
    java: PathBuf,
    classpath: Vec<PathBuf>,
    main_class: String,
    jvm_args: Vec<String>,
    game_args: Vec<String>,
    version_id: String,
    asset_index: String,
    natives: PathBuf,
}

async fn get_json(client: &reqwest::Client, url: &str, cache: &Path) -> Result<Value> {
    match client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => {
            let bytes = r.bytes().await?;
            let v: Value = serde_json::from_slice(&bytes)?;
            util::write_atomic(cache, &bytes)?;
            Ok(v)
        }
        // Offline: fall back to the copy saved last time.
        Err(e) => match std::fs::read(cache) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(_) => Err(e).with_context(|| format!("fetch {url}")),
        },
    }
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "osx",
        _ => "linux",
    }
}

/// Mojang library/argument rules. Features (demo, custom resolution, quick play) are all off.
fn rules_allow(rules: Option<&Value>) -> bool {
    let Some(rules) = rules.and_then(|r| r.as_array()) else {
        return true;
    };
    let mut allowed = false;
    for rule in rules {
        let allow = rule.get("action").and_then(|a| a.as_str()) == Some("allow");
        let mut matches = true;
        if let Some(os) = rule.get("os") {
            if let Some(name) = os.get("name").and_then(|n| n.as_str()) {
                matches &= name == os_name();
            }
            if let Some(arch) = os.get("arch").and_then(|n| n.as_str()) {
                matches &= match arch {
                    "x86" => std::env::consts::ARCH == "x86",
                    "arm64" => std::env::consts::ARCH == "aarch64",
                    _ => true,
                };
            }
        }
        if let Some(features) = rule.get("features").and_then(|f| f.as_object()) {
            matches &= features.values().all(|v| v.as_bool() == Some(false));
        }
        if matches {
            allowed = allow;
        }
    }
    allowed
}

/// "group:artifact:version[:classifier][@ext]" -> relative maven path.
pub fn maven_path(name: &str) -> Option<String> {
    let (coords, ext) = name.split_once('@').unwrap_or((name, "jar"));
    let parts: Vec<&str> = coords.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let (group, artifact, version) = (parts[0], parts[1], parts[2]);
    let file = match parts.get(3) {
        Some(c) => format!("{artifact}-{version}-{c}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!(
        "{}/{artifact}/{version}/{file}",
        group.replace('.', "/")
    ))
}

/// Identity of a library without its version, so Fabric's copies win over vanilla ones.
fn library_key(name: &str) -> String {
    let parts: Vec<&str> = name.split('@').next().unwrap_or(name).split(':').collect();
    match parts.as_slice() {
        [g, a, _, c, ..] => format!("{g}:{a}:{c}"),
        [g, a, ..] => format!("{g}:{a}"),
        _ => name.to_string(),
    }
}

fn collect_args(v: Option<&Value>, out: &mut Vec<String>) {
    let Some(arr) = v.and_then(|a| a.as_array()) else {
        return;
    };
    for a in arr {
        match a {
            Value::String(s) => out.push(s.clone()),
            Value::Object(o) => {
                if !rules_allow(o.get("rules")) {
                    continue;
                }
                match o.get("value") {
                    Some(Value::String(s)) => out.push(s.clone()),
                    Some(Value::Array(vs)) => {
                        out.extend(vs.iter().filter_map(|x| x.as_str().map(str::to_string)))
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

async fn prepare(
    client: &reqwest::Client,
    o: &LaunchOptions,
    progress: ProgressFn,
) -> Result<Prepared> {
    let data = &o.data;
    progress("meta", 0, 1);

    // Vanilla version json.
    let manifest = get_json(
        client,
        VERSION_MANIFEST,
        &data.join("versions").join("version_manifest_v2.json"),
    )
    .await?;
    let entry = manifest["versions"]
        .as_array()
        .and_then(|vs| vs.iter().find(|v| v["id"] == o.minecraft.as_str()))
        .with_context(|| {
            format!(
                "Minecraft {} not found in Mojang's version list",
                o.minecraft
            )
        })?;
    let vdir = data.join("versions").join(&o.minecraft);
    let vanilla = get_json(
        client,
        entry["url"].as_str().context("version url")?,
        &vdir.join(format!("{}.json", o.minecraft)),
    )
    .await?;

    // Fabric profile json.
    let fabric_url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
        o.minecraft, o.fabric_loader
    );
    let fabric_id = format!("fabric-loader-{}-{}", o.fabric_loader, o.minecraft);
    let fabric = get_json(
        client,
        &fabric_url,
        &data
            .join("versions")
            .join(&fabric_id)
            .join(format!("{fabric_id}.json")),
    )
    .await?;

    let mut jobs = Vec::new();
    let libs_dir = data.join("libraries");
    let natives = data.join("natives").join(&o.minecraft);
    let mut classpath = Vec::new();
    let mut native_jars = Vec::new();
    let mut seen = HashSet::new();

    // Fabric first so its versions of shared libraries (e.g. ASM) take precedence.
    for lib in fabric["libraries"].as_array().into_iter().flatten() {
        let name = lib["name"].as_str().context("fabric library name")?;
        if !seen.insert(library_key(name)) {
            continue;
        }
        let rel = maven_path(name).context("bad library name")?;
        let base = lib["url"].as_str().unwrap_or("https://maven.fabricmc.net/");
        let dest = libs_dir.join(&rel);
        jobs.push(Job {
            url: format!("{}/{rel}", base.trim_end_matches('/')),
            dest: dest.clone(),
            sha1: lib["sha1"].as_str().map(str::to_string),
            size: lib["size"].as_u64().unwrap_or(0),
        });
        classpath.push(dest);
    }
    for lib in vanilla["libraries"].as_array().into_iter().flatten() {
        if !rules_allow(lib.get("rules")) {
            continue;
        }
        let name = lib["name"].as_str().context("library name")?;
        if !seen.insert(library_key(name)) {
            continue;
        }
        if let Some(art) = lib.pointer("/downloads/artifact") {
            let rel = art["path"]
                .as_str()
                .map(str::to_string)
                .or_else(|| maven_path(name))
                .context("library path")?;
            let dest = libs_dir.join(&rel);
            jobs.push(Job {
                url: art["url"].as_str().context("library url")?.to_string(),
                dest: dest.clone(),
                sha1: art["sha1"].as_str().map(str::to_string),
                size: art["size"].as_u64().unwrap_or(0),
            });
            if name.contains(":natives-") {
                native_jars.push(dest.clone());
            }
            classpath.push(dest);
        }
    }

    // Client jar.
    let client_dl = &vanilla["downloads"]["client"];
    let client_jar = vdir.join(format!("{}.jar", o.minecraft));
    jobs.push(Job {
        url: client_dl["url"].as_str().context("client url")?.to_string(),
        dest: client_jar.clone(),
        sha1: client_dl["sha1"].as_str().map(str::to_string),
        size: client_dl["size"].as_u64().unwrap_or(0),
    });
    classpath.push(client_jar);

    // Mojang's logging config (`logging.client`) is skipped on purpose: it turns console output into
    // log4j XML for launchers that parse it. The game's built-in config prints plain lines.
    let mut jvm_args = Vec::new();

    // Assets.
    let ai = &vanilla["assetIndex"];
    let asset_index = ai["id"].as_str().context("asset index id")?.to_string();
    let index_path = data
        .join("assets")
        .join("indexes")
        .join(format!("{asset_index}.json"));
    let index_ok = index_path.exists()
        && ai["sha1"].as_str().is_some_and(|s| {
            util::sha1_file(&index_path)
                .map(|h| h == s)
                .unwrap_or(false)
        });
    if !index_ok {
        let url = ai["url"].as_str().context("asset index url")?;
        let expect = ai["sha1"]
            .as_str()
            .map(|s| Expect::Sha1(s.into()))
            .unwrap_or(Expect::None);
        util::download(client, url, &index_path, &expect, &|_| {}).await?;
    }
    let index: Value = serde_json::from_slice(&std::fs::read(&index_path)?)?;
    for obj in index["objects"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        let Some(hash) = obj["hash"].as_str() else {
            continue;
        };
        let rel = format!("{}/{hash}", &hash[..2]);
        jobs.push(Job {
            url: format!("{RESOURCES}/{rel}"),
            dest: data.join("assets").join("objects").join(&rel),
            sha1: Some(hash.to_string()),
            size: obj["size"].as_u64().unwrap_or(0),
        });
    }

    download_all(client, jobs, progress.clone()).await?;
    extract_natives(&native_jars, &natives)?;

    // Java.
    let java = match &o.java_override {
        Some(j) => j.clone(),
        None => {
            let component = vanilla
                .pointer("/javaVersion/component")
                .and_then(|c| c.as_str())
                .unwrap_or("java-runtime-delta");
            crate::java::ensure(client, data, component, progress.clone()).await?
        }
    };

    let mut game_args = Vec::new();
    collect_args(vanilla.pointer("/arguments/jvm"), &mut jvm_args);
    collect_args(fabric.pointer("/arguments/jvm"), &mut jvm_args);
    collect_args(vanilla.pointer("/arguments/game"), &mut game_args);
    collect_args(fabric.pointer("/arguments/game"), &mut game_args);
    let main_class = fabric["mainClass"]
        .as_str()
        .or(vanilla["mainClass"].as_str())
        .context("main class")?
        .to_string();

    Ok(Prepared {
        java,
        classpath,
        main_class,
        jvm_args,
        game_args,
        version_id: fabric_id,
        asset_index,
        natives,
    })
}

async fn download_all(
    client: &reqwest::Client,
    jobs: Vec<Job>,
    progress: ProgressFn,
) -> Result<()> {
    // Skip files that are already there (size check; sha1 only when the size is unknown).
    let missing: Vec<Job> = jobs
        .into_iter()
        .filter(|j| match std::fs::metadata(&j.dest) {
            Ok(m) if j.size > 0 => m.len() != j.size,
            Ok(_) => match &j.sha1 {
                Some(s) => util::sha1_file(&j.dest).map(|h| &h != s).unwrap_or(true),
                None => false,
            },
            Err(_) => true,
        })
        .collect();
    let total: u64 = missing.iter().map(|j| j.size).sum();
    let done = Arc::new(AtomicU64::new(0));
    let results: Vec<Result<()>> = futures::stream::iter(missing.into_iter().map(|j| {
        let done = done.clone();
        let progress = progress.clone();
        let client = client.clone();
        async move {
            let on = |n| progress("game", done.fetch_add(n, Ordering::Relaxed) + n, total);
            let expect = j.sha1.clone().map(Expect::Sha1).unwrap_or(Expect::None);
            util::download(&client, &j.url, &j.dest, &expect, &on).await
        }
    }))
    .buffer_unordered(16)
    .collect()
    .await;
    let errors: Vec<String> = results
        .into_iter()
        .filter_map(|r| r.err())
        .map(|e| format!("{e:#}"))
        .collect();
    if !errors.is_empty() {
        bail!(
            "{} game file(s) failed to download:\n{}",
            errors.len(),
            errors.join("\n")
        );
    }
    Ok(())
}

fn extract_natives(jars: &[PathBuf], dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for jar in jars {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(jar)?)?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i)?;
            let name = f.name().to_string();
            if f.is_dir() || name.starts_with("META-INF") {
                continue;
            }
            if ![".dll", ".so", ".dylib", ".jnilib"]
                .iter()
                .any(|e| name.ends_with(e))
            {
                continue;
            }
            let file_name = Path::new(&name).file_name().context("native name")?;
            let dest = dir.join(file_name);
            if dest.exists() {
                continue;
            }
            let mut out = std::fs::File::create(&dest)?;
            std::io::copy(&mut f, &mut out)?;
        }
    }
    Ok(())
}

/// Install everything needed and start the game. Returns the running process.
pub async fn launch(
    client: &reqwest::Client,
    o: &LaunchOptions,
    progress: ProgressFn,
) -> Result<tokio::process::Child> {
    let p = prepare(client, o, progress).await?;
    std::fs::create_dir_all(&o.game_dir)?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = p
        .classpath
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(sep);
    let vars: HashMap<&str, String> = HashMap::from([
        ("auth_player_name", o.account.name.clone()),
        ("version_name", p.version_id.clone()),
        ("game_directory", o.game_dir.to_string_lossy().into_owned()),
        (
            "assets_root",
            o.data.join("assets").to_string_lossy().into_owned(),
        ),
        (
            "game_assets",
            o.data.join("assets").to_string_lossy().into_owned(),
        ),
        ("assets_index_name", p.asset_index.clone()),
        ("auth_uuid", o.account.uuid.clone()),
        ("auth_access_token", o.account.access_token.clone()),
        ("auth_session", o.account.access_token.clone()),
        ("auth_xuid", o.account.xuid.clone()),
        ("clientid", String::new()),
        ("user_type", o.account.user_type.clone()),
        ("user_properties", "{}".into()),
        ("version_type", "release".into()),
        (
            "natives_directory",
            p.natives.to_string_lossy().into_owned(),
        ),
        (
            "library_directory",
            o.data.join("libraries").to_string_lossy().into_owned(),
        ),
        ("classpath_separator", sep.into()),
        ("classpath", cp),
        ("launcher_name", "jans-launcher".into()),
        ("launcher_version", env!("CARGO_PKG_VERSION").into()),
    ]);
    let subst = |s: &str| {
        let mut out = s.to_string();
        for (k, v) in &vars {
            out = out.replace(&format!("${{{k}}}"), v);
        }
        out
    };

    let mut args: Vec<String> = vec![format!("-Xmx{}M", o.memory_mb)];
    args.extend(o.extra_jvm_args.split_whitespace().map(str::to_string));
    args.extend(p.jvm_args.iter().map(|a| subst(a)));
    args.push(p.main_class.clone());
    args.extend(p.game_args.iter().map(|a| subst(a)));

    let log_dir = o.data.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let shown: Vec<String> = args
        .iter()
        .map(|a| {
            if a == &o.account.access_token {
                "<token>".into()
            } else {
                a.clone()
            }
        })
        .collect();
    std::fs::write(
        log_dir.join("last-launch-command.txt"),
        format!("{}\n{}", p.java.display(), shown.join("\n")),
    )?;

    let mut cmd = tokio::process::Command::new(&p.java);
    cmd.args(&args)
        .current_dir(&o.game_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
        .with_context(|| format!("start {}", p.java.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maven() {
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.20.1").unwrap(),
            "net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar"
        );
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3:natives-linux").unwrap(),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar"
        );
        assert_eq!(library_key("org.ow2.asm:asm:9.8"), "org.ow2.asm:asm");
    }

    #[test]
    fn rules() {
        assert!(rules_allow(None));
        let other = if os_name() == "windows" {
            "linux"
        } else {
            "windows"
        };
        assert!(!rules_allow(Some(
            &json!([{"action":"allow","os":{"name":other}}])
        )));
        assert!(rules_allow(Some(
            &json!([{"action":"allow","os":{"name":os_name()}}])
        )));
        assert!(!rules_allow(Some(
            &json!([{"action":"allow","features":{"is_demo_user":true}}])
        )));
        assert!(rules_allow(Some(
            &json!([{"action":"allow"},{"action":"disallow","os":{"name":other}}])
        )));
    }
}
