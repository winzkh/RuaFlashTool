use tokio::process::Command;
use std::path::PathBuf;
use std::env;
use colored::*;
use crate::error::{FlashError, Result};
use crate::device::{ConnectedDevice, DeviceMode};

#[derive(Clone)]
pub struct FastbootClient {
    fastboot_path: PathBuf,
    pub debug: bool,
    pub selected_serial: Option<String>,
}

impl FastbootClient {
    pub fn new() -> Result<Self> {
        let mut base_path = env::current_dir()?;
        base_path.push("platform-tools");

        if !base_path.exists()
            && let Ok(mut exe_path) = env::current_exe() {
                exe_path.pop();
                exe_path.push("platform-tools");
                if exe_path.exists() {
                    base_path = exe_path;
                }
            }

        let fastboot_path = if cfg!(target_os = "windows") {
            base_path.join("fastboot.exe")
        } else {
            base_path.join("fastboot")
        };

        if !fastboot_path.exists() {
            return Err(FlashError::FastbootExecutableNotFound(fastboot_path.to_string_lossy().to_string()));
        }

        Ok(Self {
            fastboot_path,
            debug: false,
            selected_serial: None,
        })
    }

    pub fn set_debug(&mut self, debug: bool) {
        self.debug = debug;
    }

    pub fn set_serial(&mut self, serial: Option<String>) {
        self.selected_serial = serial;
    }

    pub fn get_serial(&self) -> Option<&str> {
        self.selected_serial.as_deref()
    }

    fn build_args(&self, args: &[&str]) -> Vec<String> {
        let mut cmd_args = Vec::new();
        if let Some(ref serial) = self.selected_serial {
            cmd_args.push("-s".to_string());
            cmd_args.push(serial.clone());
        }
        for arg in args {
            cmd_args.push(arg.to_string());
        }
        cmd_args
    }

    pub async fn run(&self, args: &[&str]) -> Result<bool> {
        let cmd_args = self.build_args(args);
        if self.debug {
            let cmd_name = self.fastboot_path.file_name().and_then(|f| f.to_str()).unwrap_or("fastboot");
            println!("\n{} [模拟] 执行: {} {}", ">>".yellow(), cmd_name, cmd_args.join(" "));
            return Ok(true);
        }
        let status = Command::new(&self.fastboot_path)
            .args(&cmd_args)
            .status()
            .await?;
        Ok(status.success())
    }

    pub async fn capture(&self, args: &[&str]) -> Result<String> {
        let cmd_args = self.build_args(args);
        if self.debug {
            let cmd_name = self.fastboot_path.file_name().and_then(|f| f.to_str()).unwrap_or("fastboot");
            println!("\n{} [模拟] 捕获输出: {} {}", ">>".yellow(), cmd_name, cmd_args.join(" "));
            if args.contains(&"devices") {
                return Ok("EMULATOR12345\tfastboot".to_string());
            }
            if args.contains(&"getvar") {
                return Ok("product: EMULATOR\ncurrent-slot: a".to_string());
            }
            return Ok("".to_string());
        }
        let output = Command::new(&self.fastboot_path)
            .args(&cmd_args)
            .output()
            .await?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(FlashError::FastbootError(stderr.to_string()))
        }
    }

    pub async fn list_devices(&self) -> Result<Vec<ConnectedDevice>> {
        let mut devices = Vec::new();

        if let Ok(output) = self.capture(&["devices"]).await {
            for line in output.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let serial = parts[0].to_string();
                    let status = parts[1].to_string();
                    let mode = if status.contains("fastboot") {
                        DeviceMode::Fastboot
                    } else {
                        DeviceMode::Recovery
                    };

                    let mut dev = ConnectedDevice {
                        serial: serial.clone(),
                        mode,
                        status,
                        product: None,
                        current_slot: None,
                    };

                    if let Ok(product) = self.get_var(&serial, "product").await {
                        dev.product = Some(product);
                    }
                    if let Ok(slot) = self.get_var(&serial, "current-slot").await {
                        dev.current_slot = Some(slot);
                    }

                    devices.push(dev);
                }
            }
        }

        Ok(devices)
    }

    async fn get_var(&self, serial: &str, var: &str) -> Result<String> {
        let output = Command::new(&self.fastboot_path)
            .args(["-s", serial, "getvar", var])
            .output()
            .await?;

        let out_str = String::from_utf8_lossy(&output.stdout);
        let err_str = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}{}", out_str, err_str);

        for line in combined.lines() {
            if line.contains(var) {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() >= 2 {
                    return Ok(parts[1].trim().to_string());
                }
            }
        }
        Err(FlashError::PropertyNotFound(var.to_string()))
    }

    pub async fn reboot(&self, target: Option<&str>) -> Result<bool> {
        let mut args = vec!["reboot"];
        if let Some(t) = target {
            args.push(t);
        }
        self.run(&args).await
    }

    pub async fn set_active(&self, slot: &str) -> Result<bool> {
        self.run(&["set_active", slot]).await
    }

    pub async fn erase(&self, partition: &str) -> Result<bool> {
        self.run(&["erase", partition]).await
    }

    pub async fn format(&self, partition: &str) -> Result<bool> {
        self.run(&["format", partition]).await
    }

    pub async fn flash(&self, partition: &str, image_path: &str) -> Result<bool> {
        self.run(&["flash", partition, image_path]).await
    }
}
