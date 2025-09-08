use serde::{Deserialize, Serialize};
use std::{fs, io, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    pub default_open_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationConfig {
    pub rewind_seconds: u32,
    pub forward_seconds: u32,
    pub hold_rewind_interval_ms: u64,
    pub play_start_rewind_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PedalModel {
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub left_code: u32,
    pub middle_code: u32,
    pub right_code: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    // Path is optional and used as a fallback. Primary detection is vendor/product.
    pub device_path: String,
    pub selected_model: String,
    pub pedals: Vec<PedalModel>,
    pub pedal_defaults: PedalModel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub paths: PathsConfig,
    pub application: ApplicationConfig,
    pub input: InputConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            paths: PathsConfig {
                default_open_dir:
                    "/run/user/1000/gvfs/smb-share:server=100.99.88.66,share=daten/diktat"
                        .to_string(),
            },
            application: ApplicationConfig {
                rewind_seconds: 3,
                forward_seconds: 3,
                hold_rewind_interval_ms: 500,
                play_start_rewind_seconds: 1,
            },
            input: InputConfig {
                device_path: String::new(),
                selected_model: String::new(),
                pedals: vec![],
                pedal_defaults: PedalModel {
                    name: "Default".to_string(),
                    vendor_id: 0x0911,
                    product_id: 0x1844,
                    left_code: 288,
                    middle_code: 290,
                    right_code: 289,
                },
            },
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Some(base) = dirs::config_dir() {
        base.join("transcribeupl").join("config.toml")
    } else {
        // Fallback
        PathBuf::from(".").join("config.toml")
    }
}

impl Config {
    pub fn load_or_default() -> (Self, bool, PathBuf) {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(cfg) => (cfg, false, path),
                Err(e) => {
                    log::warn!("Failed to parse config at {}: {e}", path.display());
                    (Self::default(), true, path)
                }
            },
            Err(_e) => {
                log::warn!("No config found at {}. Using defaults.", path.display());
                (Self::default(), true, path)
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let s = toml::to_string_pretty(self).unwrap();
        fs::write(path, s)
    }

    // Resolve the pedal detection order:
    // 1) default vendor/product
    // 2) additional configured pedals in their listed order
    pub fn pedal_detection_list(&self) -> Vec<PedalModel> {
        let mut list = Vec::new();
        list.push(self.input.pedal_defaults.clone());
        for p in &self.input.pedals {
            list.push(p.clone());
        }
        list
    }
}
