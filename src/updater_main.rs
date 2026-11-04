#![cfg_attr(windows, windows_subsystem = "windows")]
// Shared updater modules are intentionally used without the launcher's authentication/game code.
#![allow(dead_code)]
mod config;
mod manifest;
mod modrinth_setup;
mod publish;
mod source;
mod sync;
mod transaction;
mod updater_background;
mod updater_copy;
mod updater_profiles;
#[cfg(windows)]
mod updater_tray;
mod util;

use anyhow::{Context, Result};
use clap::Parser;
use eframe::egui::{self, Color32, RichText};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
};
use updater_profiles::Profile;

#[derive(Parser)]
#[command(
    version,
    about = "Update Jan's Fabric 26.3 pack inside an existing Modrinth profile. Run without flags for the window."
)]
#[command(group(clap::ArgGroup::new("action").args(["check","update","list","background","copy_from"]).multiple(false)))]
struct Args {
    #[arg(long)]
    profile: Option<PathBuf>,
    #[arg(long)]
    check: bool,
    #[arg(long)]
    update: bool,
    #[arg(long)]
    list: bool,
    /// Run only the low-memory notification-area updater (requires enrollment in the window).
    #[arg(long)]
    background: bool,
    /// Copy player files from an old profile into a new empty local --profile.
    #[arg(long, requires = "profile")]
    copy_from: Option<PathBuf>,
}

async fn check(client: &reqwest::Client, path: &std::path::Path) -> Result<sync::Changes> {
    let profile = updater_profiles::from_directory(path)?;
    let fetched = source::PackSource::parse(config::DEFAULT_PACK_SOURCE)?
        .fetch(client)
        .await?;
    profile.compatible(&fetched.manifest)?;
    tokio::task::spawn_blocking(move || sync::plan(&profile.directory, fetched)).await?
}

async fn apply(
    client: &reqwest::Client,
    path: &std::path::Path,
    changes: &sync::Changes,
    progress: sync::ProgressFn,
) -> Result<()> {
    // Re-read Modrinth metadata: a preview cannot authorize changes to a different game version.
    updater_profiles::from_directory(path)?.compatible(&changes.manifest)?;
    sync::apply(client, path, changes, progress).await
}

fn main() {
    let args = Args::parse();
    if args.background {
        if let Err(error) = updater_background::run() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.list || args.check || args.update || args.copy_from.is_some() {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
        let result:Result<()>=tokio::runtime::Runtime::new().unwrap().block_on(async {
            if args.list {println!("{}",serde_json::to_string_pretty(&updater_profiles::discover()?)?);return Ok(());}
            let path=args.profile.context("Specify the existing Modrinth profile using --profile")?;
            if let Some(source)=args.copy_from {let count=updater_copy::copy(&source,&path)?;println!("{}",serde_json::json!({"copied_files":count,"destination":path,"source_preserved":true}));return Ok(());}
            let client=util::http();let changes=check(&client,&path).await?;
            if args.update && !changes.up_to_date() {apply(&client,&path,&changes,Arc::new(|_,_,_|{})).await?;}
            println!("{}",serde_json::json!({"pack":changes.pack_name,"version":changes.new_version,"build":changes.new_build,"files":changes.manifest.files.len(),"download_bytes":changes.download_bytes,"actions":changes.actions.len(),"updated":args.update,"source_revision":changes.revision}));
            Ok(())
        });
        if let Err(error) = result {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Jan's Modpack Updater")
            .with_inner_size([850.0, 720.0])
            .with_min_inner_size([700.0, 560.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .unwrap_or_default(),
            ),
        ..Default::default()
    };
    let result = eframe::run_native(
        "jans-modpack-updater",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, args.profile)))),
    );
    if let Err(error) = result {
        eprintln!("{error}");
    }
}

enum Message {
    Profiles(Result<Vec<Profile>>),
    Checked(Result<sync::Changes>),
    Applied(Result<()>),
    Copied(Result<usize>),
    NativeProfile(Result<Option<PathBuf>>),
}
struct App {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
    tx: Sender<Message>,
    rx: Receiver<Message>,
    ctx: egui::Context,
    profiles: Vec<Profile>,
    selected: Option<PathBuf>,
    copy_source: Option<PathBuf>,
    changes: Option<sync::Changes>,
    busy: bool,
    progress: Arc<Mutex<(String, u64, u64)>>,
    status: String,
    error: Option<String>,
}

impl App {
    fn preference() -> PathBuf {
        config::data_dir().join("modrinth-updater.json")
    }
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut style = (*cc.egui_ctx.style_of(egui::Theme::Dark)).clone();
        style.visuals.panel_fill = Color32::from_rgb(23, 25, 29);
        style.spacing.item_spacing = egui::vec2(12.0, 10.0);
        style.spacing.button_padding = egui::vec2(18.0, 10.0);
        cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
        let (tx, rx) = mpsc::channel();
        let selected = initial.or_else(|| {
            std::fs::read(Self::preference())
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        });
        let mut app = Self {
            runtime: tokio::runtime::Runtime::new().unwrap(),
            client: util::http(),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            profiles: vec![],
            selected,
            copy_source: None,
            changes: None,
            busy: false,
            progress: Arc::new(Mutex::new((String::new(), 0, 0))),
            status: String::new(),
            error: None,
        };
        app.refresh();
        app
    }
    fn sender(&self) -> impl Fn(Message) + Send + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |message| {
            let _ = tx.send(message);
            ctx.request_repaint();
        }
    }
    fn refresh(&mut self) {
        self.busy = true;
        self.error = None;
        let send = self.sender();
        self.runtime
            .spawn_blocking(move || send(Message::Profiles(updater_profiles::discover())));
    }
    fn choose(&mut self, path: PathBuf) {
        self.selected = Some(path);
        self.changes = None;
        self.error = None;
        if let Ok(bytes) = serde_json::to_vec(&self.selected) {
            if let Err(e) = util::write_atomic(&Self::preference(), &bytes) {
                self.error = Some(e.to_string());
            }
        }
    }
    fn start_check(&mut self) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.changes = None;
        self.status = "Checking the published pack…".into();
        let (client, send) = (self.client.clone(), self.sender());
        self.runtime.spawn(async move {
            send(Message::Checked(check(&client, &path).await));
        });
    }
    fn start_update(&mut self) {
        let (Some(path), Some(changes)) = (self.selected.clone(), self.changes.clone()) else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.status = "Downloading verified updates…".into();
        *self.progress.lock().unwrap() = (String::new(), 0, 0);
        let (client, send, progress, ctx) = (
            self.client.clone(),
            self.sender(),
            self.progress.clone(),
            self.ctx.clone(),
        );
        self.runtime.spawn(async move {
            send(Message::Applied(
                apply(
                    &client,
                    &path,
                    &changes,
                    Arc::new(move |phase, done, total| {
                        *progress.lock().unwrap() = (phase.into(), done, total);
                        ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    }),
                )
                .await,
            ));
        });
    }
    fn messages(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            self.busy = false;
            match message {
                Message::Profiles(Ok(profiles)) => {
                    self.profiles = profiles;
                    let owned: Vec<_> = self
                        .profiles
                        .iter()
                        .filter(|p| modrinth_setup::marker(&p.directory).is_some())
                        .collect();
                    if owned.len() == 1 {
                        self.selected = Some(owned[0].directory.clone());
                    }
                    if self
                        .selected
                        .as_ref()
                        .is_some_and(|p| !self.profiles.iter().any(|x| &x.directory == p))
                    {
                        if let Some(path) = self.selected.clone() {
                            match updater_profiles::from_directory(&path) {
                                Ok(profile) => self.profiles.push(profile),
                                Err(_) => self.selected = None,
                            }
                        }
                    }
                    if self.selected.is_none() && self.profiles.len() == 1 {
                        self.choose(self.profiles[0].directory.clone());
                    }
                    self.status = "Select your Fabric 26.3 profile, then check for updates.".into();
                }
                Message::Checked(Ok(changes)) => {
                    self.status = if changes.up_to_date() {
                        format!("Up to date: {}", changes.new_version)
                    } else {
                        format!("Ready to update to {}", changes.new_version)
                    };
                    self.changes = Some(changes);
                }
                Message::Applied(Ok(())) => {
                    self.start_check();
                    self.status =
                        "Update installed. Check complete before opening Modrinth.".into();
                }
                Message::Copied(Ok(count)) => {
                    self.status = format!(
                        "Copied {count} files. The old profile is preserved. Check for pack updates, then enable automatic updates."
                    );
                    self.changes = None;
                }
                Message::Profiles(Err(e))
                | Message::Checked(Err(e))
                | Message::Applied(Err(e))
                | Message::Copied(Err(e))
                | Message::NativeProfile(Err(e)) => {
                    self.error = Some(format!("{e:#}"));
                    self.status =
                        "Operation did not complete. Read the error below and retry.".into();
                }
                Message::NativeProfile(Ok(path)) => {
                    match modrinth_setup::open_installer(path.as_deref()) {
                        Ok(()) => {
                            self.status = if path.is_some() {
                                "Confirm Install in Modrinth, then Refresh here and select Jan's Fabric 26.3. Game settings and worlds are copied; the old profile is kept. Keep the local migration archive private.".into()
                            } else {
                                "Your independent profile already exists. Select it here for automatic updates.".into()
                            }
                        }
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.messages();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(23,25,29)).inner_margin(24)).show(ui,|ui| {
            ui.heading(RichText::new("Jan's Modpack Updater").size(28.0));
            ui.label("Update your existing Fabric 26.3 profile. Keep playing through Modrinth.");
            ui.add_space(12.0);
            egui::Frame::new().fill(Color32::from_rgb(30,44,37)).corner_radius(10).inner_margin(16).show(ui,|ui| {
                ui.label(RichText::new("No Minecraft API approval or additional sign-in needed").color(Color32::LIGHT_GREEN));
                ui.label("Close Minecraft before updating. Your worlds and existing configurations stay unchanged.");
            });
            ui.add_space(12.0);
            ui.label(RichText::new("MODRINTH PROFILE").strong());
            if ui.add_enabled(!self.busy,egui::Button::new("Create Modrinth profile").fill(Color32::from_rgb(27,125,79))).clicked(){
                self.busy=true;self.error=None;self.status="Detecting your pack and preparing an independent copy…".into();
                let client=self.client.clone();let send=self.sender();let state=self.progress.clone();let ctx=self.ctx.clone();
                let progress:sync::ProgressFn=Arc::new(move|phase,done,total|{*state.lock().unwrap()=(phase.into(),done,total);ctx.request_repaint_after(std::time::Duration::from_millis(150));});
                self.runtime.spawn(async move{send(Message::NativeProfile(modrinth_setup::prepare(&client,None,progress).await));});
            }
            ui.small("Detects your old Jan's pack and copies its game settings and worlds into an independent profile. Confirm Install in Modrinth; keep the old profile as backup.");
            let current=self.profiles.iter().find(|p|Some(&p.directory)==self.selected.as_ref()).map(|p|p.name.clone()).unwrap_or_else(||"Choose Fabric 26.3…".into());
            let mut chosen=None;
            ui.add_enabled_ui(!self.busy,|ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("modrinth-profile").selected_text(current).width(370.0).show_ui(ui,|ui| {
                        for profile in &self.profiles {if ui.selectable_label(Some(&profile.directory)==self.selected.as_ref(),&profile.name).clicked(){chosen=Some(profile.directory.clone());}}
                    });
                    if ui.button("Refresh").clicked(){self.refresh();}
                    if ui.button("Browse…").clicked() {
                        if let Some(path)=rfd::FileDialog::new().set_title("Select the Fabric 26.3 folder in ModrinthApp/profiles").pick_folder() {
                            match updater_profiles::from_directory(&path) {Ok(profile)=>{chosen=Some(profile.directory.clone());if !self.profiles.iter().any(|p|p.directory==profile.directory){self.profiles.push(profile);}},Err(e)=>self.error=Some(format!("{e:#}"))}
                        }
                    }
                });
            });
            if let Some(path)=chosen{self.choose(path);}
            if let Some(path)=&self.selected {ui.label(RichText::new(path.display().to_string()).weak().size(12.0));}
            egui::CollapsingHeader::new("One-time setup: independent copy of your old profile").show(ui,|ui|{
                ui.label("1. Select the old profile above and click Use as source.");
                ui.label("2. In Modrinth, create a NEW local Fabric 26.3 profile with the same loader. Keep it empty; do not duplicate/import the shared pack.");
                ui.label("3. Refresh this list, select the new profile and click Copy everything. Close Minecraft first.");
                if ui.add_enabled(!self.busy&&self.selected.is_some(),egui::Button::new("Use selected profile as source")).clicked(){self.copy_source=self.selected.clone();}
                if let Some(source)=self.copy_source.clone(){
                    ui.small(format!("Source (kept): {}",source.display()));
                    if ui.add_enabled(!self.busy&&self.selected.as_ref().is_some_and(|p|p!=&source),egui::Button::new("Copy everything into selected new profile")).clicked(){
                        let destination=self.selected.clone().unwrap();let send=self.sender();self.busy=true;self.error=None;self.status="Copying and verifying your profile; this may take several minutes…".into();
                        self.runtime.spawn_blocking(move||send(Message::Copied(updater_copy::copy(&source,&destination))));
                    }
                }
                ui.small("Copies mods, worlds, settings and personal files. Updater caches/locks stay separate. Linked files are refused. The old profile is never deleted.");
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy&&self.selected.is_some(),egui::Button::new("Check for updates")).clicked(){self.start_check();}
                let available=self.changes.as_ref().is_some_and(|c|!c.up_to_date());
                if ui.add_enabled(!self.busy&&available,egui::Button::new("Update profile").fill(Color32::from_rgb(27,125,79))).clicked(){self.start_update();}
                if ui.add_enabled(!self.busy,egui::Button::new("Open Modrinth")).clicked(){if let Err(e)=open::that("modrinth://"){self.error=Some(format!("Cannot open Modrinth: {e}"));}}
            });
            if self.busy {
                let (phase,done,total)=self.progress.lock().unwrap().clone();
                if total>0 {ui.add(egui::ProgressBar::new((done as f32/total as f32).clamp(0.0,1.0)).show_percentage().text(format!("{phase}: {} / {}",util::human_bytes(done),util::human_bytes(total))));}else{ui.spinner();}
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            }
            ui.label(&self.status);
            ui.separator();
            ui.label(RichText::new("AUTOMATIC UPDATES").strong());
            ui.label("Check every 5 minutes in the Windows tray. Waits while Minecraft is running.");
            let enrolled=updater_background::enrollment().ok().flatten();
            let enabled=enrolled.as_ref().is_some_and(|e|e.enabled);
            if let Some(e)=enrolled.filter(|e|e.enabled){ui.label(format!("Enabled for {}",e.profile.display()));}
            ui.horizontal(|ui|{
                if ui.add_enabled(cfg!(windows)&&!self.busy&&self.selected.is_some(),egui::Button::new("Enable automatic updates at login")).clicked(){
                    match updater_background::enable(self.selected.as_ref().unwrap()){Ok(())=>self.status="Background updater enabled. You can close this window.".into(),Err(e)=>self.error=Some(format!("{e:#}"))}
                }
                if ui.add_enabled(enabled,egui::Button::new("Disable automatic updates")).clicked(){
                    match updater_background::disable(){Ok(())=>self.status="Automatic updates and login startup disabled.".into(),Err(e)=>self.error=Some(format!("{e:#}"))}
                }
            });
            if let Some(s)=updater_background::status(){ui.label(format!("Background: {}",s.message));if let Some(t)=s.last_check{ui.small(format!("Last check: {t}"));}if let Some(t)=s.last_update{ui.small(format!("Last update: {t}"));}}
            if enabled {ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));}
            if let Some(error)=&self.error {ui.label(RichText::new(error).color(Color32::LIGHT_RED));}
            if let Some(changes)=self.changes.clone() {
                ui.separator();
                ui.label(format!("Installed: {}   →   Published: {}",changes.installed_version.as_deref().unwrap_or("Not linked yet"),changes.new_version));
                ui.label(format!("{} to download · {} file operation(s)",util::human_bytes(changes.download_bytes),changes.actions.len()));
                if changes.first_install {ui.label("First connection: matching existing mod files are reused; they are not downloaded again.");}
                egui::ScrollArea::vertical().id_salt("pack-changes").show(ui,|ui| {
                    for entry in &changes.changelog {ui.label(RichText::new(format!("{} · {}",entry.version,entry.date)).strong());ui.label(&entry.text);}
                    for item in &changes.added {ui.label(format!("+ {} {}",item.name,item.version.as_deref().unwrap_or("")));}
                    for (old,new) in &changes.updated {ui.label(format!("~ {}: {} → {}",new.name,old.version.as_deref().unwrap_or("?"),new.version.as_deref().unwrap_or("?")));}
                    for item in &changes.removed {ui.label(format!("− {}",item.name));}
                    ui.add_space(10.0);ui.label(RichText::new("Updates come from Jan's published pack, not Modrinth's shared-instance updater. Existing config files are installed only when missing.").weak().size(12.0));
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_update_rechecks_loader_and_preserves_database_worlds_and_configs() {
        let root =
            std::env::temp_dir().join(format!("jans-updater-install-{}", std::process::id()));
        assert!(!root.exists());
        let profile = root.join("profiles/Fabric 26.3");
        std::fs::create_dir_all(profile.join("mods")).unwrap();
        std::fs::create_dir_all(profile.join("config")).unwrap();
        std::fs::create_dir_all(profile.join("saves/Keep")).unwrap();
        std::fs::write(profile.join("mods/personal.jar"), b"personal mod").unwrap();
        std::fs::write(profile.join("config/example.json"), b"player config").unwrap();
        std::fs::write(profile.join("saves/Keep/level.dat"), b"world").unwrap();
        let db = root.join("app.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE instances(name TEXT,path TEXT,applied_content_set_id TEXT);CREATE TABLE instance_content_sets(id TEXT,game_version TEXT,loader TEXT,loader_version TEXT);INSERT INTO instances VALUES('Friends','Fabric 26.3','one');INSERT INTO instance_content_sets VALUES('one','26.3','fabric','0.19.3');").unwrap();
        drop(c);
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        let artifact = source.join("fixture.jar");
        std::fs::write(&artifact, b"test mod bytes").unwrap();
        let config = source.join("example.json");
        std::fs::write(&config, b"pack config").unwrap();
        let m:manifest::Manifest=serde_json::from_value(serde_json::json!({"format":1,"name":"fixture","build":1,"version":"test","minecraft":"26.3","fabric_loader":"0.20.1","files":[{"path":"mods/fixture.jar","sha512":util::sha512_file(&artifact).unwrap(),"size":14,"url":"fixture.jar"},{"path":"config/example.json","sha512":util::sha512_file(&config).unwrap(),"size":11,"url":"example.json","policy":"once"}]})).unwrap();
        let changes = sync::plan(
            &profile,
            source::Fetched {
                manifest: m,
                base: source.display().to_string(),
                revision: "fixture".into(),
            },
        )
        .unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = util::http();
        let progress: sync::ProgressFn = Arc::new(|_, _, _| {});
        let err = rt
            .block_on(apply(&client, &profile, &changes, progress.clone()))
            .unwrap_err();
        assert!(err.to_string().contains("0.20.1"));
        assert!(!profile.join("mods/fixture.jar").exists());
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute(
            "UPDATE instance_content_sets SET loader_version='0.20.1'",
            [],
        )
        .unwrap();
        drop(c);
        let db_hash = util::sha512_file(&db).unwrap();
        rt.block_on(apply(&client, &profile, &changes, progress))
            .unwrap();
        assert_eq!(
            std::fs::read(profile.join("mods/fixture.jar")).unwrap(),
            b"test mod bytes"
        );
        assert_eq!(
            std::fs::read(profile.join("mods/personal.jar")).unwrap(),
            b"personal mod"
        );
        assert_eq!(
            std::fs::read(profile.join("config/example.json")).unwrap(),
            b"player config"
        );
        assert_eq!(
            std::fs::read(profile.join("saves/Keep/level.dat")).unwrap(),
            b"world"
        );
        assert_eq!(util::sha512_file(&db).unwrap(), db_hash);
        std::fs::remove_dir_all(root).unwrap();
    }
}
