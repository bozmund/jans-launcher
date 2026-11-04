//! The window friends use: check for changes, update, play.

use crate::auth::{self, Account, DeviceCode};
use crate::config::{self, Lang, Settings};
use crate::selfupdate::{self, AppRelease};
use crate::sync::{self, Changes, ProgressFn};
use crate::{import, launch, source, util};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncBufReadExt;

const GREEN: Color32 = Color32::from_rgb(90, 190, 110);
const RED: Color32 = Color32::from_rgb(225, 95, 95);
const AMBER: Color32 = Color32::from_rgb(230, 180, 80);
const MAX_LOG: usize = 2000;

/// Mods are listed by name; shaders and resource packs get their folder as a hint.
fn item_name(i: &sync::Item) -> String {
    match i.folder.as_str() {
        "mods" => i.name.clone(),
        f => format!("{} [{f}]", i.name),
    }
}

pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Jan's Launcher")
            .with_inner_size([820.0, 620.0])
            .with_min_inner_size([560.0, 420.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .unwrap_or_default(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "jans-launcher",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Account ready to play with: offline test name, or the saved Microsoft login (refreshed if needed).
pub async fn fresh_account(client: &reqwest::Client, settings: &Settings) -> Result<Account> {
    if let Ok(name) = std::env::var("JANS_LAUNCHER_OFFLINE") {
        return Ok(Account::offline(&name));
    }
    let acc = Account::load().context("not logged in")?;
    if !acc.needs_refresh() {
        return Ok(acc);
    }
    let id = settings
        .client_id()
        .context("Microsoft login is not configured")?;
    let acc = auth::refresh(client, &id, &acc).await?;
    acc.save()?;
    Ok(acc)
}

enum Msg {
    Changes(Result<Changes>),
    Updated(Result<String>),
    Log(String),
    DeviceCode(DeviceCode),
    LoggedIn(Result<Account>),
    GameStarted,
    GameExited(String),
    LaunchFailed(String),
    AppUpdate(Option<AppRelease>),
    AppUpdated(Result<()>),
    Imported(Result<usize>),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Home,
    Settings,
    Log,
}

#[derive(PartialEq, Clone, Copy)]
enum Busy {
    Checking,
    Updating,
    Launching,
    Playing,
    LoggingIn,
    SelfUpdating,
    Importing,
}

struct App {
    settings: Settings,
    rt: tokio::runtime::Runtime,
    client: reqwest::Client,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    tab: Tab,
    busy: Option<Busy>,
    progress: Arc<Mutex<Option<(String, u64, u64)>>>,
    changes: Option<Changes>,
    status: String,
    error: Option<String>,
    log: Vec<String>,
    account: Option<Account>,
    device_code: Option<DeviceCode>,
    app_update: Option<AppRelease>,
    profiles: Vec<(String, PathBuf)>,
    import_from: Option<PathBuf>,
    play_after_update: bool,
    settings_dirty: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        let (tx, rx) = channel();
        let mut app = App {
            settings: Settings::load(),
            rt: tokio::runtime::Runtime::new().expect("tokio"),
            client: util::http(),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            tab: Tab::Home,
            busy: None,
            progress: Arc::new(Mutex::new(None)),
            changes: None,
            status: String::new(),
            error: None,
            log: Vec::new(),
            account: Account::load(),
            device_code: None,
            app_update: None,
            profiles: import::find_profiles(),
            import_from: None,
            play_after_update: false,
            settings_dirty: false,
        };
        app.import_from = app.profiles.first().map(|p| p.1.clone());
        app.check();
        if app.settings.check_app_updates {
            let (client, tx, ctx) = (app.client.clone(), app.tx.clone(), app.ctx.clone());
            app.rt.spawn(async move {
                let r = selfupdate::check(&client, config::APP_REPO)
                    .await
                    .ok()
                    .flatten();
                let _ = tx.send(Msg::AppUpdate(r));
                ctx.request_repaint();
            });
        }
        app
    }

    fn t(&self, hr: &'static str, en: &'static str) -> &'static str {
        if self.settings.language == Lang::Hr {
            hr
        } else {
            en
        }
    }

    fn sender(&self) -> impl Fn(Msg) + Send + Clone + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |m| {
            let _ = tx.send(m);
            ctx.request_repaint();
        }
    }

    fn progress_fn(&self) -> ProgressFn {
        let (p, ctx) = (self.progress.clone(), self.ctx.clone());
        Arc::new(move |what: &str, done, total| {
            *p.lock().unwrap() = Some((what.to_string(), done, total));
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        })
    }

    fn check(&mut self) {
        self.busy = Some(Busy::Checking);
        self.error = None;
        let send = self.sender();
        let (client, src, inst) = (
            self.client.clone(),
            self.settings.pack_source.clone(),
            self.settings.instance_dir(),
        );
        self.rt.spawn(async move {
            let r = async {
                let fetched = source::PackSource::parse(&src)?.fetch(&client).await?;
                tokio::task::spawn_blocking(move || sync::plan(&inst, fetched)).await?
            }
            .await;
            send(Msg::Changes(r));
        });
    }

    fn update(&mut self) {
        let Some(changes) = self.changes.clone() else {
            return;
        };
        self.busy = Some(Busy::Updating);
        self.error = None;
        let send = self.sender();
        let (client, inst, progress) = (
            self.client.clone(),
            self.settings.instance_dir(),
            self.progress_fn(),
        );
        self.rt.spawn(async move {
            let r = sync::apply(&client, &inst, &changes, progress)
                .await
                .map(|_| changes.new_version.clone());
            send(Msg::Updated(r));
        });
    }

    fn play(&mut self) {
        let inst = self.settings.instance_dir();
        if self.account.is_none()
            && self.settings.client_id().is_none()
            && std::env::var("JANS_LAUNCHER_OFFLINE").is_err()
        {
            if self.settings.instance_dir.is_none() {
                self.tab = Tab::Settings;
                self.error = Some(
                    self.t(
                        "Odaberi postojeći Modrinth profil za ažuriranja.",
                        "Choose your existing Modrinth profile for updates.",
                    )
                    .into(),
                );
                return;
            }
            match open::that("modrinth://") {
                Ok(()) => {
                    self.status = self
                        .t(
                            "Modrinth je otvoren. Pokreni ažurirani profil.",
                            "Modrinth opened. Start your updated profile.",
                        )
                        .into()
                }
                Err(e) => self.error = Some(format!("Modrinth: {e}")),
            }
            self.check();
            return;
        }
        let Some(_manifest) = sync::installed_manifest(&inst) else {
            self.error = Some(
                self.t(
                    "Pack još nije instaliran.",
                    "The pack is not installed yet.",
                )
                .into(),
            );
            return;
        };
        self.busy = Some(Busy::Launching);
        self.error = None;
        let send = self.sender();
        let (client, settings, progress) = (
            self.client.clone(),
            self.settings.clone(),
            self.progress_fn(),
        );
        self.rt.spawn(async move {
            let r: Result<()> = async {
                let _lock = crate::transaction::InstanceLock::acquire(&inst)?;
                crate::transaction::ensure_game_stopped(&inst)?;
                crate::transaction::recover(&inst)?;
                let manifest = sync::installed_manifest(&inst)
                    .context("pack metadata unavailable after recovery")?;
                let account = fresh_account(&client, &settings).await?;
                let opts = launch::LaunchOptions {
                    data: config::data_dir(),
                    game_dir: inst,
                    minecraft: manifest.minecraft.clone(),
                    fabric_loader: manifest.fabric_loader.clone(),
                    memory_mb: settings.memory_mb,
                    extra_jvm_args: settings.extra_jvm_args.clone(),
                    java_override: settings.java_path.clone(),
                    account,
                };
                let mut child = launch::launch(&client, &opts, progress).await?;
                send(Msg::GameStarted);
                let out = child.stdout.take();
                let err = child.stderr.take();
                let s1 = send.clone();
                let s2 = send.clone();
                let a = tokio::spawn(async move {
                    if let Some(o) = out {
                        let mut lines = tokio::io::BufReader::new(o).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            s1(Msg::Log(l));
                        }
                    }
                });
                let b = tokio::spawn(async move {
                    if let Some(e) = err {
                        let mut lines = tokio::io::BufReader::new(e).lines();
                        while let Ok(Some(l)) = lines.next_line().await {
                            s2(Msg::Log(l));
                        }
                    }
                });
                let status = child.wait().await?;
                let _ = tokio::join!(a, b);
                send(Msg::GameExited(status.to_string()));
                Ok(())
            }
            .await;
            if let Err(e) = r {
                send(Msg::LaunchFailed(format!("{e:#}")));
            }
        });
    }

    fn login(&mut self) {
        let Some(id) = self.settings.client_id() else {
            return;
        };
        self.busy = Some(Busy::LoggingIn);
        self.error = None;
        let send = self.sender();
        let client = self.client.clone();
        self.rt.spawn(async move {
            let r = async {
                let code = auth::start_device_login(&client, &id).await?;
                let _ = open::that(&code.verification_uri);
                send(Msg::DeviceCode(code.clone()));
                let acc = auth::finish_device_login(&client, &id, &code).await?;
                acc.save()?;
                Ok(acc)
            }
            .await;
            send(Msg::LoggedIn(r));
        });
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Changes(r) => {
                self.busy = None;
                match r {
                    Ok(c) => {
                        self.status = if c.up_to_date() {
                            format!(
                                "{} {}",
                                self.t("Sve je ažurno:", "Up to date:"),
                                c.new_version
                            )
                        } else {
                            format!(
                                "{} {}",
                                self.t("Dostupna je nova verzija:", "New version available:"),
                                c.new_version
                            )
                        };
                        self.changes = Some(c);
                    }
                    Err(e) => self.fail(self.t("Provjera nije uspjela", "Check failed"), e),
                }
            }
            Msg::Updated(r) => {
                self.busy = None;
                match r {
                    Ok(v) => {
                        self.push_log(format!("updated to {v}"));
                        if self.play_after_update {
                            self.play_after_update = false;
                            self.play();
                        } else {
                            self.check();
                        }
                    }
                    Err(e) => {
                        self.play_after_update = false;
                        self.fail(self.t("Ažuriranje nije uspjelo", "Update failed"), e)
                    }
                }
            }
            Msg::Log(l) => self.push_log(l),
            Msg::DeviceCode(c) => self.device_code = Some(c),
            Msg::LoggedIn(r) => {
                self.busy = None;
                self.device_code = None;
                match r {
                    Ok(a) => self.account = Some(a),
                    Err(e) => self.fail(self.t("Prijava nije uspjela", "Login failed"), e),
                }
            }
            Msg::GameStarted => {
                self.busy = Some(Busy::Playing);
                self.status = self
                    .t("Minecraft je pokrenut.", "Minecraft is running.")
                    .into();
            }
            Msg::GameExited(s) => {
                self.busy = None;
                self.status = format!(
                    "{} ({s})",
                    self.t("Minecraft je zatvoren", "Minecraft closed")
                );
                self.push_log(format!("game exited: {s}"));
            }
            Msg::LaunchFailed(e) => {
                self.busy = None;
                self.error = Some(format!(
                    "{}: {e}",
                    self.t("Pokretanje nije uspjelo", "Launch failed")
                ));
                self.push_log(e);
            }
            Msg::AppUpdate(r) => self.app_update = r,
            Msg::AppUpdated(r) => {
                self.busy = None;
                match r {
                    Ok(()) => selfupdate::restart(),
                    Err(e) => self.fail(
                        self.t(
                            "Ažuriranje launchera nije uspjelo",
                            "Launcher update failed",
                        ),
                        e,
                    ),
                }
            }
            Msg::Imported(r) => {
                self.busy = None;
                match r {
                    Ok(n) => {
                        self.status =
                            format!("{} {n}", self.t("Uvezeno datoteka:", "Files imported:"))
                    }
                    Err(e) => self.fail(self.t("Uvoz nije uspio", "Import failed"), e),
                }
            }
        }
    }

    fn fail(&mut self, what: &str, e: anyhow::Error) {
        let msg = format!("{what}: {e:#}");
        self.push_log(msg.clone());
        self.error = Some(msg);
    }

    fn push_log(&mut self, l: String) {
        self.log.push(l);
        if self.log.len() > MAX_LOG {
            self.log.drain(..self.log.len() - MAX_LOG);
        }
    }

    // ---- UI ----

    fn ui_top(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let name = self
                .changes
                .as_ref()
                .map(|c| c.pack_name.clone())
                .unwrap_or_else(|| "Jan's Launcher".into());
            ui.heading(name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let other = if self.settings.language == Lang::Hr {
                    "EN"
                } else {
                    "HR"
                };
                if ui.small_button(other).clicked() {
                    self.settings.language = if self.settings.language == Lang::Hr {
                        Lang::En
                    } else {
                        Lang::Hr
                    };
                    let _ = self.settings.save();
                }
                let tabs = [
                    (Tab::Log, self.t("Zapisnik", "Log")),
                    (Tab::Settings, self.t("Postavke", "Settings")),
                    (Tab::Home, self.t("Početna", "Home")),
                ];
                for (tab, label) in tabs {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
        });
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui) {
        if let Some(busy) = self.busy {
            let label = match busy {
                Busy::Checking => self.t("Provjeravam…", "Checking…"),
                Busy::Updating => self.t("Ažuriram…", "Updating…"),
                Busy::Launching => self.t("Pripremam igru…", "Preparing the game…"),
                Busy::Playing => self.t("Igra je pokrenuta", "Game is running"),
                Busy::LoggingIn => self.t("Čekam prijavu…", "Waiting for login…"),
                Busy::SelfUpdating => self.t("Ažuriram launcher…", "Updating launcher…"),
                Busy::Importing => self.t("Uvozim…", "Importing…"),
            };
            let p = self.progress.lock().unwrap().clone();
            match (busy, p) {
                (Busy::Updating | Busy::Launching, Some((what, done, total))) if total > 0 => {
                    let what = match what.as_str() {
                        "java" => "Java",
                        "game" => "Minecraft",
                        _ => "",
                    };
                    ui.add(
                        egui::ProgressBar::new(done as f32 / total as f32)
                            .show_percentage()
                            .text(format!(
                                "{label} {what} {} / {}",
                                util::human_bytes(done),
                                util::human_bytes(total)
                            )),
                    );
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(label);
                    });
                }
            }
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(RED));
        } else if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }

    fn ui_home(&mut self, ui: &mut egui::Ui) {
        if let Some(rel) = self.app_update.clone() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} {}",
                        self.t("Nova verzija launchera:", "New launcher version:"),
                        rel.version
                    ))
                    .color(AMBER),
                )
                .on_hover_text(&rel.notes);
                if ui
                    .add_enabled(
                        self.busy.is_none(),
                        egui::Button::new(self.t("Ažuriraj launcher", "Update launcher")),
                    )
                    .clicked()
                {
                    self.busy = Some(Busy::SelfUpdating);
                    let send = self.sender();
                    let client = self.client.clone();
                    self.rt.spawn(async move {
                        send(Msg::AppUpdated(selfupdate::install(&client, &rel).await))
                    });
                }
            });
            ui.separator();
        }

        self.ui_account(ui);
        ui.separator();

        let idle = self.busy.is_none();
        let delegated = self.account.is_none()
            && self.settings.client_id().is_none()
            && std::env::var("JANS_LAUNCHER_OFFLINE").is_err();
        if delegated && self.settings.instance_dir.is_none() {
            ui.label(self.t(
                "Odaberi Modrinth profil koji želiš ažurirati:",
                "Choose the Modrinth profile to update:",
            ));
            for (label, path) in self.profiles.clone() {
                if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                    self.settings.instance_dir = Some(path);
                    if let Err(e) = self.settings.save() {
                        self.error = Some(e.to_string());
                    }
                    self.check();
                }
            }
            if self.profiles.is_empty()
                && ui
                    .button(self.t("Odaberi mapu profila…", "Choose profile folder…"))
                    .clicked()
            {
                self.tab = Tab::Settings;
            }
            return;
        }
        let needs_update = self.changes.as_ref().is_some_and(|c| !c.up_to_date());
        let installed = sync::installed_manifest(&self.settings.instance_dir()).is_some();
        let can_play =
            delegated || self.account.is_some() || std::env::var("JANS_LAUNCHER_OFFLINE").is_ok();
        ui.horizontal(|ui| {
            let big = |s: &str| {
                egui::Button::new(RichText::new(s).size(18.0)).min_size(egui::vec2(150.0, 40.0))
            };
            if ui
                .add_enabled(idle, big(self.t("Provjeri promjene", "Check for changes")))
                .clicked()
            {
                self.check();
            }
            if ui
                .add_enabled(idle && needs_update, big(self.t("Ažuriraj", "Update")))
                .clicked()
            {
                self.update();
            }
            let play_label = if needs_update {
                self.t("Ažuriraj i igraj", "Update & play")
            } else if delegated {
                self.t("Igraj u Modrinthu", "Play in Modrinth")
            } else {
                self.t("Igraj", "Play")
            };
            if ui
                .add_enabled(
                    idle && can_play && (installed || needs_update),
                    big(play_label).fill(Color32::from_rgb(40, 120, 60)),
                )
                .clicked()
            {
                if needs_update {
                    self.play_after_update = true;
                    self.update();
                } else {
                    self.play();
                }
            }
        });
        ui.add_space(6.0);

        let Some(c) = self.changes.clone() else {
            return;
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(format!(
                "{} {}   →   {} {} (build {})",
                self.t("Instalirano:", "Installed:"),
                c.installed_version.as_deref().unwrap_or(self.t("ništa", "nothing")),
                self.t("Najnovije:", "Newest:"),
                c.new_version,
                c.new_build
            ));
            if c.up_to_date() {
                ui.label(RichText::new(self.t("Sve je ažurno.", "Everything is up to date.")).color(GREEN));
            } else {
                ui.label(format!(
                    "{} {}",
                    self.t("Za preuzeti:", "To download:"),
                    util::human_bytes(c.download_bytes)
                ));
            }
            for e in &c.changelog {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{}  ·  {}", e.version, e.date)).strong().size(16.0));
                if e.text.trim().is_empty() {
                    ui.label(RichText::new(self.t("(bez bilješki)", "(no notes)")).weak());
                } else {
                    ui.label(e.text.trim());
                }
            }
            if c.first_install && !c.added.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{} ({})", self.t("Prva instalacija", "First install"), c.added.len())).strong());
                ui.collapsing(self.t("Sadržaj packa", "Pack contents"), |ui| {
                    for i in &c.added {
                        ui.label(format!("{}  {}", i.name, i.version.as_deref().unwrap_or("")));
                    }
                });
                return;
            }
            let section = |ui: &mut egui::Ui, title: &str, n: usize| {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{title} ({n})")).strong());
            };
            if !c.added.is_empty() {
                section(ui, self.t("Dodano", "Added"), c.added.len());
                for i in &c.added {
                    ui.label(RichText::new(format!("+ {}  {}", item_name(i), i.version.as_deref().unwrap_or(""))).color(GREEN));
                }
            }
            if !c.updated.is_empty() {
                section(ui, self.t("Ažurirano", "Updated"), c.updated.len());
                for (o, n) in &c.updated {
                    ui.label(
                        RichText::new(format!(
                            "~ {}  {} → {}",
                            item_name(n),
                            o.version.as_deref().unwrap_or("?"),
                            n.version.as_deref().unwrap_or("?")
                        ))
                        .color(AMBER),
                    );
                }
            }
            if !c.removed.is_empty() {
                section(ui, self.t("Uklonjeno", "Removed"), c.removed.len());
                for i in &c.removed {
                    ui.label(RichText::new(format!("- {}", item_name(i))).color(RED));
                }
            }
            if c.other_files_changed > 0 {
                ui.add_space(8.0);
                ui.label(format!(
                    "{} {}",
                    c.other_files_changed,
                    self.t(
                        "ostalih datoteka (configovi) promijenjeno; tvoji postojeći configovi se ne prepisuju.",
                        "other files (configs) changed; your existing configs are not overwritten."
                    )
                ));
            }
        });
    }

    fn ui_account(&mut self, ui: &mut egui::Ui) {
        if std::env::var("JANS_LAUNCHER_OFFLINE").is_ok() {
            ui.label(RichText::new("Offline test mode").color(AMBER));
            return;
        }
        if let Some(code) = self.device_code.clone() {
            ui.label(self.t(
                "Upiši ovaj kod na Microsoftovoj stranici:",
                "Enter this code on Microsoft's page:",
            ));
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&code.user_code)
                        .size(26.0)
                        .strong()
                        .monospace(),
                );
                if ui.button(self.t("Kopiraj", "Copy")).clicked() {
                    ui.ctx().copy_text(code.user_code.clone());
                }
                if ui.button(self.t("Otvori stranicu", "Open page")).clicked() {
                    let _ = open::that(&code.verification_uri);
                }
            });
            return;
        }
        match &self.account {
            Some(a) => {
                let name = a.name.clone();
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} {name}",
                        self.t("Prijavljen kao", "Logged in as")
                    ));
                    if ui.small_button(self.t("Odjava", "Log out")).clicked() {
                        Account::forget();
                        self.account = None;
                    }
                });
            }
            None if self.settings.client_id().is_some() => {
                if ui
                    .add_enabled(
                        self.busy.is_none(),
                        egui::Button::new(self.t("Prijava s Microsoftom", "Log in with Microsoft")),
                    )
                    .clicked()
                {
                    self.login();
                }
            }
            None => {
                ui.label(RichText::new(self.t(
                    "Microsoft prijava još nije uključena. Ažuriraj pack ovdje, a igraj preko Modrinth Appa (Postavke → mapa instance = tvoj Modrinth profil).",
                    "Microsoft login is not enabled yet. Update the pack here and play through Modrinth App (Settings → instance folder = your Modrinth profile).",
                )).color(AMBER));
                if ui
                    .button(self.t("Otvori Modrinth App", "Open Modrinth App"))
                    .clicked()
                {
                    let _ = open::that("modrinth://");
                }
            }
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        if self.busy.is_some() {
            ui.label(self.t(
                "Pričekaj završetak trenutne radnje prije promjene postavki.",
                "Wait for the current operation before changing settings.",
            ));
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut dirty = false;
            ui.heading(self.t("Mapa instance", "Instance folder"));
            ui.label(self.settings.instance_dir().display().to_string());
            ui.horizontal(|ui| {
                if ui.button(self.t("Odaberi…", "Choose…")).clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_folder()
                {
                    self.settings.instance_dir = Some(p);
                    dirty = true;
                }
                if ui.button(self.t("Zadano", "Default")).clicked() {
                    self.settings.instance_dir = None;
                    dirty = true;
                }
                if ui.button(self.t("Otvori mapu", "Open folder")).clicked() {
                    let d = self.settings.instance_dir();
                    let _ = std::fs::create_dir_all(&d);
                    let _ = open::that(d);
                }
            });
            if !self.profiles.is_empty() {
                egui::ComboBox::from_id_salt("use-profile")
                    .selected_text(self.t("Koristi postojeći profil…", "Use an existing profile…"))
                    .show_ui(ui, |ui| {
                        for (label, path) in self.profiles.clone() {
                            if ui.selectable_label(false, label).clicked() {
                                self.settings.instance_dir = Some(path);
                                dirty = true;
                            }
                        }
                    });
            }

            ui.add_space(10.0);
            ui.heading(self.t("Uvoz iz drugog launchera", "Import from another launcher"));
            ui.label(self.t(
                "Kopira svjetove, options.txt, servers.dat i screenshotove. Ništa postojeće se ne prepisuje.",
                "Copies worlds, options.txt, servers.dat and screenshots. Nothing existing is overwritten.",
            ));
            ui.horizontal(|ui| {
                let sel = self
                    .profiles
                    .iter()
                    .find(|p| Some(&p.1) == self.import_from.as_ref())
                    .map(|p| p.0.clone())
                    .unwrap_or_else(|| "-".into());
                egui::ComboBox::from_id_salt("import-from").selected_text(sel).show_ui(ui, |ui| {
                    for (label, path) in self.profiles.clone() {
                        ui.selectable_value(&mut self.import_from, Some(path), label);
                    }
                });
                let target = self.settings.instance_dir();
                let ok = self.busy.is_none() && self.import_from.as_ref().is_some_and(|f| f != &target);
                if ui.add_enabled(ok, egui::Button::new(self.t("Uvezi", "Import"))).clicked() {
                    let from = self.import_from.clone().unwrap();
                    self.busy = Some(Busy::Importing);
                    self.settings.imported_from = Some(from.clone());
                    dirty = true;
                    let send = self.sender();
                    self.rt.spawn_blocking(move || send(Msg::Imported(import::import(&from, &target))));
                }
            });

            ui.add_space(10.0);
            ui.heading(self.t("Igra", "Game"));
            ui.horizontal(|ui| {
                ui.label(self.t("Memorija (MB):", "Memory (MB):"));
                dirty |= ui.add(egui::Slider::new(&mut self.settings.memory_mb, 2048..=32768).step_by(512.0)).changed();
            });
            if let Some(rec) = self.changes.as_ref().and_then(|c| c.manifest.recommended_memory_mb) {
                ui.label(RichText::new(format!("{} {rec} MB", self.t("Preporučeno:", "Recommended:"))).weak());
            }
            ui.horizontal(|ui| {
                ui.label(self.t("Dodatni JVM argumenti:", "Extra JVM arguments:"));
                dirty |= ui.text_edit_singleline(&mut self.settings.extra_jvm_args).changed();
            });
            ui.horizontal(|ui| {
                ui.label(self.t("Java (prazno = automatski):", "Java (empty = automatic):"));
                let mut s = self.settings.java_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                if ui.text_edit_singleline(&mut s).changed() {
                    self.settings.java_path = (!s.trim().is_empty()).then(|| PathBuf::from(s.trim()));
                    dirty = true;
                }
            });

            ui.add_space(10.0);
            ui.collapsing(self.t("Napredno", "Advanced"), |ui| {
                ui.horizontal(|ui| {
                    ui.label(self.t("Izvor packa:", "Pack source:"));
                    dirty |= ui.text_edit_singleline(&mut self.settings.pack_source).changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Azure client ID:");
                    let mut s = self.settings.msa_client_id.clone().unwrap_or_default();
                    if ui.text_edit_singleline(&mut s).changed() {
                        self.settings.msa_client_id = (!s.trim().is_empty()).then(|| s.trim().to_string());
                        dirty = true;
                    }
                });
                let label = self.t("Provjeravaj nove verzije launchera", "Check for launcher updates");
                dirty |= ui.checkbox(&mut self.settings.check_app_updates, label).changed();
                ui.label(RichText::new(format!("jans-launcher {}  ·  {}", env!("CARGO_PKG_VERSION"), config::data_dir().display())).weak());
            });
            if dirty {
                self.settings_dirty = true;
                if let Err(e) = self.settings.save() {
                    self.error = Some(format!("{e:#}"));
                }
            }
        });
    }

    fn ui_log(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(self.t("Kopiraj sve", "Copy all")).clicked() {
                ui.ctx().copy_text(self.log.join("\n"));
            }
            if ui.button(self.t("Očisti", "Clear")).clicked() {
                self.log.clear();
            }
        });
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for l in &self.log {
                    ui.label(RichText::new(l).monospace().size(11.0));
                }
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        // Settings that change where/what we sync need a fresh check once the user is done editing.
        if self.settings_dirty && self.tab == Tab::Home && self.busy.is_none() {
            self.settings_dirty = false;
            self.changes = None;
            self.check();
        }
        egui::Panel::top("top").show(ui, |ui| self.ui_top(ui));
        egui::Panel::bottom("bottom").show(ui, |ui| self.ui_bottom(ui));
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Home => self.ui_home(ui),
            Tab::Settings => self.ui_settings(ui),
            Tab::Log => self.ui_log(ui),
        });
        if self.busy.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}
