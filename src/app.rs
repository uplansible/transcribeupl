use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Context, Layout};
use eframe::Frame;
use rfd::FileDialog;

use crate::archive::{archive_and_exit, archive_file};
use crate::audio::{decode_to_pcm_f32, seconds_to_index, DecodedAudio, Output, Player};
use crate::config::Config;
use crate::pedal::{PedalEvent, PedalManager, PedalScanStatus};
use crate::util::content_time_fmt;

#[derive(Debug, Clone)]
struct UiError {
    id: u64,
    text: String,
}

pub struct TranscribeApp {
    cfg: Config,
    cfg_missing: bool,
    cfg_path: PathBuf,

    // Audio
    player: Player,
    audio: Option<DecodedAudio>,
    current_file: Option<PathBuf>,

    // UI state
    speed_idx: usize, // 0..=3 for 0.75x,1.0x,1.25x,1.5x
    speeds: [f64; 4],

    show_archive_dialog: bool,
    last_error_id: u64,
    errors: Vec<UiError>,

    // Pedal
    pedal: PedalManager,
    pedal_status: String,
    left_hold: bool,
    last_left_rewind: Option<Instant>,

    // Timekeeping for progress while playing
    last_tick: Option<Instant>,
}

impl TranscribeApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (cfg, missing, cfg_path) = Config::load_or_default();
        if missing {
            log::warn!("Using default config. Will save to {}", cfg_path.display());
        } else {
            log::info!("Loaded config from {}", cfg_path.display());
        }

        let output = Output::new().expect("Audio output init failed");
        let player = Player::new(output);

        // Start pedal autoscan
        let pedal = PedalManager::new(cfg.clone());

        Self {
            cfg,
            cfg_missing: missing,
            cfg_path,
            player,
            audio: None,
            current_file: None,
            speed_idx: 1,
            speeds: [0.75, 1.0, 1.25, 1.5],
            show_archive_dialog: false,
            last_error_id: 0,
            errors: Vec::new(),
            pedal,
            pedal_status: "Not started".into(),
            left_hold: false,
            last_left_rewind: None,
            last_tick: None,
        }
    }

    fn push_error(&mut self, text: impl Into<String>) {
        self.last_error_id += 1;
        self.errors.push(UiError {
            id: self.last_error_id,
            text: text.into(),
        });
        log::error!("{}", self.errors.last().unwrap().text);
    }

    fn open_file_dialog(&mut self) {
        let start_dir = if !self.cfg.paths.default_open_dir.is_empty() {
            PathBuf::from(&self.cfg.paths.default_open_dir)
        } else {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
        };

        let dialog = FileDialog::new()
            .add_filter("Audio", &["mp3", "wav", "ogg", "opus"])
            .set_directory(start_dir);

        if let Some(path) = dialog.pick_file() {
            self.open_file(path);
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        match decode_to_pcm_f32(&path) {
            Ok(audio) => {
                self.audio = Some(audio.clone());
                self.current_file = Some(path.clone());
                self.player.current_index = 0;
                self.player.speed = self.speeds[self.speed_idx];
                // Prepare paused sink from start
                if let Err(e) = self.player.start_from(&audio, 0) {
                    self.push_error(format!("Audio start failed: {e}"));
                } else {
                    self.player.pause();
                }
            }
            Err(e) => {
                self.push_error(format!("Open failed: {e}"));
            }
        }
    }

    fn do_playpause(&mut self) {
        if self.audio.is_none() {
            return;
        }
        if self.player.is_playing() {
            self.player.pause();
        } else if let Some(audio) = &self.audio {
            let _ = self.player.start_from(audio, self.player.current_index);
            self.player.resume();
        }
    }

    fn seek_relative(&mut self, seconds: i32) {
        if let Some(audio) = &self.audio {
            let sr = audio.sample_rate;
            let ch = audio.channels;
            let cur = self.player.current_index;
            let delta = seconds_to_index(seconds as f64, sr, ch);
            let next = if seconds >= 0 {
                cur.saturating_add(delta)
            } else {
                cur.saturating_sub(delta)
            };
            let clamped = next.min(audio.total_samples);
            self.player.current_index = clamped;
            // Rebuild at new position
            let _ = self.player.start_from(audio, clamped);
            // If we were paused, keep paused
            if !self.player.is_playing() {
                self.player.pause();
            }
        }
    }

    fn apply_speed(&mut self, idx: usize) {
        self.speed_idx = idx;
        if let Some(audio) = &self.audio {
            self.player.set_speed(self.speeds[self.speed_idx], audio);
            if !self.player.is_playing() {
                self.player.pause();
            }
        }
    }

    fn handle_pedal_event(&mut self, ev: PedalEvent) {
        // Debounce: we already ignore repeats (value=2) in reader.
        // Map numeric codes from config.
        let m = &self.cfg.input.pedal_defaults; // For Phase 1, use defaults
        let start_rewind_s = self.cfg.application.play_start_rewind_seconds as i32;
        match (ev.code, ev.value) {
            (code, 1) if code == m.right_code => {
                // RightPress: immediate small rewind then play
                if self.audio.is_some() {
                    self.seek_relative(-start_rewind_s);
                    // Start/resume playback
                    self.player.resume();
                    if !self.player.is_playing() {
                        if let Some(audio) = &self.audio {
                            let _ = self.player.start_from(audio, self.player.current_index);
                            self.player.resume();
                        }
                    }
                }
            }
            (code, 0) if code == m.right_code => {
                // RightRelease: pause
                self.player.pause();
            }
            (code, 1) if code == m.left_code => {
                // LeftPress: begin repeated rewind while held
                self.left_hold = true;
                self.last_left_rewind = None; // trigger immediate rewind on first tick
            }
            (code, 0) if code == m.left_code => {
                // LeftRelease: stop loop
                self.left_hold = false;
                self.last_left_rewind = None;
            }
            (code, 1) if code == m.middle_code => {
                // MiddlePress: pause and open archive dialog
                self.player.pause();
                self.show_archive_dialog = true;
            }
            _ => {}
        }
    }

    fn process_pedal_inputs(&mut self, ctx: &Context) {
        // Update status
        if let Some(st) = self.pedal.try_recv_status() {
            self.pedal_status = match &st {
                PedalScanStatus::NotStarted => "Not started".into(),
                PedalScanStatus::Scanning => "Scanning for pedal...".into(),
                PedalScanStatus::Connected {
                    name,
                    path,
                    vendor,
                    product,
                } => {
                    format!(
                        "Connected: {} ({:04x}:{:04x}) {}",
                        name, vendor, product, path
                    )
                }
                PedalScanStatus::Error(e) => format!("Error: {}", e),
            };
            if matches!(&st, PedalScanStatus::Error(_)) {
                // Pause on disconnect error
                self.player.pause();
                // Show concise user-visible error
                self.push_error("Pedal disconnected");
            }
        }

        // Events
        for ev in self.pedal.try_recv_events() {
            self.handle_pedal_event(ev);
        }

        // Handle left-hold repeated rewinds while playback continues
        if self.left_hold {
            let now = Instant::now();
            let interval = Duration::from_millis(self.cfg.application.hold_rewind_interval_ms);
            let do_rewind = match self.last_left_rewind {
                None => true,
                Some(last) => now.duration_since(last) >= interval,
            };
            if do_rewind {
                let amount = -(self.cfg.application.rewind_seconds as i32);
                self.seek_relative(amount);
                self.last_left_rewind = Some(now);
            }
            // Request frequent repaints during hold
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

impl eframe::App for TranscribeApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        self.process_pedal_inputs(ctx);

        // Update content-time position estimate during playback
        if let (Some(audio), true) = (&self.audio, self.player.is_playing()) {
            let now = Instant::now();
            if let Some(prev) = self.last_tick {
                let dt = now.saturating_duration_since(prev).as_secs_f64();
                let inc = (dt
                    * audio.sample_rate as f64
                    * audio.channels as f64
                    * self.speeds[self.speed_idx]) as usize;
                if inc > 0 {
                    let new_idx = self
                        .player
                        .current_index
                        .saturating_add(inc)
                        .min(audio.total_samples);
                    self.player.current_index = new_idx;
                }
            }
            self.last_tick = Some(now);
        } else {
            self.last_tick = Some(Instant::now());
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            // Errors area (dismissible)
            if !self.errors.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    let mut to_remove: Vec<u64> = Vec::new();
                    for e in &self.errors {
                        ui.colored_label(egui::Color32::RED, &e.text);
                        if ui.button("Dismiss").clicked() {
                            to_remove.push(e.id);
                        }
                    }
                    if !to_remove.is_empty() {
                        self.errors.retain(|ee| !to_remove.contains(&ee.id));
                    }
                    // If too many, keep the latest few
                    if self.errors.len() > 3 {
                        let keep_from = self.errors.len() - 3;
                        self.errors.drain(0..keep_from);
                    }
                });
            }

            ui.horizontal(|ui| {
                if ui.button("Open").clicked() {
                    self.open_file_dialog();
                }

                if ui
                    .button(if self.player.is_playing() {
                        "Pause"
                    } else {
                        "Play"
                    })
                    .clicked()
                {
                    self.do_playpause();
                }

                if ui.button("Rewind").clicked() {
                    let s = -(self.cfg.application.rewind_seconds as i32);
                    self.seek_relative(s);
                }

                if ui.button("Forward").clicked() {
                    let s = self.cfg.application.forward_seconds as i32;
                    self.seek_relative(s);
                }

                ui.menu_button(format!("Speed: {}x", self.speeds[self.speed_idx]), |ui| {
                    let speeds = self.speeds; // copy array to avoid borrow issues
                    for (i, sp) in speeds.iter().enumerate() {
                        if ui.button(format!("{:.2}x", sp)).clicked() {
                            self.apply_speed(i);
                            ui.close_menu();
                        }
                    }
                });

                if ui.button("Archive").clicked() {
                    self.player.pause();
                    self.show_archive_dialog = true;
                }

                ui.separator();
                ui.label(self.pedal_status.clone());
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                // Filename
                ui.label(
                    self.current_file
                        .as_ref()
                        .and_then(|p| p.file_name().and_then(|s| s.to_str()))
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "No file selected".to_string()),
                );

                // Time + progress
                let (time_l, time_r, frac) = if let Some(audio) = &self.audio {
                    let (l, r) = content_time_fmt(
                        self.player.current_index,
                        audio.total_samples,
                        audio.sample_rate,
                        audio.channels,
                    );
                    let frac = if audio.total_samples > 0 {
                        (self.player.current_index as f32 / audio.total_samples as f32)
                            .clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    (l, r, frac)
                } else {
                    ("00:00".into(), "00:00".into(), 0.0)
                };

                ui.label(format!("{time_l} / {time_r}"));
                ui.add(egui::ProgressBar::new(frac).desired_width(ui.available_width()));

                // Archive dialog
                if self.show_archive_dialog {
                    egui::Window::new("Archive")
                        .collapsible(false)
                        .resizable(false)
                        .show(ctx, |ui| {
                            ui.label("Archive the current file?");
                            ui.horizontal(|ui| {
                                if ui.button("Archive").clicked() {
                                    if let Some(path) = &self.current_file {
                                        match archive_file(path) {
                                            crate::archive::ArchiveResult::Success(_dst) => {
                                                // Return to empty selection; leave app running.
                                                self.current_file = None;
                                                self.audio = None;
                                                self.player.stop();
                                                self.show_archive_dialog = false;
                                            }
                                            crate::archive::ArchiveResult::Error(e) => {
                                                self.push_error(format!("Archive failed: {e}"));
                                            }
                                        }
                                    } else {
                                        self.push_error("No file to archive");
                                    }
                                }
                                if ui.button("Continue").clicked() {
                                    // Close dialog; leave playback paused
                                    self.show_archive_dialog = false;
                                }
                                if ui.button("Exit").clicked() {
                                    if let Some(path) = &self.current_file {
                                        match archive_and_exit(path) {
                                            crate::archive::ArchiveResult::Success(_) => {
                                                // Request app close
                                                frame.close();
                                            }
                                            crate::archive::ArchiveResult::Error(e) => {
                                                self.push_error(format!("Archive failed: {e}"));
                                            }
                                        }
                                    } else {
                                        frame.close();
                                    }
                                }
                            });
                        });
                }
            });
        });

        // Keep UI updating during playback/holds
        if self.player.is_playing() || self.left_hold {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}
