pub mod error;
pub mod device;
pub mod adb;
pub mod fastboot;
pub mod flasher;
pub mod sepolicy;

pub mod constants;
pub mod utils;
pub mod payload;
pub mod bootimg;
pub mod avb;

pub use error::{FlashError, Result};
pub use device::{DeviceMode, ConnectedDevice};
pub use adb::AdbClient;
pub use fastboot::FastbootClient;
pub use payload::{ProgressReporter, unpack_payload};

#[cfg(not(target_os = "windows"))]
compile_error!("RuaFlashTool currently only supports Windows platform. This tool is designed for flashing Android devices using fastboot on Windows.");
