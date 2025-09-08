use crossbeam_channel::{unbounded, Receiver, Sender};
use evdev::{Device, InputEventKind};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::config::{Config, PedalModel};

#[derive(Debug, Clone)]
pub enum PedalScanStatus {
    NotStarted,
    Scanning,
    Connected {
        name: String,
        path: String,
        vendor: u16,
        product: u16,
    },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct PedalEvent {
    pub code: u32,
    pub value: i32, // 1 press, 0 release, 2 repeat (ignored)
}

pub struct PedalManager {
    status_rx: Receiver<PedalScanStatus>,
    event_rx: Receiver<PedalEvent>,
}

impl PedalManager {
    pub fn new(config: Config) -> Self {
        let (status_tx, status_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();

        // Spawn scanner thread
        std::thread::spawn(move || scanner_loop(config, status_tx, event_tx));

        Self {
            status_rx,
            event_rx,
        }
    }

    pub fn try_recv_status(&self) -> Option<PedalScanStatus> {
        self.status_rx.try_recv().ok()
    }

    pub fn try_recv_events(&self) -> Vec<PedalEvent> {
        self.event_rx.try_iter().collect()
    }
}

fn scanner_loop(config: Config, status_tx: Sender<PedalScanStatus>, event_tx: Sender<PedalEvent>) {
    status_tx.send(PedalScanStatus::Scanning).ok();
    let detect_list = config.pedal_detection_list();
    loop {
        match find_matching_device(&detect_list, &config.input.device_path) {
            Ok((dev, path, _mdl)) => {
                let name = dev.name().unwrap_or("unknown").to_string();
                let id = dev.input_id();
                status_tx
                    .send(PedalScanStatus::Connected {
                        name: name.clone(),
                        path: path.to_string_lossy().to_string(),
                        vendor: id.vendor(),
                        product: id.product(),
                    })
                    .ok();
                log::info!(
                    "Pedal connected: {} @ {} (vid={:04x} pid={:04x})",
                    name,
                    path.display(),
                    id.vendor(),
                    id.product()
                );
                // Read loop
                let res = read_loop(dev, &event_tx);
                status_tx
                    .send(PedalScanStatus::Error(format!("Pedal disconnected: {res}")))
                    .ok();
                log::warn!("Pedal disconnected: {}", res);
                // fall through to rescan
            }
            Err(e) => {
                status_tx
                    .send(PedalScanStatus::Error(format!("Scan error: {e}")))
                    .ok();
                thread::sleep(Duration::from_millis(1500));
            }
        }
        // Retry
        status_tx.send(PedalScanStatus::Scanning).ok();
        thread::sleep(Duration::from_millis(1000));
    }
}

// Prefer vendor/product detection; fallback to explicit device_path if set.
fn find_matching_device(
    list: &[PedalModel],
    device_path: &str,
) -> Result<(Device, PathBuf, PedalModel), String> {
    // 1) vendor/product (default first, then others by list order)
    let mut event_paths: Vec<PathBuf> = fs::read_dir("/dev/input")
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.starts_with("event"))
                .unwrap_or(false)
        })
        .collect();
    event_paths.sort();

    for mdl in list {
        for p in &event_paths {
            if let Ok(dev) = Device::open(p) {
                let id = dev.input_id();
                if id.vendor() == mdl.vendor_id && id.product() == mdl.product_id {
                    return Ok((dev, p.clone(), mdl.clone()));
                }
            }
        }
    }

    // 2) fallback to explicit path if provided
    if !device_path.is_empty() {
        let p = PathBuf::from(device_path);
        let dev = Device::open(&p).map_err(|e| e.to_string())?;
        let id = dev.input_id();
        let fallback = list.first().cloned().unwrap_or_else(|| PedalModel {
            name: "Fallback".to_string(),
            vendor_id: id.vendor(),
            product_id: id.product(),
            left_code: 288,
            middle_code: 290,
            right_code: 289,
        });
        return Ok((dev, p, fallback));
    }

    Err("No matching pedal found".into())
}

fn read_loop(mut dev: Device, tx: &Sender<PedalEvent>) -> String {
    loop {
        match dev.fetch_events() {
            Ok(events) => {
                for ev in events {
                    if let InputEventKind::Key(k) = ev.kind() {
                        let code = k.code() as u32;
                        let value = ev.value();
                        // value: 1=press, 0=release, 2=repeat (ignore)
                        if value == 0 || value == 1 {
                            let _ = tx.send(PedalEvent { code, value });
                        }
                    }
                }
            }
            Err(e) => return e.to_string(),
        }
    }
}
