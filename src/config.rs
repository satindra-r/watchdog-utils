use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Deserialize, Clone)]
pub struct KeyhouseConf {
    pub base_url: String,
    pub token: String,
}

#[derive(Deserialize, Clone)]
pub struct Config {
    pub hostname: String,
    pub keyhouse: KeyhouseConf,
    pub cache_path: PathBuf, // Recieved from main watchdog
    pub branch: String,
}

impl Config {
    pub fn new(hostname: String, mut keyhouse: KeyhouseConf, cache_path: PathBuf) -> Self {
        let raw_url = keyhouse.base_url.clone();
        let clean_url = raw_url
            .trim_end_matches('/')
            .strip_suffix("/contents")
            .unwrap_or(raw_url.trim_end_matches('/'))
            .trim_end_matches('/')
            .to_string();

        keyhouse.base_url = clean_url;

        Config {
            hostname,
            keyhouse,
            cache_path,
            branch: "build".to_string(),
        }
    }
}

pub static LOGGER: OnceLock<String> = OnceLock::new();
pub fn get_log_target() -> &'static str {
    LOGGER.get().expect("log target not set").as_str()
}

pub fn set_log_target(log_target: String) {
    LOGGER.set(log_target).expect("log target already set");
}

pub fn init(config: &Config) -> Result<()> {
    let path = Path::new(&config.cache_path);
    if path.exists() && !path.is_dir() {
        return Err(anyhow!("Cache path {:?} is invalid", path));
    }
    fs::create_dir_all(path)?;
    Ok(())
}

pub fn test_init(cache: &str) -> Config {
    dotenvy::dotenv().ok();
    let test_keyhouse = KeyhouseConf {
        base_url: "https://api.github.com/repos/satindra-r/watchdog-utils".to_string(),
        token: std::env::var("token").expect("token not found in .env"),
    };

    Config {
        hostname: "centos".to_string(),
        keyhouse: test_keyhouse,
        cache_path: PathBuf::from(cache),
        branch: "build".to_string(),
    }
}
