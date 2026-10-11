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

use crate::pipeline::{self, ABIS, Check, Event, Job, RomInfo, Stage};

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
}

impl Studio {
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>, rom: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 8.0);
            s.spacing.button_padding = egui::vec2(12.0, 6.0);
        });
        let mut studio = Self {
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
        };
        studio.refresh_checks();
        if let Some(path) = rom {
            studio.load_rom(path);
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
        self.checks = pipeline::check_tools(&self.chosen_abis());
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
            match rx.try_recv() {
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
            self.refresh_checks();
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
                ui.label(RichText::new("Use a ROM you own: it is built into the app.").color(DIM));
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
        card(ui, "2", "The app", |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                egui::Grid::new("app").num_columns(2).show(ui, |ui| {
                    ui.label(RichText::new("Name").color(DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.app_name).desired_width(320.0));
                    ui.end_row();
                    ui.label(RichText::new("Identifier").color(DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.app_id).desired_width(320.0));
                    ui.end_row();
                    ui.label("");
                    match pipeline::check_app_id(self.app_id.trim()) {
                        Ok(()) => ui.label(
                            RichText::new("installs as its own app, next to gase").color(DIM),
                        ),
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
                        RichText::new(
                            "arm64-v8a runs on nearly every phone; each one compiles the game again",
                        )
                        .color(DIM),
                    );
                    ui.end_row();
                    ui.label(RichText::new("Folder").color(DIM));
                    path_label(ui, &self.out);
                    ui.end_row();
                });
            });
        });
    }

    fn build_card(&mut self, ui: &mut egui::Ui) {
        let tools_ok = self.checks.iter().all(|c| c.ok);
        let ready = self.rom.is_some()
            && !self.busy()
            && tools_ok
            && pipeline::check_app_id(self.app_id.trim()).is_ok()
            && !self.chosen_abis().is_empty();
        card(ui, "3", "Build", |ui| {
            ui.horizontal(|ui| {
                let button = egui::Button::new(RichText::new("Build the APK").size(18.0).strong())
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
            for stage in Stage::ALL {
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
                        ui.label(RichText::new("Your app is ready:").color(GOOD).strong());
                        path_label(ui, &apk);
                    });
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
    }
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
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
                ui.label(RichText::new("pick a ROM · press Build · get an Android app").color(DIM));
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
