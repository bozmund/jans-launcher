//! The window friends use: check for changes, update, play.

use crate::auth::{self, Account, DeviceCode};
use crate::config::{self, Settings};
use crate::selfupdate::{self, AppRelease};
use crate::sync::{self, Changes, ProgressFn};
use crate::{catalog, launch, source, util};
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
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([900.0, 620.0])
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

/// Saved Microsoft account, refreshed if needed. Game access is verified again before launch.
pub async fn fresh_account(client: &reqwest::Client, settings: &Settings) -> Result<Account> {
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
    CatalogSearch(Result<catalog::Search>),
    Inventory(Result<Vec<catalog::Installed>>),
    ModUpdates(Result<Vec<catalog::AvailableUpdate>>),
    ModPlan(Result<catalog::InstallPlan>),
    ModInstalled(Result<()>),
    ModrinthProfile(Result<Option<PathBuf>>),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Home,
    Discover,
    Installed,
    Updates,
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
    Catalog,
    InstallingMod,
    ModrinthProfile,
}

struct App {
    close_to_tray: bool,
    resident_started: bool,
    setup_archive: Option<PathBuf>,
    setup_started: Option<std::time::Instant>,
    setup_handoff: Option<std::time::Instant>,
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
    play_after_update: bool,
    settings_dirty: bool,
    query: String,
    catalog_offset: usize,
    catalog_result: Option<catalog::Search>,
    inventory: Vec<catalog::Installed>,
    mod_updates: Option<Vec<catalog::AvailableUpdate>>,
    mod_plan: Option<catalog::InstallPlan>,
    mod_filter: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        let mut style = (*cc.egui_ctx.style_of(egui::Theme::Dark)).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = Color32::from_rgb(23, 25, 29);
        style.visuals.window_fill = Color32::from_rgb(31, 34, 40);
        style.visuals.extreme_bg_color = Color32::from_rgb(17, 19, 23);
        style.visuals.faint_bg_color = Color32::from_rgb(32, 35, 41);
        style.visuals.selection.bg_fill = Color32::from_rgb(26, 110, 74);
        style.visuals.hyperlink_color = GREEN;
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(42, 46, 54);
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(55, 61, 70);
        style.spacing.item_spacing = egui::vec2(12.0, 10.0);
        style.spacing.button_padding = egui::vec2(16.0, 10.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
        let (tx, rx) = channel();
        let mut app = App {
            close_to_tray: false,
            resident_started: false,
            setup_archive: crate::modrinth_setup::pending_archive(),
            setup_started: None,
            setup_handoff: None,
            settings: Settings::load().single_pack(),
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
            play_after_update: false,
            settings_dirty: false,
            query: String::new(),
            catalog_offset: 0,
            catalog_result: None,
            inventory: vec![],
            mod_updates: None,
            mod_plan: None,
            mod_filter: String::new(),
        };
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
                anyhow::ensure!(
                    fetched.manifest.minecraft == config::PACK_MINECRAFT,
                    "This launcher supports only Jan's Fabric 26.3 pack"
                );
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
        if self.account.is_none() {
            self.tab = Tab::Settings;
            self.error = Some(
                "Sign in with Microsoft for standalone play. See application setup in Settings."
                    .into(),
            );
            return;
        }
        let Some(_manifest) = sync::installed_manifest(&inst) else {
            self.error = Some("The pack is not installed yet.".into());
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
            Msg::CatalogSearch(r) => {
                self.busy = None;
                match r {
                    Ok(result) => self.catalog_result = Some(result),
                    Err(e) => self.fail("Catalog", e),
                }
            }
            Msg::Inventory(r) => {
                self.busy = None;
                match r {
                    Ok(result) => self.inventory = result,
                    Err(e) => self.fail("Installed mods", e),
                }
            }
            Msg::ModUpdates(r) => {
                self.busy = None;
                match r {
                    Ok(result) => self.mod_updates = Some(result),
                    Err(e) => self.fail("Mod updates", e),
                }
            }
            Msg::ModPlan(r) => {
                self.busy = None;
                match r {
                    Ok(result) => self.mod_plan = Some(result),
                    Err(e) => self.fail("Installation preview", e),
                }
            }
            Msg::ModInstalled(r) => {
                self.busy = None;
                self.mod_plan = None;
                match r {
                    Ok(()) => {
                        self.status = "Mods installed.".into();
                        self.load_inventory(false);
                    }
                    Err(e) => self.fail("Mod installation", e),
                }
            }
            Msg::ModrinthProfile(result) => {
                self.busy = None;
                self.setup_started = None;
                match result {
                    Ok(path) => {
                        self.setup_archive = path.clone();
                        self.setup_handoff = Some(std::time::Instant::now());
                        match crate::modrinth_setup::open_installer(path.as_deref()) {
                            Ok(()) => {
                                self.status = if path.is_some() {
                                    "Confirm Install in Modrinth. Your game settings, mods and worlds are copied; the old profile is kept. Keep the migration archive private.".into()
                                } else {
                                    "Your independent profile already exists. Open it in Modrinth to play.".into()
                                }
                            }
                            Err(e) => self.fail("Open Modrinth installer", e),
                        }
                    }
                    Err(e) => self.fail("Create Modrinth profile", e),
                }
            }
            Msg::Changes(r) => {
                self.busy = None;
                match r {
                    Ok(c) => {
                        self.status = if c.up_to_date() {
                            format!("{} {}", "Up to date:", c.new_version)
                        } else {
                            format!("{} {}", "New version available:", c.new_version)
                        };
                        self.changes = Some(c);
                    }
                    Err(e) => self.fail("Check failed", e),
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
                        self.fail("Update failed", e)
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
                    Err(e) => self.fail("Login failed", e),
                }
            }
            Msg::GameStarted => {
                self.busy = Some(Busy::Playing);
                self.status = "Minecraft is running.".into();
            }
            Msg::GameExited(s) => {
                self.busy = None;
                self.status = format!("{} ({s})", "Minecraft closed");
                self.push_log(format!("game exited: {s}"));
            }
            Msg::LaunchFailed(e) => {
                self.busy = None;
                self.error = Some(format!("{}: {e}", "Launch failed"));
                self.push_log(e);
            }
            Msg::AppUpdate(r) => self.app_update = r,
            Msg::AppUpdated(r) => {
                self.busy = None;
                match r {
                    Ok(()) => selfupdate::restart(),
                    Err(e) => self.fail("Launcher update failed", e),
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
            ui.label(
                RichText::new("JAN'S LAUNCHER")
                    .strong()
                    .color(GREEN)
                    .size(17.0),
            );
            ui.separator();
            ui.label("Your Minecraft, in one place");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    self.account
                        .as_ref()
                        .map(|a| a.name.as_str())
                        .unwrap_or("Microsoft account"),
                );
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
            });
        });
    }

    fn ui_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);
        ui.label(RichText::new("LIBRARY").weak().size(11.0));
        for (tab, label) in [
            (Tab::Home, "Overview"),
            (Tab::Installed, "Installed mods"),
            (Tab::Discover, "Discover mods"),
            (Tab::Updates, "Mod updates"),
            (Tab::Log, "Activity & logs"),
            (Tab::Settings, "Settings & account"),
        ] {
            if ui
                .add_sized(
                    [190.0, 40.0],
                    egui::Button::new(label).selected(self.tab == tab),
                )
                .clicked()
            {
                self.tab = tab;
                if self.busy.is_none() {
                    if tab == Tab::Installed {
                        self.load_inventory(false);
                    }
                    if tab == Tab::Updates {
                        self.load_inventory(true);
                    }
                    if tab == Tab::Discover && self.catalog_result.is_none() {
                        self.search_mods();
                    }
                }
            }
        }
        ui.add_space(24.0);
        ui.separator();
        ui.label(RichText::new("Jan's Fabric 26.3").strong());
        ui.label(RichText::new("Your modpack").weak().size(12.0));
        ui.add_space(18.0);
        ui.label(RichText::new("Minecraft 26.3 · Fabric").weak().size(12.0));
        ui.label(
            RichText::new("No 500 MB pack limit")
                .color(GREEN)
                .size(12.0),
        );
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui) {
        if let Some(busy) = self.busy {
            let label = match busy {
                Busy::Checking => "Checking…",
                Busy::Updating => "Updating…",
                Busy::Launching => "Preparing the game…",
                Busy::Playing => "Game is running",
                Busy::LoggingIn => "Waiting for login…",
                Busy::SelfUpdating => "Updating launcher…",
                Busy::Catalog => "Fetching mods…",
                Busy::InstallingMod => "Installing mods…",
                Busy::ModrinthProfile => "Preparing your Modrinth profile…",
            };
            let p = self.progress.lock().unwrap().clone();
            match (busy, p) {
                (
                    Busy::Updating | Busy::Launching | Busy::InstallingMod | Busy::ModrinthProfile,
                    Some((what, done, total)),
                ) if total > 0 => {
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
                    RichText::new(format!("{} {}", "New launcher version:", rel.version))
                        .color(AMBER),
                )
                .on_hover_text(&rel.notes);
                if ui
                    .add_enabled(self.busy.is_none(), egui::Button::new("Update launcher"))
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

        let idle = self.busy.is_none();
        let needs_update = self.changes.as_ref().is_some_and(|c| !c.up_to_date());
        let installed = self
            .changes
            .as_ref()
            .is_some_and(|c| c.installed_version.is_some());
        egui::Frame::new()
            .fill(Color32::from_rgb(29, 44, 37))
            .corner_radius(12)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("JAN'S COLLECTION")
                        .color(GREEN)
                        .strong()
                        .size(12.0),
                );
                ui.add_space(4.0);
                ui.heading(RichText::new("Fabric 26.3").size(34.0));
                ui.label("Your shared modpack. Fetch updates, manage content and launch the game.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            idle && (installed || needs_update),
                            egui::Button::new(RichText::new("Play").strong())
                                .fill(Color32::from_rgb(27, 125, 79))
                                .min_size(egui::vec2(130.0, 44.0)),
                        )
                        .clicked()
                    {
                        if self.account.is_none() {
                            self.tab = Tab::Settings;
                        } else if needs_update {
                            self.play_after_update = true;
                            self.update();
                        } else {
                            self.play();
                        }
                    }
                    if ui
                        .add_enabled(
                            idle,
                            egui::Button::new("Fetch · Check updates")
                                .min_size(egui::vec2(160.0, 44.0)),
                        )
                        .clicked()
                    {
                        self.check();
                    }
                    if ui
                        .add_enabled(
                            idle && needs_update,
                            egui::Button::new("Download update").min_size(egui::vec2(150.0, 44.0)),
                        )
                        .clicked()
                    {
                        self.update();
                    }
                });
                ui.add_space(6.0);
                if self.account.is_none() {
                    ui.label(
                        RichText::new("Sign in with Microsoft in Settings to play.").color(AMBER),
                    );
                }
            });
        ui.add_space(12.0);
        ui.label("Play through Modrinth while standalone sign-in approval is pending.");
        let cooldown = self
            .setup_handoff
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2));
        if ui
            .add_enabled(
                idle && self.setup_started.is_none() && !cooldown,
                egui::Button::new(if self.busy == Some(Busy::ModrinthProfile) {
                    "Preparing profile—please wait…"
                } else if self.setup_archive.is_some() {
                    "Continue installation in Modrinth"
                } else {
                    "Create Modrinth profile"
                })
                .min_size(egui::vec2(220.0, 44.0)),
            )
            .clicked()
        {
            if let Some(path) = &self.setup_archive {
                self.setup_handoff = Some(std::time::Instant::now());
                if let Err(e) = crate::modrinth_setup::open_installer(Some(path)) {
                    self.fail("Open pending installer", e);
                }
            } else {
                self.busy = Some(Busy::ModrinthProfile);
                self.setup_started = Some(std::time::Instant::now());
                self.error = None;
                *self.progress.lock().unwrap() = Some(("Starting profile setup…".into(), 0, 0));
                let client = self.client.clone();
                let send = self.sender();
                let state = self.progress.clone();
                let ctx = self.ctx.clone();
                let progress: ProgressFn = Arc::new(move |phase, done, total| {
                    *state.lock().unwrap() = Some((phase.into(), done, total));
                    ctx.request_repaint_after(std::time::Duration::from_millis(150));
                });
                self.rt.spawn(async move {
                    send(Msg::ModrinthProfile(
                        crate::modrinth_setup::prepare(&client, None, progress).await,
                    ));
                });
            }
        }
        if cooldown {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        if let Some(started) = self.setup_started {
            let (stage, done, total) = self.progress.lock().unwrap().clone().unwrap_or_default();
            crate::modrinth_setup::setup_progress(ui, &stage, done, total, started.elapsed());
        } else if self.setup_archive.is_some() {
            ui.colored_label(AMBER,"Your copy is prepared. In Modrinth, confirm Install and wait for installation to finish. Continue reopens the same copy; it does not create another one.");
        }
        ui.small("Detects your Jan's pack and copies its game settings, mods and worlds into an independent profile. Your old profile stays intact. Confirm installation in Modrinth.");
        if cfg!(windows) {
            ui.small("Closing this window keeps Jan's updater in the Windows tray. Right-click its icon and choose Exit to stop it.");
        }
        if let Some(s) = crate::updater_background::status() {
            ui.label(format!("Tray updater: {}", s.message));
        }
        ui.horizontal(|ui|{
            if ui.add_enabled(idle,egui::Button::new("Enable background updates")).clicked(){
                let result=(||->Result<()>{let profiles:Vec<_>=crate::updater_profiles::discover()?.into_iter().filter(|p|crate::modrinth_setup::marker(&p.directory).is_some()).collect();anyhow::ensure!(profiles.len()==1,"Finish installation in Modrinth, then choose your profile in the companion updater");crate::updater_background::enable(&profiles[0].directory)})();
                if let Err(e)=result{self.fail("Background updates",e);}
            }
            if ui.add_enabled(idle,egui::Button::new("Pause automatic updates")).clicked(){if let Err(e)=crate::updater_background::disable(){self.fail("Pause updates",e);}}
        });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            let count = self
                .changes
                .as_ref()
                .map(|c| {
                    c.manifest
                        .files
                        .iter()
                        .filter(|f| f.folder() == "mods")
                        .count()
                })
                .unwrap_or(0);
            ui.label(RichText::new(format!("{count} {}", "pack mods")).strong());
            ui.separator();
            ui.label("Existing configs are preserved");
            ui.separator();
            ui.label("SHA512-verified downloads");
        });
        ui.label(
            RichText::new(self.settings.instance_dir().display().to_string())
                .weak()
                .size(12.0),
        );
        ui.add_space(12.0);
        ui.heading("Pack activity");
        let Some(c) = self.changes.clone() else {
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!("Fetch revision: {}", c.revision))
                        .weak()
                        .size(11.0),
                );
                ui.label(format!(
                    "{} {}   →   {} {} (build {})",
                    "Installed:",
                    c.installed_version.as_deref().unwrap_or("nothing"),
                    "Newest:",
                    c.new_version,
                    c.new_build
                ));
                if c.up_to_date() {
                    ui.label(RichText::new("Everything is up to date.").color(GREEN));
                } else {
                    ui.label(format!(
                        "{} {}",
                        "To download:",
                        util::human_bytes(c.download_bytes)
                    ));
                }
                for e in &c.changelog {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("{}  ·  {}", e.version, e.date))
                            .strong()
                            .size(16.0),
                    );
                    if e.text.trim().is_empty() {
                        ui.label(RichText::new("(no notes)").weak());
                    } else {
                        ui.label(e.text.trim());
                    }
                }
                if c.first_install && !c.added.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("{} ({})", "First install", c.added.len())).strong(),
                    );
                    ui.collapsing("Pack contents", |ui| {
                        for i in &c.added {
                            ui.label(format!(
                                "{}  {}",
                                i.name,
                                i.version.as_deref().unwrap_or("")
                            ));
                        }
                    });
                    return;
                }
                let section = |ui: &mut egui::Ui, title: &str, n: usize| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("{title} ({n})")).strong());
                };
                if !c.added.is_empty() {
                    section(ui, "Added", c.added.len());
                    for i in &c.added {
                        ui.label(
                            RichText::new(format!(
                                "+ {}  {}",
                                item_name(i),
                                i.version.as_deref().unwrap_or("")
                            ))
                            .color(GREEN),
                        );
                    }
                }
                if !c.updated.is_empty() {
                    section(ui, "Updated", c.updated.len());
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
                    section(ui, "Removed", c.removed.len());
                    for i in &c.removed {
                        ui.label(RichText::new(format!("- {}", item_name(i))).color(RED));
                    }
                }
                if c.other_files_changed > 0 {
                    ui.add_space(8.0);
                    ui.label(format!(
                        "{} {}",
                        c.other_files_changed,
                        "other files (configs) changed; your existing configs are not overwritten."
                    ));
                }
            });
    }

    fn ui_account(&mut self, ui: &mut egui::Ui) {
        if let Some(code) = self.device_code.clone() {
            ui.label("Enter this code on Microsoft's page:");
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&code.user_code)
                        .size(26.0)
                        .strong()
                        .monospace(),
                );
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(code.user_code.clone());
                }
                if ui.button("Open page").clicked() {
                    let _ = open::that(&code.verification_uri);
                }
            });
            return;
        }
        match &self.account {
            Some(a) => {
                let name = a.name.clone();
                ui.horizontal(|ui| {
                    ui.label(format!("{} {name}", "Logged in as"));
                    if ui.small_button("Log out").clicked() {
                        Account::forget();
                        self.account = None;
                    }
                });
            }
            None if self.settings.client_id().is_some() => {
                if ui
                    .add_enabled(
                        self.busy.is_none(),
                        egui::Button::new("Log in with Microsoft"),
                    )
                    .clicked()
                {
                    self.login();
                }
            }
            None => {
                ui.label(RichText::new("Microsoft sign-in needs application registration. Pack and mod downloads work independently; playing requires sign-in setup.").color(AMBER));
                ui.hyperlink_to(
                    "Registration guide",
                    "https://github.com/bozmund/jans-launcher/blob/main/docs/MICROSOFT_LOGIN.md",
                );
            }
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        if self.busy == Some(Busy::LoggingIn) {
            self.ui_account(ui);
            return;
        }
        if self.busy.is_some() {
            ui.label("Wait for the current operation before changing settings.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.ui_account(ui);
            ui.separator();
            let mut dirty = false;
            ui.heading("Microsoft sign-in");
            ui.label("One application registration enables sign-in for all players. Enter your password only on Microsoft's website.");
            ui.horizontal(|ui| {
                ui.label("Application (client) ID:");
                let mut id=self.settings.msa_client_id.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut id).desired_width(340.0).hint_text("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx")).changed() {
                    self.settings.msa_client_id=(!id.trim().is_empty()).then(||id.trim().into());dirty=true;
                }
            });
            ui.hyperlink_to("Registration: step by step","https://github.com/bozmund/jans-launcher/blob/main/docs/MICROSOFT_SETUP.md");
            ui.add_space(16.0);
            ui.add_space(10.0);
            ui.heading("Game");
            ui.horizontal(|ui| {
                ui.label("Memory (MB):");
                dirty |= ui.add(egui::Slider::new(&mut self.settings.memory_mb, 2048..=32768).step_by(512.0)).changed();
            });
            if let Some(rec) = self.changes.as_ref().and_then(|c| c.manifest.recommended_memory_mb) {
                ui.label(RichText::new(format!("{} {rec} MB", "Recommended:")).weak());
            }
            ui.horizontal(|ui| {
                ui.label("Extra JVM arguments:");
                dirty |= ui.text_edit_singleline(&mut self.settings.extra_jvm_args).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Java (empty = automatic):");
                let mut s = self.settings.java_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                if ui.text_edit_singleline(&mut s).changed() {
                    self.settings.java_path = (!s.trim().is_empty()).then(|| PathBuf::from(s.trim()));
                    dirty = true;
                }
            });

            ui.add_space(10.0);
            ui.collapsing("Advanced", |ui| {
                let label = "Check for launcher updates";
                dirty |= ui.checkbox(&mut self.settings.check_app_updates, label).changed();
                ui.label(RichText::new(format!("jans-launcher {}  ·  {}", env!("CARGO_PKG_VERSION"), config::data_dir().display())).weak());
            });
            if dirty {
                self.mod_plan=None;
                self.mod_updates=None;
                self.inventory.clear();
                self.catalog_result=None;
                self.settings_dirty = true;
                if let Err(e) = self.settings.save() {
                    self.error = Some(format!("{e:#}"));
                }
            }
        });
    }

    fn ui_log(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Copy all").clicked() {
                ui.ctx().copy_text(self.log.join("\n"));
            }
            if ui.button("Clear").clicked() {
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
        crate::updater_background::handle_close(
            ui,
            self.busy.is_some() || self.setup_started.is_some(),
            &mut self.close_to_tray,
            &mut self.resident_started,
            &mut self.error,
        );
        // Settings that change where/what we sync need a fresh check once the user is done editing.
        if self.settings_dirty && self.tab == Tab::Home && self.busy.is_none() {
            self.settings_dirty = false;
            self.changes = None;
            self.check();
        }
        egui::Panel::top("top").show(ui, |ui| self.ui_top(ui));
        egui::Panel::bottom("bottom").show(ui, |ui| self.ui_bottom(ui));
        egui::Panel::left("navigation")
            .exact_size(220.0)
            .resizable(false)
            .show(ui, |ui| self.ui_sidebar(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(23, 25, 29))
                    .inner_margin(24),
            )
            .show(ui, |ui| match self.tab {
                Tab::Home => self.ui_home(ui),
                Tab::Discover => self.ui_discover(ui),
                Tab::Installed => self.ui_installed(ui),
                Tab::Updates => self.ui_mod_updates(ui),
                Tab::Settings => self.ui_settings(ui),
                Tab::Log => self.ui_log(ui),
            });
        if self.busy.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

impl App {
    fn game_version(&self) -> String {
        config::PACK_MINECRAFT.into()
    }

    fn search_mods(&mut self) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Catalog);
        self.error = None;
        let (client, query, game, offset, send) = (
            self.client.clone(),
            self.query.clone(),
            self.game_version(),
            self.catalog_offset,
            self.sender(),
        );
        self.rt.spawn(async move {
            send(Msg::CatalogSearch(
                catalog::search(&client, &query, &game, offset).await,
            ));
        });
    }

    fn load_inventory(&mut self, updates: bool) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Catalog);
        self.error = None;
        let (client, path, game, send) = (
            self.client.clone(),
            self.settings.instance_dir(),
            self.game_version(),
            self.sender(),
        );
        self.rt.spawn(async move {
            let result = tokio::task::spawn_blocking(move || catalog::inventory(&path)).await;
            match result {
                Ok(Ok(mods)) if updates => send(Msg::ModUpdates(
                    catalog::updates(&client, &mods, &game).await,
                )),
                Ok(result) => send(Msg::Inventory(result)),
                Err(e) => {
                    if updates {
                        send(Msg::ModUpdates(Err(e.into())));
                    } else {
                        send(Msg::Inventory(Err(e.into())));
                    }
                }
            }
        });
    }

    fn prepare_mod(&mut self, project: String, version: Option<catalog::Version>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Catalog);
        self.mod_plan = None;
        self.error = None;
        let (client, path, game, send) = (
            self.client.clone(),
            self.settings.instance_dir(),
            self.game_version(),
            self.sender(),
        );
        self.rt.spawn(async move {
            let r = async {
                let previous =
                    tokio::task::spawn_blocking(move || catalog::inventory(&path)).await??;
                let v = match version {
                    Some(v) => v,
                    None => catalog::versions(&client, &project, &game)
                        .await?
                        .into_iter()
                        .next()
                        .context("No compatible Fabric version")?,
                };
                catalog::prepare(&client, v, &game, previous).await
            }
            .await;
            send(Msg::ModPlan(r));
        });
    }

    fn ui_mod_plan(&mut self, ui: &mut egui::Ui) {
        let Some(plan) = self.mod_plan.clone() else {
            return;
        };
        egui::Frame::new().fill(Color32::from_rgb(37,46,40)).corner_radius(10).inner_margin(18).show(ui,|ui| {
            ui.heading("Confirm installation");
            ui.label("Selected mod and required dependencies:");
            for v in &plan.versions {ui.label(format!("{} · {}",v.name,v.version_number));}
            ui.label(RichText::new("The next pack Update may restore pack-owned mods. Installation does not guarantee compatibility with every other mod.").color(AMBER));
            ui.horizontal(|ui| {
                if ui.add_enabled(self.busy.is_none(),egui::Button::new("Install").fill(Color32::from_rgb(27,125,79))).clicked() {
                    self.busy=Some(Busy::InstallingMod);
                    let (client,instance,progress,send)=(self.client.clone(),self.settings.instance_dir(),self.progress_fn(),self.sender());
                    self.rt.spawn(async move {send(Msg::ModInstalled(catalog::install(&client,&instance,plan,progress).await));});
                }
                if ui.add_enabled(self.busy.is_none(),egui::Button::new("Cancel")).clicked(){self.mod_plan=None;}
            });
        });
        ui.add_space(12.0);
    }

    fn ui_discover(&mut self, ui: &mut egui::Ui) {
        ui.heading("Discover mods");
        ui.label(
            RichText::new(format!("Modrinth catalog · Fabric {}", self.game_version())).weak(),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let response = ui.add_sized(
                [ui.available_width() - 150.0, 36.0],
                egui::TextEdit::singleline(&mut self.query).hint_text("Search mods…"),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui
                .add_enabled(self.busy.is_none(), egui::Button::new("Search"))
                .clicked()
                || enter
            {
                self.catalog_offset = 0;
                self.search_mods();
            }
        });
        self.ui_mod_plan(ui);
        let result = self.catalog_result.clone();
        if let Some(result) = result {
            ui.label(format!("{} {}", result.total_hits, "results"));
            egui::ScrollArea::vertical()
                .id_salt("catalog-results")
                .show(ui, |ui| {
                    for hit in result.hits {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(31, 34, 40))
                            .corner_radius(9)
                            .inner_margin(16)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(hit.title.clone()).size(20.0).strong());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .add_enabled(
                                                    self.busy.is_none(),
                                                    egui::Button::new("Install preview"),
                                                )
                                                .clicked()
                                            {
                                                self.prepare_mod(hit.project_id.clone(), None);
                                            }
                                        },
                                    );
                                });
                                ui.label(hit.description);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {} downloads",
                                            hit.author, hit.downloads
                                        ))
                                        .weak()
                                        .size(12.0),
                                    );
                                    ui.hyperlink_to(
                                        "Details",
                                        format!("https://modrinth.com/mod/{}", hit.slug),
                                    );
                                });
                            });
                        ui.add_space(8.0);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                self.busy.is_none() && self.catalog_offset >= 20,
                                egui::Button::new("Previous"),
                            )
                            .clicked()
                        {
                            self.catalog_offset -= 20;
                            self.search_mods();
                        }
                        if ui
                            .add_enabled(
                                self.busy.is_none()
                                    && (self.catalog_offset as u64 + 20) < result.total_hits,
                                egui::Button::new("Next"),
                            )
                            .clicked()
                        {
                            self.catalog_offset += 20;
                            self.search_mods();
                        }
                    });
                });
        }
    }

    fn ui_installed(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Installed mods");
            if ui
                .add_enabled(self.busy.is_none(), egui::Button::new("Refresh"))
                .clicked()
            {
                self.load_inventory(false);
            }
        });
        ui.label("Mods installed in your Fabric 26.3 modpack.");
        ui.add(
            egui::TextEdit::singleline(&mut self.mod_filter)
                .hint_text("Filter mods…")
                .desired_width(f32::INFINITY),
        );
        let filter = self.mod_filter.to_lowercase();
        let mods = self.inventory.clone();
        egui::ScrollArea::vertical()
            .id_salt("installed-mods")
            .show(ui, |ui| {
                for m in mods
                    .iter()
                    .filter(|m| m.name.to_lowercase().contains(&filter))
                {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(31, 34, 40))
                        .corner_radius(8)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&m.name).strong().size(17.0));
                                    ui.label(
                                        RichText::new(format!("{} · {}", m.version, m.path))
                                            .weak()
                                            .size(12.0),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(if m.pack_owned {
                                                "Pack managed"
                                            } else {
                                                "Personal mod"
                                            })
                                            .color(if m.pack_owned { GREEN } else { AMBER })
                                            .size(12.0),
                                        );
                                    },
                                );
                            });
                        });
                    ui.add_space(5.0);
                }
                if mods.is_empty() {
                    ui.label("The modpack is not installed yet. Download it from Overview.");
                }
            });
    }

    fn ui_mod_updates(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Mod updates");
            if ui
                .add_enabled(self.busy.is_none(), egui::Button::new("Check"))
                .clicked()
            {
                self.load_inventory(true);
            }
        });
        ui.label("Only versions for the active Minecraft and Fabric. Custom ports update through pack Fetch.");
        self.ui_mod_plan(ui);
        let updates = self.mod_updates.clone();
        if let Some(updates) = updates {
            if updates.is_empty() {
                ui.label(
                    RichText::new("No newer compatible Modrinth versions found.").color(GREEN),
                );
            }
            egui::ScrollArea::vertical().id_salt("upstream-updates").show(ui,|ui| {
                for update in updates {
                    egui::Frame::new().fill(Color32::from_rgb(31,34,40)).corner_radius(8).inner_margin(14).show(ui,|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui|{
                                ui.label(RichText::new(&update.installed.name).strong().size(18.0));
                                ui.label(format!("{} → {}",update.installed.version,update.version.version_number));
                            });
                            if ui.add_enabled(self.busy.is_none(),egui::Button::new("Update preview")).clicked(){self.prepare_mod(update.version.project_id.clone(),Some(update.version.clone()));}
                        });
                        if update.installed.pack_owned {ui.label(RichText::new("Pack-managed; next pack Update restores the pack version.").color(AMBER).size(12.0));}
                    });
                    ui.add_space(8.0);
                }
            });
        }
    }
}
