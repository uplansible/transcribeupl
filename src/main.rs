mod audio;
mod config;
mod pedal;
mod ui_time;

use crate::audio::Player;
use crate::config::Config;
use crate::pedal::{PedalEvent, PedalManager, PedalMsg, PedalStatus};
use crate::ui_time::format_clock;

use chrono::{Datelike, Local};
use eframe::egui;
use egui::Color32;
use log::{error, info, warn};
use rfd::FileDialog;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct UiError {
    id: u64,
    msg: String,
    ts: Instant,
}

struct App {
    cfg: Config,
    player: Player,

    // UI state
    errors: Vec<UiError>,
    next_err_id: u64,

    // Pedal
    pedal_status: PedalStatus,
    pedal_rx: mpsc::Receiver<PedalMsg>,
    _pedal_mgr: PedalManager,
    // Tracking pressed state for debounce
    left_pressed: bool,
    right_pressed: bool,
    middle_pressed: bool,
    // Codes (current mapping in effect)
    left_code: u32,
    right_code: u32,
    middle_code: u32,

    // Repeated rewind
    hold_last_tick: Option<Instant>,

    // Archive dialog
    show_archive_dialog: bool,
    archive_error: Option<String>,
    archive_pending_exit: bool,

    // Quit
    request_close: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        cc.egui_ctx.set_pixels_per_point(1.0);

        // Audio player
        let player = Player::new().expect("Audio output init failed");

        // Logging initial
        info!("App start");

        // Pedal manager
        let (tx, rx) = mpsc::channel::<PedalMsg>();
        let mgr = PedalManager::start(cfg.clone(), tx);

        // Codes from defaults or selected model
        let (l, m, r) = if let Some(name) = &cfg.input.selected_model {
            if let Some(model) = cfg.pedals.iter().find(|p| &p.name == name) {
                (model.left_code, model.middle_code, model.right_code)
            } else {
                warn!("Selected model '{}' not found; using defaults", name);
                (
                    cfg.pedal_defaults.left_code,
                    cfg.pedal_defaults.middle_code,
                    cfg.pedal_defaults.right_code,
                )
            }
        } else {
            (
                cfg.pedal_defaults.left_code,
                cfg.pedal_defaults.middle_code,
                cfg.pedal_defaults.right_code,
            )
        };

        Self {
            cfg,
            player,

            errors: Vec::new(),
            next_err_id: 1,

            pedal_status: PedalStatus::Scanning,
            pedal_rx: rx,
            _pedal_mgr: mgr,

            left_pressed: false,
            right_pressed: false,
            middle_pressed: false,

            left_code: l,
            right_code: r,
            middle_code: m,

            hold_last_tick: None,

            show_archive_dialog: false,
            archive_error: None,
            archive_pending_exit: false,

            request_close: false,
        }
    }

    fn push_error(&mut self, msg: impl Into<String>) {
        let id = self.next_err_id;
        self.next_err_id += 1;
        let msg = msg.into();
        error!("{}", msg);
        self.errors.push(UiError {
            id,
            msg,
            ts: Instant::now(),
        });
    }

    fn drain_pedal_msgs(&mut self) {
        while let Ok(msg) = self.pedal_rx.try_recv() {
            match msg {
                PedalMsg::Status(s) => {
                    self.pedal_status = s.clone();
                    match &s {
                        PedalStatus::Connected { name, path } => {
                            info!("Pedal connected: {} @ {}", name, path.display());
                        }
                        PedalStatus::Scanning => {}
                        PedalStatus::NotFound => {}
                        PedalStatus::Error(e) => {
                            self.push_error(format!("Pedal error: {}", e));
                        }
                    }
                }
                PedalMsg::Disconnected => {
                    // Pause playback immediately
                    self.player.pause();
                    self.push_error("Pedal disconnected");
                }
                PedalMsg::Input(ev) => {
                    self.handle_pedal_event(ev);
                }
            }
        }
    }

    fn handle_pedal_event(&mut self, ev: PedalEvent) {
        // Ignore repeats
        if ev.value == 2 {
            return;
        }
        let is_press = ev.value == 1;
        let code = ev.code;

        if code == self.right_code {
            // Debounce
            if is_press && !self.right_pressed {
                self.right_pressed = true;
                // RightPress: seek back by play_start_rewind_seconds and start playback
                let back = -(self.cfg.application.play_start_rewind_seconds as i64);
                self.player.seek_seconds(back);
                self.player.play_from_current();
            } else if !is_press && self.right_pressed {
                self.right_pressed = false;
                // RightRelease: pause immediately
                self.player.pause();
            }
            return;
        }

        if code == self.left_code {
            if is_press && !self.left_pressed {
                self.left_pressed = true;
                self.hold_last_tick = Some(Instant::now());
                // No immediate seek; first action occurs after interval.
            } else if !is_press && self.left_pressed {
                self.left_pressed = false;
                self.hold_last_tick = None;
            }
            return;
        }

        if code == self.middle_code {
            if is_press && !self.middle_pressed {
                self.middle_pressed = true;
                // MiddlePress: pause playback and open archive dialog
                self.player.pause();
                self.show_archive_dialog = true;
            } else if !is_press && self.middle_pressed {
                self.middle_pressed = false;
            }
            return;
        }
    }

    fn tick_hold_rewind(&mut self) {
        if !self.left_pressed {
            return;
        }
        let Some(last) = self.hold_last_tick else {
            return;
        };
        let interval = Duration::from_millis(self.cfg.application.hold_rewind_interval_ms);
        if last.elapsed() >= interval {
            // Rewind while playing continues
            let back = -(self.cfg.application.rewind_seconds as i64);
            self.player.seek_seconds(back);
            self.hold_last_tick = Some(Instant::now());
        }
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        // Buttons: Open, Play/Pause, Rewind, Forward, Speed dropdown, Archive
        if ui.button("Open").clicked() {
            let start_dir = self.cfg.resolve_default_open_dir();
            if let Some(path) = FileDialog::new()
                .set_directory(start_dir)
                .add_filter("Audio", &["mp3", "wav", "ogg", "opus"])
                .pick_file()
            {
                match self.player.load_file(&path) {
                    Ok(()) => {
                        info!("Opened file: {}", path.display());
                    }
                    Err(e) => {
                        self.push_error(format!("Open failed: {}", e));
                    }
                }
            }
        }

        let can_control = self.player.audio.is_some();

        if ui
            .add_enabled(
                can_control,
                egui::Button::new(if self.player.playing { "Pause" } else { "Play" }),
            )
            .clicked()
        {
            if self.player.playing {
                self.player.pause();
            } else {
                self.player.play_from_current();
            }
        }

        if ui
            .add_enabled(can_control, egui::Button::new("Rewind"))
            .clicked()
        {
            let back = -(self.cfg.application.rewind_seconds as i64);
            self.player.seek_seconds(back);
        }

        if ui
            .add_enabled(can_control, egui::Button::new("Forward"))
            .clicked()
        {
            self.player
                .seek_seconds(self.cfg.application.forward_seconds as i64);
        }

        ui.separator();

        egui::ComboBox::from_label("Speed")
            .selected_text(format!("{:.2}x", self.player.speed))
            .show_ui(ui, |ui| {
                for s in [0.75_f32, 1.0, 1.25, 1.5] {
                    if ui
                        .selectable_label(
                            (self.player.speed - s).abs() < 1e-3,
                            format!("{:.2}x", s),
                        )
                        .clicked()
                    {
                        self.player.set_speed(s);
                    }
                }
            });

        ui.separator();

        if ui
            .add_enabled(can_control, egui::Button::new("Archive"))
            .clicked()
        {
            self.player.pause();
            self.show_archive_dialog = true;
        }

        ui.separator();

        // Status and Errors
        let pedal_text = match &self.pedal_status {
            PedalStatus::Scanning => "Pedal: Scanning".to_owned(),
            PedalStatus::Connected { name, path } => {
                format!("Pedal: Connected ({}, {})", name, path.display())
            }
            PedalStatus::NotFound => "Pedal: Not found".to_owned(),
            PedalStatus::Error(e) => format!("Pedal: Error ({})", e),
        };
        ui.label(pedal_text);

        ui.separator();

        // Show dismissible errors (non-fatal) in red
        let mut to_remove: Vec<u64> = Vec::new();
        for e in &self.errors {
            ui.colored_label(Color32::RED, format!("Error: {}", e.msg));
        }
        if !self.errors.is_empty() {
            if ui.button("Dismiss errors").clicked() {
                to_remove.extend(self.errors.iter().map(|e| e.id));
            }
        }
        if !to_remove.is_empty() {
            self.errors.retain(|e| !to_remove.contains(&e.id));
        }
    }

    fn ui_central(&mut self, ui: &mut egui::Ui) {
        let name = self
            .player
            .file_path
            .as_ref()
            .map(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(invalid)")
            })
            .unwrap_or("No file selected");
        ui.heading(name);

        // Time/progress
        let (cur, total) = self.player.current_time_secs();
        ui.label(format_clock(cur, total));

        // Progress bar (read-only)
        let frac = if let Some(audio) = &self.player.audio {
            let idx = self.player.current_index_interleaved();
            if audio.total_samples == 0 {
                0.0
            } else {
                (idx as f32) / (audio.total_samples as f32)
            }
        } else {
            0.0
        };
        ui.add(egui::ProgressBar::new(frac).show_percentage());
    }

    fn ui_archive_dialog(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.show_archive_dialog {
            return;
        }
        egui::Window::new("Archive")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if let Some(err) = &self.archive_error {
                    ui.colored_label(Color32::RED, err);
                }

                ui.horizontal(|ui| {
                    if ui.button("Archive").clicked() {
                        match self.do_archive(false) {
                            Ok(()) => {
                                self.show_archive_dialog = false;
                            }
                            Err(e) => {
                                self.archive_error = Some(format!("Archive failed: {}", e));
                            }
                        }
                    }
                    if ui.button("Continue").clicked() {
                        self.show_archive_dialog = false; // leave playback paused
                        self.archive_error = None;
                    }
                    if ui.button("Exit").clicked() {
                        self.archive_pending_exit = true;
                        match self.do_archive(true) {
                            Ok(()) => {
                                self.request_close = true;
                            }
                            Err(e) => {
                                self.archive_error = Some(format!("Archive failed: {}", e));
                                self.archive_pending_exit = false;
                            }
                        }
                    }
                });
            });
        if self.request_close {
            frame.close();
        }
    }

    fn do_archive(&mut self, _exit_after: bool) -> anyhow::Result<()> {
        // Move/copy file, then unload
        let Some(src) = self.player.file_path.clone() else {
            return Err(anyhow::anyhow!("No file selected"));
        };

        // ./archive/YYYY/MM
        let now = Local::now();
        let dest_dir = PathBuf::from("./archive")
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", now.month()));
        std::fs::create_dir_all(&dest_dir)?;

        // Append suffix _YYYYMMDD_HHMMSS before extension
        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
        let ts = now.format("%Y%m%d_%H%M%S").to_string();
        let filename = if ext.is_empty() {
            format!("{}_{}", stem, ts)
        } else {
            format!("{}_{}.{}", stem, ts, ext)
        };
        let dest = dest_dir.join(filename);

        // Try rename first
        match std::fs::rename(&src, &dest) {
            Ok(()) => {
                info!("Archived (rename): {} -> {}", src.display(), dest.display());
            }
            Err(_) => {
                // Copy then delete
                std::fs::copy(&src, &dest)?;
                std::fs::remove_file(&src)?;
                info!(
                    "Archived (copy+delete): {} -> {}",
                    src.display(),
                    dest.display()
                );
            }
        }

        // Return to "No file selected"
        self.player.unload();

        Ok(())
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Drain pedal messages
        self.drain_pedal_msgs();

        // Handle repeated rewind if left is pressed
        self.tick_hold_rewind();

        // Clamp at end
        self.player.clamp_at_end_if_needed();

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_central(ui);
        });

        self.ui_archive_dialog(ctx, frame);

        // Request periodic repaints to drive timing and hold-rewind ticks
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn on_close_event(&mut self) -> bool {
        true
    }
}

fn init_logger() {
    use env_logger::{Builder, Env};
    let env = Env::default().default_filter_or("info");
    Builder::from_env(env)
        .format(|buf, record| {
            let now = chrono::Local::now();
            writeln!(
                buf,
                "{} [{}] {}",
                now.format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.args()
            )
        })
        .init();
}

fn main() -> eframe::Result<()> {
    init_logger();

    let cfg = Config::load_or_default();

    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::vec2(900.0, 300.0)),
        ..Default::default()
    };

    eframe::run_native(
        "transcribeupl",
        options,
        Box::new(|cc| Box::new(App::new(cc, cfg))),
    )
}
