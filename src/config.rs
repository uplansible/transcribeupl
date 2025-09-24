use directories::ProjectDirs;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

pub const DEFAULT_VENDOR_ID: u16 = 0x0911;
pub const DEFAULT_PRODUCT_ID: u16 = 0x1844;
pub const DEFAULT_LEFT_CODE: u32 = 288;
pub const DEFAULT_MIDDLE_CODE: u32 = 290;
pub const DEFAULT_RIGHT_CODE: u32 = 289;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    pub default_open_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationConfig {
    pub rewind_seconds: u32,
    pub forward_seconds: u32,
    pub hold_rewind_interval_ms: u64,
    pub play_start_rewind_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    pub device_path: Option<PathBuf>,
    pub selected_model: Option<String>,
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
pub struct PedalDefaults {
    pub vendor_id: u16,
    pub product_id: u16,
    pub left_code: u32,
    pub middle_code: u32,
    pub right_code: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub paths: PathsConfig,
    pub application: ApplicationConfig,
    pub input: InputConfig,
    pub pedal_defaults: PedalDefaults,
    #[serde(default)]
    pub pedals: Vec<PedalModel>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            paths: PathsConfig {
                default_open_dir: PathBuf::from(
                    "/run/user/1000/gvfs/smb-share:server=100.99.88.66,share=daten/diktat",
                ),
            },
            application: ApplicationConfig {
                rewind_seconds: 3,
                forward_seconds: 3,
                hold_rewind_interval_ms: 500,
                play_start_rewind_seconds: 1,
            },
            input: InputConfig {
                device_path: None,
                selected_model: None,
            },
            pedal_defaults: PedalDefaults {
                vendor_id: DEFAULT_VENDOR_ID,
                product_id: DEFAULT_PRODUCT_ID,
                left_code: DEFAULT_LEFT_CODE,
                middle_code: DEFAULT_MIDDLE_CODE,
                right_code: DEFAULT_RIGHT_CODE,
            },
            pedals: vec![],
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        if let Some(pd) = ProjectDirs::from("com", "transcribeupl", "transcribeupl") {
            let path = pd.config_dir().to_path_buf();
            std::fs::create_dir_all(&path).ok();
            return path.join("config.toml");
        }
        // Fallback to ~/.config/transcribeupl/config.toml
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let path = home.join(".config/transcribeupl");
        std::fs::create_dir_all(&path).ok();
        path.join("config.toml")
    }

    pub fn load_or_default() -> Self {
        let path = Self::config_path();
        match fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(cfg) => {
                    info!("Loaded config from {}", path.display());
                    cfg
                }
                Err(e) => {
                    warn!(
                        "Failed to parse config at {}: {}. Using defaults.",
                        path.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(_) => {
                warn!("Config not found at {}. Using defaults.", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_path();
        let s = toml::to_string_pretty(self)?;
        fs::write(&path, s)?;
        Ok(())
    }

    pub fn resolve_default_open_dir(&self) -> PathBuf {
        let p = &self.paths.default_open_dir;
        if p.exists() && p.is_dir() {
            return p.clone();
        }
        // Fallbacks: HOME, CWD
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            if home.is_dir() {
                return home;
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}
