mod app;
mod archive;
mod audio;
mod config;
mod pedal;
mod util;

use crate::app::TranscribeApp;
use eframe::NativeOptions;
use env_logger::Env;

fn main() -> eframe::Result<()> {
    // Configure 24-hour timestamps.
    let mut builder = env_logger::Builder::from_env(Env::default().default_filter_or("info"));
    builder.format_timestamp_secs();
    builder.init();

    log::info!("Starting transcribeupl...");

    let native_options = NativeOptions::default();
    eframe::run_native(
        "transcribeupl",
        native_options,
        Box::new(|cc| Box::new(TranscribeApp::new(cc))),
    )
}
