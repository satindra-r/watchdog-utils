use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Deserialize, Clone)]
pub struct KeyhouseConf {
    pub base_url: String,
    pub token: String,
}
pub static LOGGER: OnceLock<String> = OnceLock::new();
pub fn get_log_target() -> &'static str {
    LOGGER.get().expect("log target not set").as_str()
}

pub fn set_log_target(log_target: String) {
    LOGGER.set(log_target).expect("log target already set");
}
