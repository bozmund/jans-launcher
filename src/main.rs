#![cfg_attr(windows, windows_subsystem = "windows")]

mod auth;
mod catalog;
mod config;
mod gui;
mod import;
mod java;
mod launch;
mod manifest;
mod modrinth_setup;
mod publish;
mod selfupdate;
mod source;
mod sync;
mod transaction;
mod updater_background;
mod updater_profiles;
#[cfg(windows)]
mod updater_tray;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    version,
    about = "Jan's modpack launcher. Run without arguments for the window."
)]
struct Cli {
    #[arg(long, hide = true)]
    background: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect existing copies of Jan's pack without changing them.
    ModrinthProfiles,
    /// Prepare a local native-import archive; never opens a window or edits Modrinth's database.
    ExportModrinth {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        loader: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Inspect the mod catalog without opening the window.
    Mods {
        #[command(subcommand)]
        command: ModsCmd,
    },
    /// Build a new pack version from a profile folder (for the pack author).
    Publish {
        /// Profile to read (the live Modrinth profile).
        #[arg(long)]
        profile: PathBuf,
        /// Pack repository checkout (holds publish.toml, pack.json, overrides/).
        #[arg(long)]
        pack: PathBuf,
        /// Changelog text for this version.
        #[arg(short, long)]
        message: Option<String>,
        /// Read the changelog text from a file.
        #[arg(long, conflicts_with = "message")]
        message_file: Option<PathBuf>,
        /// Version label (default: today's date).
        #[arg(long)]
        version: Option<String>,
        /// Copy own jars into <pack>/assets instead of uploading to GitHub.
        #[arg(long)]
        local_assets: bool,
        /// Only show what would change.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show what an update would change.
    Check {
        #[arg(long)]
        instance: Option<PathBuf>,
        #[arg(long)]
        source: Option<String>,
    },
    /// Update the instance to the newest pack version.
    Update {
        #[arg(long)]
        instance: Option<PathBuf>,
        #[arg(long)]
        source: Option<String>,
    },
    /// Install the game files and start Minecraft with the instance.
    Play {
        #[arg(long)]
        instance: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ModsCmd {
    /// Explicitly install a mod and required dependencies into a selected instance.
    Install {
        project: String,
        #[arg(long)]
        instance: PathBuf,
        #[arg(long, default_value = "26.3")]
        game: String,
        #[arg(long)]
        confirm: bool,
    },
    Search {
        query: String,
        #[arg(long, default_value = "26.3")]
        game: String,
    },
    List {
        #[arg(long)]
        instance: Option<PathBuf>,
    },
    Updates {
        #[arg(long)]
        instance: Option<PathBuf>,
        #[arg(long, default_value = "26.3")]
        game: String,
    },
    Preview {
        project: String,
        #[arg(long)]
        instance: Option<PathBuf>,
        #[arg(long, default_value = "26.3")]
        game: String,
    },
}

fn main() {
    let cli = Cli::parse_from(std::env::args_os());
    if cli.background {
        if let Err(e) = updater_background::run() {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    let Some(cmd) = cli.command else {
        if let Err(e) = gui::run() {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return;
    };
    attach_console();
    let rt = tokio::runtime::Runtime::new().expect("tokio");
    if let Err(e) = rt.block_on(run_cli(cmd)) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn apply(
    client: &reqwest::Client,
    path: &std::path::Path,
    changes: &sync::Changes,
    progress: sync::ProgressFn,
) -> Result<()> {
    updater_profiles::from_directory(path)?.compatible(&changes.manifest)?;
    sync::apply(client, path, changes, progress).await
}

/// The exe is a GUI app on Windows; when started from a terminal, print there.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn cli_progress() -> sync::ProgressFn {
    let last = std::sync::Mutex::new((String::new(), 0u64));
    Arc::new(move |what: &str, done: u64, total: u64| {
        let pct = if total == 0 { 100 } else { done * 100 / total };
        let mut l = last.lock().unwrap();
        if l.0 != what || pct >= l.1 + 10 || (pct == 100 && l.1 != 100) {
            println!(
                "  {what}: {pct}% ({} / {})",
                util::human_bytes(done),
                util::human_bytes(total)
            );
            *l = (what.to_string(), pct);
        }
    })
}

async fn run_cli(cmd: Cmd) -> Result<()> {
    let settings = config::Settings::load();
    let client = util::http();
    match cmd {
        Cmd::Mods { command } => {
            match command {
                ModsCmd::Install {
                    project,
                    instance,
                    game,
                    confirm,
                } => {
                    anyhow::ensure!(confirm, "Use mods preview first, then --confirm to install");
                    let previous = catalog::inventory(&instance)?;
                    let version = catalog::versions(&client, &project, &game)
                        .await?
                        .into_iter()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("No compatible version"))?;
                    let plan = catalog::prepare(&client, version, &game, previous).await?;
                    catalog::install(&client, &instance, plan, cli_progress()).await?;
                    println!("Mods installed.");
                }
                ModsCmd::Search { query, game } => {
                    let result = catalog::search(&client, &query, &game, 0).await?;
                    for hit in result.hits {
                        println!("{}\t{}\t{}", hit.project_id, hit.title, hit.description);
                    }
                }
                ModsCmd::List { instance } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&catalog::inventory(
                            &instance.unwrap_or_else(|| settings.instance_dir())
                        )?)?
                    );
                }
                ModsCmd::Updates { instance, game } => {
                    let mods =
                        catalog::inventory(&instance.unwrap_or_else(|| settings.instance_dir()))?;
                    for u in catalog::updates(&client, &mods, &game).await? {
                        println!(
                            "{}: {} -> {}",
                            u.installed.name, u.installed.version, u.version.version_number
                        );
                    }
                }
                ModsCmd::Preview {
                    project,
                    instance,
                    game,
                } => {
                    let previous =
                        catalog::inventory(&instance.unwrap_or_else(|| settings.instance_dir()))?;
                    let version = catalog::versions(&client, &project, &game)
                        .await?
                        .into_iter()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("No compatible version"))?;
                    let plan = catalog::prepare(&client, version, &game, previous).await?;
                    for v in plan.versions {
                        println!("{}\t{}\t{}", v.project_id, v.name, v.version_number);
                    }
                }
            }
            Ok(())
        }
        Cmd::ModrinthProfiles => {
            let fetched = source::PackSource::parse(config::DEFAULT_PACK_SOURCE)?
                .fetch(&client)
                .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&modrinth_setup::matching(&fetched.manifest)?)?
            );
            Ok(())
        }
        Cmd::ExportModrinth {
            profile,
            loader,
            output,
        } => {
            let path = modrinth_setup::export(&profile, &loader, &output, Arc::new(|_, _, _| {}))?;
            println!(
                "{}",
                serde_json::json!({"archive":path,"original_preserved":true,"confirmation":"Install through Modrinth"})
            );
            Ok(())
        }
        Cmd::Publish {
            profile,
            pack,
            message,
            message_file,
            version,
            local_assets,
            dry_run,
        } => {
            let message = match message_file {
                Some(f) => Some(std::fs::read_to_string(f)?),
                None => message,
            };
            publish::run(publish::Options {
                profile,
                pack,
                message,
                version,
                local_assets,
                dry_run,
            })
            .await
        }
        Cmd::Check { instance, source } => {
            let instance = instance.unwrap_or_else(|| settings.instance_dir());
            let src =
                source::PackSource::parse(source.as_deref().unwrap_or(&settings.pack_source))?;
            let fetched = src.fetch(&client).await?;
            let changes =
                tokio::task::spawn_blocking(move || sync::plan(&instance, fetched)).await??;
            print_changes(&changes);
            Ok(())
        }
        Cmd::Update { instance, source } => {
            let instance = instance.unwrap_or_else(|| settings.instance_dir());
            let src =
                source::PackSource::parse(source.as_deref().unwrap_or(&settings.pack_source))?;
            let fetched = src.fetch(&client).await?;
            let inst = instance.clone();
            let changes = tokio::task::spawn_blocking(move || sync::plan(&inst, fetched)).await??;
            print_changes(&changes);
            if changes.up_to_date() {
                return Ok(());
            }
            sync::apply(&client, &instance, &changes, cli_progress()).await?;
            println!("Updated to {}.", changes.new_version);
            Ok(())
        }
        Cmd::Play { instance } => {
            let instance = instance.unwrap_or_else(|| settings.instance_dir());
            let _lock = transaction::InstanceLock::acquire(&instance)?;
            transaction::ensure_game_stopped(&instance)?;
            transaction::recover(&instance)?;
            let manifest = sync::installed_manifest(&instance).ok_or_else(|| {
                anyhow::anyhow!("instance has no pack installed; run update first")
            })?;
            let account = gui::fresh_account(&client, &settings).await?;
            let opts = launch::LaunchOptions {
                data: config::data_dir(),
                game_dir: instance,
                minecraft: manifest.minecraft.clone(),
                fabric_loader: manifest.fabric_loader.clone(),
                memory_mb: settings.memory_mb,
                extra_jvm_args: settings.extra_jvm_args.clone(),
                java_override: settings.java_path.clone(),
                account,
            };
            let mut child = launch::launch(&client, &opts, cli_progress()).await?;
            println!("Minecraft started (pid {:?}).", child.id());
            let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
            let (mut stdout, mut stderr) = (tokio::io::stdout(), tokio::io::stderr());
            let (a, b) = tokio::join!(
                tokio::io::copy(&mut out, &mut stdout),
                tokio::io::copy(&mut err, &mut stderr)
            );
            a?;
            b?;
            let status = child.wait().await?;
            println!("Minecraft exited: {status}");
            Ok(())
        }
    }
}

fn print_changes(c: &sync::Changes) {
    println!(
        "{}: installed {} -> newest {} (build {})",
        c.pack_name,
        c.installed_version.as_deref().unwrap_or("nothing"),
        c.new_version,
        c.new_build
    );
    for e in &c.changelog {
        println!("\n== {} ({}) ==\n{}", e.version, e.date, e.text.trim());
    }
    for i in &c.added {
        println!("  + {} {}", i.name, i.version.as_deref().unwrap_or(""));
    }
    for (o, n) in &c.updated {
        println!(
            "  ~ {} {} -> {}",
            n.name,
            o.version.as_deref().unwrap_or("?"),
            n.version.as_deref().unwrap_or("?")
        );
    }
    for i in &c.removed {
        println!("  - {}", i.name);
    }
    if c.other_files_changed > 0 {
        println!("  {} other file(s) changed", c.other_files_changed);
    }
    let trash = c
        .actions
        .iter()
        .filter(|a| matches!(a, sync::Action::Trash(_)))
        .count();
    println!(
        "Work: {} download(s) ({}), {} file(s) to trash{}",
        c.actions.len() - trash,
        util::human_bytes(c.download_bytes),
        trash,
        if c.up_to_date() { " - up to date" } else { "" }
    );
}
