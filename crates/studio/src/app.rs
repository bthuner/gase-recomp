//! The window: three cards (ROM, app, build), the tools list, and the log.
//!
//! egui is an *immediate-mode* GUI: `update` runs every frame and draws the
//! whole window from the state below; a click is just `if button.clicked()`.
//! The build runs on its own thread (see [`crate::pipeline`]) and talks to
//! the window through a channel, read at each frame.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use gase_recomp_remote::server::{Request as PhoneRequest, Server};
use gase_recomp_remote::{PORT, Progress as PhoneProgress};

use crate::pipeline::{self, ABIS, Check, Event, Job, RomInfo, Stage, Target};

const ACCENT: Color32 = Color32::from_rgb(0x4F, 0xC3, 0xF7);
const GOOD: Color32 = Color32::from_rgb(0x66, 0xBB, 0x6A);
const BAD: Color32 = Color32::from_rgb(0xEF, 0x53, 0x50);
const DIM: Color32 = Color32::from_gray(150);

/// Where a build is.
#[derive(Debug, Default)]
enum Progress {
    #[default]
    Idle,
    Running(Stage),
    Done(PathBuf),
    Failed(String),
}

#[derive(Debug)]
pub struct Studio {
    target: Target,
    rom: Option<RomInfo>,
    rom_error: Option<String>,
    app_name: String,
    app_id: String,
    out: PathBuf,
    abis: Vec<(&'static str, bool)>,
    checks: Vec<Check>,
    progress: Progress,
    /// Events from the build (or a fix, or `adb install`) thread.
    events: Option<Receiver<Event>>,
    log: Vec<String>,
    show_log: bool,
    /// Phone builds: the server, while the switch is on.
    phone: Option<Server>,
    phone_error: Option<String>,
    /// The phone build running now (its number on the server).
    phone_build: Option<u32>,
}

impl Studio {
    #[must_use]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        rom: Option<PathBuf>,
        target: Target,
        phone_builds: bool,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 8.0);
            s.spacing.button_padding = egui::vec2(12.0, 6.0);
        });
        let mut studio = Self {
            target,
            rom: None,
            rom_error: None,
            app_name: String::new(),
            app_id: String::new(),
            out: PathBuf::new(),
            abis: ABIS
                .iter()
                .map(|&(abi, _)| (abi, abi == "arm64-v8a"))
                .collect(),
            checks: Vec::new(),
            progress: Progress::Idle,
            events: None,
            log: Vec::new(),
            show_log: false,
            phone: None,
            phone_error: None,
            phone_build: None,
        };
        studio.refresh_checks();
        if let Some(path) = rom {
            studio.load_rom(path);
        }
        if phone_builds {
            studio.set_phone_builds(true);
        }
        studio
    }

    fn chosen_abis(&self) -> Vec<String> {
        self.abis
            .iter()
            .filter(|(_, on)| *on)
            .map(|(abi, _)| (*abi).to_string())
            .collect()
    }

    fn refresh_checks(&mut self) {
        self.checks = pipeline::check_tools(self.target, &self.chosen_abis());
    }

    fn load_rom(&mut self, path: PathBuf) {
        match RomInfo::read(&path) {
            Ok(info) => {
                self.app_name = info.display_name();
                self.app_id = info.default_app_id();
                self.out = info.default_out();
                self.rom = Some(info);
                self.rom_error = None;
                self.progress = Progress::Idle;
            }
            Err(e) => self.rom_error = Some(e),
        }
    }

    fn busy(&self) -> bool {
        self.events.is_some()
    }

    fn start_build(&mut self) {
        let Some(rom) = &self.rom else { return };
        let job = Job {
            target: self.target,
            rom: rom.path.clone(),
            out: self.out.clone(),
            app_name: self.app_name.trim().to_string(),
            app_id: self.app_id.trim().to_string(),
            abis: self.chosen_abis(),
            gase_dir: pipeline::default_gase_dir(),
        };
        let (tx, rx) = channel();
        std::thread::spawn(move || pipeline::run(&job, &tx));
        self.events = Some(rx);
        self.log.clear();
        self.progress = Progress::Running(Stage::Recompile);
    }

    /// Run a fix or `adb install` in the background; its output goes to the log.
    fn start_command(&mut self, program: PathBuf, args: Vec<String>) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = pipeline::run_command(&program, &args, &tx);
            let _ = tx.send(Event::Log(match r {
                Ok(()) => "done".into(),
                Err(e) => e,
            }));
        });
        self.events = Some(rx);
        self.show_log = true;
    }

    fn poll(&mut self) {
        let Some(rx) = &self.events else { return };
        let mut finished = false;
        loop {
            let event = rx.try_recv();
            if let Ok(e) = &event {
                self.tell_phone(e);
            }
            match event {
                Ok(Event::Log(line)) => self.log.push(line),
                Ok(Event::Stage(stage)) => self.progress = Progress::Running(stage),
                Ok(Event::Done(Ok(apk))) => self.progress = Progress::Done(apk),
                Ok(Event::Done(Err(e))) => {
                    self.log.push(format!("error: {e}"));
                    self.progress = Progress::Failed(e);
                    self.show_log = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.events = None;
            self.phone_build = None;
            self.refresh_checks();
        }
    }

    // ---- phone builds ---------------------------------------------------------

    fn set_phone_builds(&mut self, on: bool) {
        self.phone_error = None;
        self.phone = None;
        if on {
            match Server::start(&host_name(), PORT, Some(PORT)) {
                Ok(server) => self.phone = Some(server),
                Err(e) => self.phone_error = Some(format!("cannot listen on port {PORT}: {e}")),
            }
        }
    }

    /// Pass a build event on to the phone that asked for it.
    fn tell_phone(&self, event: &Event) {
        let (Some(server), Some(id)) = (&self.phone, self.phone_build) else {
            return;
        };
        let stage = match &self.progress {
            Progress::Running(s) => s.label().to_string(),
            _ => String::new(),
        };
        match event {
            Event::Stage(s) => server.set_progress(
                id,
                PhoneProgress::Running {
                    stage: s.label().into(),
                    log: String::new(),
                },
            ),
            Event::Log(line) => server.set_progress(
                id,
                PhoneProgress::Running {
                    stage,
                    log: line.clone(),
                },
            ),
            Event::Done(Ok(apk)) => match std::fs::read(apk) {
                Ok(bytes) => server.finish(id, bytes),
                Err(e) => server.set_progress(id, PhoneProgress::Failed(e.to_string())),
            },
            Event::Done(Err(e)) => server.set_progress(id, PhoneProgress::Failed(e.clone())),
        }
    }

    /// Accept a phone's request: build its ROM into an APK for arm64.
    fn accept_phone(&mut self, request: &PhoneRequest) {
        let Some(rom) = self.phone.as_ref().and_then(|s| s.rom(request.id)) else {
            return;
        };
        let fail = |studio: &Self, e: String| {
            if let Some(server) = &studio.phone {
                server.set_progress(request.id, PhoneProgress::Failed(e));
            }
        };
        let dir = pipeline::default_gase_dir()
            .parent()
            .map_or_else(std::env::temp_dir, |p| p.join("phone"));
        let path = dir.join(format!("{}.bin", request.id));
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &rom)) {
            fail(self, e.to_string());
            return;
        }
        // (Not a ROM? Then nothing is loaded, rather than the last one.)
        self.rom = None;
        self.load_rom(path);
        let Some(id) = self.rom.as_ref().map(|info| info.id.clone()) else {
            fail(self, self.rom_error.clone().unwrap_or_default());
            return;
        };
        // Next to the other phone builds, named after the game.
        self.out = dir.join(format!("{id}-recomp"));
        self.target = Target::Android;
        for (abi, on) in &mut self.abis {
            *on = *abi == "arm64-v8a";
        }
        self.refresh_checks();
        self.phone_build = Some(request.id);
        self.start_build();
    }

    /// The question, while a phone waits for an answer.
    fn phone_dialog(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return; // asked again when this build is over
        }
        let Some(request) = self
            .phone
            .as_ref()
            .and_then(|s| s.waiting().first().cloned())
        else {
            return;
        };
        let mut answer = None;
        egui::Window::new("A phone wants an app")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("Build \"{}\" into an Android app?", request.title))
                        .size(16.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(format!("Asked by {} on this network.", request.from.ip()))
                        .color(DIM),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let accept = egui::Button::new(RichText::new("Accept").strong())
                        .fill(GOOD.gamma_multiply(0.5));
                    if ui.add(accept).clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Refuse").clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => self.accept_phone(&request),
            Some(false) => {
                if let Some(server) = &self.phone {
                    server.set_progress(request.id, PhoneProgress::Refused);
                }
            }
            None => {}
        }
    }

    // ---- drawing -----------------------------------------------------------

    fn rom_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "1", "The ROM", |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.busy(), egui::Button::new("Choose a ROM…"))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Mega Drive / Genesis ROM", &["md", "bin", "gen", "smd"])
                        .pick_file()
                    {
                        self.load_rom(path);
                    }
                }
                ui.label(
                    RichText::new("Use a ROM you own: it goes into what you build.").color(DIM),
                );
            });
            if let Some(e) = &self.rom_error {
                ui.colored_label(BAD, e);
            }
            if let Some(rom) = &self.rom {
                egui::Grid::new("rom").num_columns(2).show(ui, |ui| {
                    ui.label(RichText::new("Title").color(DIM));
                    ui.label(RichText::new(&rom.title).strong());
                    ui.end_row();
                    ui.label(RichText::new("File").color(DIM));
                    path_label(ui, &rom.path);
                    ui.end_row();
                    ui.label(RichText::new("Size").color(DIM));
                    ui.label(format!(
                        "{} KiB · CRC-32 {:08X} · checksum {}",
                        rom.size / 1024,
                        rom.crc32,
                        if rom.checksum_ok { "OK" } else { "mismatch" }
                    ));
                    ui.end_row();
                });
            }
        });
    }

    fn app_card(&mut self, ui: &mut egui::Ui) {
        let enabled = self.rom.is_some() && !self.busy();
        card(ui, "2", "What to build", |ui| {
            ui.add_enabled_ui(!self.busy(), |ui| {
                ui.horizontal(|ui| {
                    let before = self.target;
                    ui.selectable_value(&mut self.target, Target::Desktop, "This computer");
                    ui.selectable_value(&mut self.target, Target::Android, "Android app (APK)");
                    if self.target != before {
                        self.progress = Progress::Idle;
                        self.refresh_checks();
                    }
                });
            });
            ui.add_enabled_ui(enabled, |ui| {
                egui::Grid::new("app").num_columns(2).show(ui, |ui| {
                    if self.target == Target::Android {
                        self.android_fields(ui);
                    } else {
                        ui.label(RichText::new("What").color(DIM));
                        ui.label(
                            RichText::new(
                                "gase with the game's code compiled in: same window, menus and save states",
                            )
                            .color(DIM),
                        );
                        ui.end_row();
                    }
                    ui.label(RichText::new("Folder").color(DIM));
                    path_label(ui, &self.out);
                    ui.end_row();
                });
            });
        });
    }

    fn android_fields(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Name").color(DIM));
        ui.add(egui::TextEdit::singleline(&mut self.app_name).desired_width(320.0));
        ui.end_row();
        ui.label(RichText::new("Identifier").color(DIM));
        ui.add(egui::TextEdit::singleline(&mut self.app_id).desired_width(320.0));
        ui.end_row();
        ui.label("");
        match pipeline::check_app_id(self.app_id.trim()) {
            Ok(()) => ui.label(RichText::new("installs as its own app, next to gase").color(DIM)),
            // (Nothing to complain about before a ROM is chosen.)
            Err(_) if self.rom.is_none() => ui.label(""),
            Err(e) => ui.colored_label(BAD, e),
        };
        ui.end_row();
        ui.label(RichText::new("Processors").color(DIM));
        ui.horizontal(|ui| {
            let mut changed = false;
            for (abi, on) in &mut self.abis {
                changed |= ui.checkbox(on, *abi).changed();
            }
            if changed {
                self.refresh_checks();
            }
        });
        ui.end_row();
        ui.label("");
        ui.label(
            RichText::new("arm64-v8a runs on nearly every phone; each one compiles the game again")
                .color(DIM),
        );
        ui.end_row();
    }

    fn build_card(&mut self, ui: &mut egui::Ui) {
        let tools_ok = self.checks.iter().all(|c| c.ok);
        let android_ok =
            pipeline::check_app_id(self.app_id.trim()).is_ok() && !self.chosen_abis().is_empty();
        let ready = self.rom.is_some()
            && !self.busy()
            && tools_ok
            && (self.target == Target::Desktop || android_ok);
        let label = match self.target {
            Target::Android => "Build the APK",
            Target::Desktop => "Build the game",
        };
        card(ui, "3", "Build", |ui| {
            ui.horizontal(|ui| {
                let button = egui::Button::new(RichText::new(label).size(18.0).strong())
                    .fill(if ready {
                        ACCENT.gamma_multiply(0.6)
                    } else {
                        Color32::from_gray(60)
                    })
                    .min_size(egui::vec2(180.0, 40.0));
                if ui.add_enabled(ready, button).clicked() {
                    self.start_build();
                }
                if !tools_ok {
                    ui.colored_label(BAD, "Some tools are missing: see the list on the right.");
                } else if self.rom.is_none() {
                    ui.label(RichText::new("Choose a ROM first.").color(DIM));
                }
            });
            // The stages, ticked as they pass.
            let current = match &self.progress {
                Progress::Running(s) => Some(*s),
                _ => None,
            };
            for &stage in self.target.stages() {
                let passed = matches!(self.progress, Progress::Done(_))
                    || current.is_some_and(|c| stage < c);
                let color = if passed {
                    GOOD
                } else if current == Some(stage) {
                    ACCENT
                } else {
                    Color32::from_gray(80)
                };
                ui.horizontal(|ui| {
                    dot(ui, color);
                    let text = RichText::new(stage.label());
                    ui.label(if current == Some(stage) {
                        text.strong()
                    } else if passed {
                        text
                    } else {
                        text.color(DIM)
                    });
                    if current == Some(stage) {
                        ui.spinner();
                    }
                });
            }
            match &self.progress {
                Progress::Done(apk) => {
                    let apk = apk.clone();
                    ui.horizontal(|ui| {
                        dot(ui, GOOD);
                        let ready = match self.target {
                            Target::Android => "Your app is ready:",
                            Target::Desktop => "Your game is ready:",
                        };
                        ui.label(RichText::new(ready).color(GOOD).strong());
                        path_label(ui, &apk);
                    });
                    if self.target == Target::Desktop {
                        ui.horizontal(|ui| {
                            let play = egui::Button::new(RichText::new("Play").size(18.0).strong())
                                .fill(GOOD.gamma_multiply(0.5))
                                .min_size(egui::vec2(120.0, 36.0));
                            if ui.add(play).clicked() {
                                if let Err(e) = pipeline::play(&apk) {
                                    self.log.push(format!("error: {e}"));
                                    self.show_log = true;
                                }
                            }
                            if ui.button("Open the folder").clicked() {
                                if let Some(dir) = apk.parent() {
                                    open_folder(dir);
                                }
                            }
                        });
                        return;
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Open the folder").clicked() {
                            if let Some(dir) = apk.parent() {
                                open_folder(dir);
                            }
                        }
                        match pipeline::adb() {
                            Some(adb) => {
                                if ui
                                    .add_enabled(
                                        !self.busy(),
                                        egui::Button::new("Install on the phone (adb)"),
                                    )
                                    .on_hover_text("USB debugging must be on")
                                    .clicked()
                                {
                                    let args = vec![
                                        "install".into(),
                                        "-r".into(),
                                        apk.display().to_string(),
                                    ];
                                    self.start_command(adb, args);
                                }
                            }
                            None => {
                                ui.label(
                                    RichText::new("copy the APK to your phone and open it")
                                        .color(DIM),
                                );
                            }
                        }
                    });
                }
                Progress::Failed(e) => {
                    ui.colored_label(BAD, format!("Failed: {e}"));
                }
                _ => {}
            }
        });
    }

    fn tools_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("What you need");
            if ui
                .add_enabled(!self.busy(), egui::Button::new("Check again").small())
                .clicked()
            {
                self.refresh_checks();
            }
        });
        ui.label(RichText::new("Checked on this computer.").color(DIM));
        ui.add_space(4.0);
        let mut fix_to_run = None;
        for check in &self.checks {
            ui.horizontal(|ui| {
                dot(ui, if check.ok { GOOD } else { BAD });
                ui.label(RichText::new(check.name).strong());
            });
            ui.horizontal_wrapped(|ui| {
                ui.add_space(20.0);
                ui.label(RichText::new(&check.detail).color(DIM).small());
            });
            if let Some(fix) = &check.fix {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    if ui
                        .add_enabled(!self.busy(), egui::Button::new("Fix").small())
                        .on_hover_text(fix.display())
                        .clicked()
                    {
                        fix_to_run = Some(fix.clone());
                    }
                });
            }
        }
        if let Some(fix) = fix_to_run {
            self.start_command(fix.program, fix.args);
        }

        ui.add_space(12.0);
        ui.separator();
        ui.heading("Phone builds");
        let mut on = self.phone.is_some();
        if ui
            .checkbox(&mut on, "Phones on this network may ask")
            .on_hover_text("gase on a phone: pause menu → Make an app of this game")
            .changed()
        {
            self.set_phone_builds(on);
        }
        match (&self.phone, &self.phone_error) {
            (Some(server), _) => {
                ui.horizontal(|ui| {
                    dot(ui, GOOD);
                    ui.label(
                        RichText::new(format!(
                            "Listening as {} (port {})",
                            host_name(),
                            server.port()
                        ))
                        .small(),
                    );
                });
                ui.label(
                    RichText::new("You will be asked before anything is built.")
                        .color(DIM)
                        .small(),
                );
            }
            (None, Some(e)) => {
                ui.colored_label(BAD, RichText::new(e).small());
            }
            (None, None) => {
                ui.label(
                    RichText::new("Off: phones cannot see this computer.")
                        .color(DIM)
                        .small(),
                );
            }
        }
    }
}

/// This computer's name, as phones will show it.
fn host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "studio".into())
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.phone.is_some() {
            // Look for phone requests twice a second.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.phone_dialog(ctx);
        // A ROM dropped on the window.
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()))
        {
            if !self.busy() {
                self.load_rom(path);
            }
        }

        egui::TopBottomPanel::top("title").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("gase-recomp")
                        .size(22.0)
                        .strong()
                        .color(ACCENT),
                );
                ui.label(RichText::new("studio").size(22.0));
                ui.label(
                    RichText::new(
                        "pick a ROM · press Build · play it natively, here or on Android",
                    )
                    .color(DIM),
                );
            });
            ui.add_space(4.0);
        });
        egui::TopBottomPanel::bottom("log")
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.selectable_label(self.show_log, "Log").clicked() {
                        self.show_log = !self.show_log;
                    }
                    if let Some(last) = self.log.last() {
                        ui.label(RichText::new(last).color(DIM).monospace().small());
                    }
                });
                if self.show_log {
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for line in &self.log {
                                ui.label(RichText::new(line).monospace().small());
                            }
                        });
                }
            });
        egui::SidePanel::right("tools")
            .default_width(280.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.tools_panel(ui));
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.rom_card(ui);
                self.app_card(ui);
                self.build_card(ui);
            });
        });
    }
}

/// A status light: green, red, blue (running) or grey.
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// A path that may not fit: cut with "…", whole on hover.
fn path_label(ui: &mut egui::Ui, path: &std::path::Path) {
    let text = path.display().to_string();
    ui.add(egui::Label::new(&text).truncate())
        .on_hover_text(text);
}

/// A numbered card.
fn card(ui: &mut egui::Ui, number: &str, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(Color32::from_gray(32))
        .corner_radius(8.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(number).size(20.0).strong().color(ACCENT));
                ui.label(RichText::new(title).size(18.0).strong());
            });
            body(ui);
        });
    ui.add_space(6.0);
}

fn open_folder(dir: &std::path::Path) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(dir).spawn();
}
