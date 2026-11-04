#![cfg_attr(windows, windows_subsystem = "windows")]
#![allow(dead_code)]
mod config;
mod util;
use anyhow::{Context, Result, ensure};
use clap::Parser;
use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
};
const BACKEND: &str = include_str!("admin_backend.py");

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    remote: bool,
    ssh: String,
    python: String,
    publisher: String,
    profile: String,
    pack: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            remote: true,
            ssh: "reviewdesk-mony".into(),
            python: "/home/jan/strata/.venv/bin/python".into(),
            publisher: "/home/jan/Projects/jans-launcher-20261104/target/release/jans-launcher"
                .into(),
            profile: "/home/jan/.local/share/ModrinthApp/profiles/Fabric 26.3".into(),
            pack: "/home/jan/Projects/jans-modpack".into(),
        }
    }
}
impl Settings {
    fn path() -> PathBuf {
        config::data_dir().join("admin-settings.json")
    }
    fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    fn save(&self) -> Result<()> {
        util::write_atomic(&Self::path(), &serde_json::to_vec_pretty(self)?)
    }
}
#[derive(Parser)]
#[command(
    version,
    about = "Jan's pack publisher. Players only need jans-modpack-updater."
)]
struct Args {
    /// Run a read-only publication preview using saved admin settings.
    #[arg(long)]
    preview: bool,
    /// JSON settings to use instead of saved settings (no publishing on startup).
    #[arg(long)]
    settings: Option<PathBuf>,
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}
fn backend(
    settings: &Settings,
    action: &str,
    version: &str,
    notes: &str,
    token: Option<&str>,
    emit: impl Fn(String),
) -> Result<()> {
    let mut cmd = if settings.remote {
        ensure!(
            !settings.ssh.is_empty()
                && !settings.ssh.starts_with('-')
                && settings
                    .ssh
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.@:-".contains(&c)),
            "Use an SSH host/alias without extra arguments"
        );
        let mut cmd = Command::new("ssh");
        cmd.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "--",
            &settings.ssh,
        ]);
        cmd.arg(format!(
            "{} -u -c {}",
            quote(&settings.python),
            quote(BACKEND)
        ));
        cmd
    } else {
        let mut cmd = Command::new(&settings.python);
        cmd.args(["-u", "-c", BACKEND]);
        cmd
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Cannot start admin helper. Check Python/SSH settings")?;
    let stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if text.len() < 16000 {
                text.push_str(&line);
                text.push('\n');
            }
        }
        text
    });
    let request = serde_json::json!({"action":action,"profile":settings.profile,"pack":settings.pack,"publisher":settings.publisher,"version":version,"notes":notes,"token":token});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request)?)?;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        emit(line?);
    }
    let code = child.wait()?;
    let error = errors.join().unwrap_or_default();
    ensure!(
        code.success(),
        "Admin operation did not complete ({}). {}",
        code,
        error
    );
    Ok(())
}
enum Event {
    Line(String),
    Done(Result<()>),
}
struct App {
    settings: Settings,
    version: String,
    notes: String,
    token: Option<String>,
    busy: bool,
    rx: Option<Receiver<Event>>,
    logs: Vec<String>,
    status: String,
    error: Option<String>,
    recovery: String,
}
impl App {
    fn start(&mut self, ctx: &egui::Context, action: &str) {
        if let Err(e) = self.settings.save() {
            self.error = Some(format!("{e:#}"));
            return;
        }
        let settings = self.settings.clone();
        let version = self.version.clone();
        let notes = self.notes.clone();
        let token = self.token.clone();
        let action = action.to_string();
        self.busy = true;
        self.error = None;
        self.status = "Working on Mony…".into();
        if action == "preview" {
            self.token = None;
            self.logs.clear();
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = backend(
                &settings,
                &action,
                &version,
                &notes,
                token.as_deref(),
                |line| {
                    let _ = tx.send(Event::Line(line));
                    ctx.request_repaint();
                },
            );
            let _ = tx.send(Event::Done(result));
            ctx.request_repaint();
        });
    }
    fn messages(&mut self) {
        let events: Vec<_> = self
            .rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Done(result) => {
                    self.busy = false;
                    if let Err(e) = result {
                        self.error.get_or_insert_with(|| format!("{e:#}"));
                    }
                    self.rx = None;
                }
                Event::Line(line) => {
                    if let Ok(item) = serde_json::from_str::<serde_json::Value>(&line) {
                        if item["type"] == "result" {
                            self.status = item["message"]
                                .as_str()
                                .unwrap_or("Operation finished")
                                .into();
                            if item["ok"] == false {
                                self.error = Some(self.status.clone());
                            }
                            if item["action"] == "preview" && item["ok"] == true {
                                self.token = item["token"].as_str().map(str::to_string);
                            }
                            if item["action"] == "published" {
                                self.token = None;
                                self.recovery.clear();
                            }
                            if let Some(phase) = item["recovery"]["phase"].as_str() {
                                self.recovery = phase.into();
                            }
                            if let Some(log) = item["log"].as_str() {
                                self.logs.push(format!("Full log on source machine: {log}"));
                            }
                        } else if let Some(text) = item["text"].as_str() {
                            self.logs.push(text.into());
                        }
                    } else {
                        self.logs.push(line);
                    }
                    if self.logs.len() > 500 {
                        self.logs.drain(..self.logs.len() - 500);
                    }
                }
            }
        }
    }
}
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        self.messages();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(20,23,28)).inner_margin(24)).show(ui,|ui|{
            ui.heading(RichText::new("Jan's Pack Admin").size(30.0));ui.label("Prepare on Mony. Publish once. Players update automatically.");ui.add_space(12.0);
            let mut changed=false;
            egui::CollapsingHeader::new("Source and GitHub publishing setup").show(ui,|ui|{ui.add_enabled_ui(!self.busy,|ui|{
                changed|=ui.checkbox(&mut self.settings.remote,"Work on Mony over SSH").changed();
                egui::Grid::new("admin-settings").num_columns(2).spacing([12.0,8.0]).show(ui,|ui|{
                    for(label,value)in[("SSH alias",&mut self.settings.ssh),("Python 3.11+",&mut self.settings.python),("Publisher executable",&mut self.settings.publisher),("Live profile folder",&mut self.settings.profile),("Pack repository folder",&mut self.settings.pack)]{ui.label(label);changed|=ui.add(egui::TextEdit::singleline(value).desired_width(650.0)).changed();ui.end_row();}
                });
                ui.small("SSH uses your existing keys and verified host. GitHub authentication stays on the source machine (gh auth login). Players need neither Git nor GitHub accounts.");
            });});
            ui.add_enabled_ui(!self.busy,|ui|{
                ui.horizontal(|ui|{ui.label("Version (blank = automatic)");changed|=ui.text_edit_singleline(&mut self.version).changed();});
                ui.label("Release notes for your friends");changed|=ui.add(egui::TextEdit::multiline(&mut self.notes).desired_rows(3).desired_width(f32::INFINITY).hint_text("What changed in this update?")).changed();
            });
            if changed {self.token=None;}
            ui.horizontal(|ui|{
                if ui.add_enabled(!self.busy,egui::Button::new("1. Preview changes")).clicked(){self.start(ui.ctx(),"preview");}
                if ui.add_enabled(!self.busy&&self.token.is_some(),egui::Button::new("2. Publish update").fill(Color32::from_rgb(25,115,75))).clicked(){self.start(ui.ctx(),"publish");}
                if ui.add_enabled(!self.busy,egui::Button::new("Retry push only")).clicked(){self.start(ui.ctx(),"retry");}
                if self.recovery=="prepared"&&ui.add_enabled(!self.busy,egui::Button::new("Retry prepared commit & push")).clicked(){self.start(ui.ctx(),"resume");}
            });
            ui.small("Publish uploads assets, stages only generated pack files, commits, then pushes main. No force push. Existing player configs stay unchanged.");
            ui.separator();if self.busy{ui.spinner();}ui.label(&self.status);if let Some(error)=&self.error{ui.colored_label(Color32::LIGHT_RED,error);}
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui,|ui|{for line in &self.logs{ui.monospace(line);}});
        });
    }
}
fn main() -> Result<()> {
    let args = Args::parse();
    let settings = if let Some(path) = args.settings {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        Settings::load()
    };
    if args.preview {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
        return backend(&settings, "preview", "", "", None, |line| {
            println!("{line}")
        });
    }
    let app = App {
        settings,
        version: String::new(),
        notes: String::new(),
        token: None,
        busy: false,
        rx: None,
        logs: vec![],
        status: "Preview your source profile before publishing an update.".into(),
        error: None,
        recovery: String::new(),
    };
    eframe::run_native(
        "Jan's Pack Admin",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1000.0, 780.0])
                .with_min_inner_size([780.0, 600.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
