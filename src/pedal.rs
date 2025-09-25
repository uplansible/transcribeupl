use crate::config::Config;
use evdev::Device;
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum PedalStatus {
    Scanning,
    Connected { name: String, path: PathBuf },
    NotFound,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct PedalEvent {
    pub code: u32,  // key code
    pub value: i32, // 1=press, 0=release, 2=repeat(ignored)
}

#[derive(Debug)]
pub enum PedalMsg {
    Status(PedalStatus),
    Input(PedalEvent),
    Disconnected,
}

pub struct PedalManager {
    tx: Sender<PedalMsg>,
    _handle: thread::JoinHandle<()>,
}

impl PedalManager {
    pub fn start(cfg: Config, tx: Sender<PedalMsg>) -> Self {
        let tx_for_struct = tx.clone();
        let handle = thread::Builder::new()
            .name("pedal-manager".into())
            .spawn(move || run_manager(cfg, tx))
            .expect("Failed to spawn pedal manager");
        Self {
            tx: tx_for_struct,
            _handle: handle,
        }
    }
}

fn preferred_device_paths(cfg: &Config) -> Vec<Preferred> {
    let mut v = Vec::new();

    // 1) Default pedal (highest priority)
    v.push(Preferred::VidPid {
        vid: 0x0911,
        pid: 0x1844,
    });

    // 2) Configured pedals in listed order (descending by list order)
    for p in &cfg.pedals {
        v.push(Preferred::VidPid {
            vid: p.vendor_id,
            pid: p.product_id,
        });
    }

    // 3) Fallback explicit device path if provided
    if let Some(p) = &cfg.input.device_path {
        v.push(Preferred::Path(p.clone()));
    }

    v
}

enum Preferred {
    VidPid { vid: u16, pid: u16 },
    Path(PathBuf),
}

fn run_manager(cfg: Config, tx: Sender<PedalMsg>) {
    let mut last_report = Instant::now() - Duration::from_secs(10);
    loop {
        if last_report.elapsed() >= Duration::from_secs(1) {
            let _ = tx.send(PedalMsg::Status(PedalStatus::Scanning));
            last_report = Instant::now();
        }

        let prefs = preferred_device_paths(&cfg);

        // Scan for a matching device
        match find_device(&prefs) {
            Ok(Some((path, dev))) => {
                let name = dev.name().unwrap_or("Unknown").to_string();
                let _ = tx.send(PedalMsg::Status(PedalStatus::Connected {
                    name: name.clone(),
                    path: path.clone(),
                }));
                info!("Pedal connected: {} @ {}", name, path.display());

                // Read events until disconnect/error
                if let Err(e) = read_events_loop(dev, &tx) {
                    warn!("Pedal disconnected or error: {}", e);
                }
                let _ = tx.send(PedalMsg::Disconnected);
                // Back to scanning
            }
            Ok(None) => {
                let _ = tx.send(PedalMsg::Status(PedalStatus::NotFound));
                thread::sleep(Duration::from_millis(2000));
            }
            Err(e) => {
                let _ = tx.send(PedalMsg::Status(PedalStatus::Error(e.to_string())));
                thread::sleep(Duration::from_millis(2000));
            }
        }
    }
}

fn find_device(prefs: &[Preferred]) -> anyhow::Result<Option<(PathBuf, Device)>> {
    // Snapshot of /dev/input event devices
    let devices: Vec<(PathBuf, Device)> = evdev::enumerate().collect();

    for pref in prefs {
        match pref {
            Preferred::VidPid { vid, pid } => {
                for (path, dev) in devices.iter() {
                    let id = dev.input_id();
                    if id.vendor() == *vid && id.product() == *pid {
                        // Try opening a fresh handle to the device
                        match Device::open(path) {
                            Ok(devc) => return Ok(Some((path.clone(), devc))),
                            Err(e) => {
                                debug!("Failed to open {}: {}", path.display(), e);
                            }
                        }
                    }
                }
            }
            Preferred::Path(p) => {
                if Path::new(p).exists() {
                    match Device::open(p) {
                        Ok(devc) => return Ok(Some((p.clone(), devc))),
                        Err(e) => {
                            debug!("Failed to open {}: {}", p.display(), e);
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

fn read_events_loop(mut dev: Device, tx: &Sender<PedalMsg>) -> anyhow::Result<()> {
    loop {
        match dev.fetch_events() {
            Ok(events) => {
                for ev in events {
                    use evdev::InputEventKind;
                    if let InputEventKind::Key(k) = ev.kind() {
                        let code_u16 = k.code();
                        let v = ev.value();
                        let _ = tx.send(PedalMsg::Input(PedalEvent {
                            code: code_u16 as u32,
                            value: v,
                        }));
                    }
                }
            }
            Err(e) => {
                // device likely disconnected or unreadable
                return Err(e.into());
            }
        }
    }
}
