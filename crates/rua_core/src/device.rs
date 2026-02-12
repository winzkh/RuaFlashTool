use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceMode {
    Fastboot,
    FastbootD,
    ADB,
    Recovery,
    Sideload,
    Unknown(String),
}

impl From<&str> for DeviceMode {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "fastboot" => DeviceMode::Fastboot,
            "fastbootd" => DeviceMode::FastbootD,
            "device" => DeviceMode::ADB,
            "recovery" => DeviceMode::Recovery,
            "sideload" => DeviceMode::Sideload,
            _ => DeviceMode::Unknown(s.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedDevice {
    pub serial: String,
    pub mode: DeviceMode,
    pub status: String,
    pub product: Option<String>,
    pub current_slot: Option<String>,
}
