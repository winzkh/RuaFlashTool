use tokio::process::Command;
use std::path::PathBuf;
use std::env;
use colored::*;
use crate::error::{FlashError, Result};
use crate::device::{ConnectedDevice, DeviceMode};

#[derive(Clone)]
pub struct AdbClient {
    adb_path: PathBuf,
    pub debug: bool,
    pub selected_serial: Option<String>,
}

impl AdbClient {
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

        let adb_path = if cfg!(target_os = "windows") {
            base_path.join("adb.exe")
        } else {
            base_path.join("adb")
        };

        if !adb_path.exists() {
            return Err(FlashError::AdbExecutableNotFound(adb_path.to_string_lossy().to_string()));
        }

        Ok(Self {
            adb_path,
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
            let cmd_name = self.adb_path.file_name().and_then(|f| f.to_str()).unwrap_or("adb");
            println!("\n{} [模拟] 执行: {} {}", ">>".yellow(), cmd_name, cmd_args.join(" "));
            return Ok(true);
        }
        let status = Command::new(&self.adb_path)
            .args(&cmd_args)
            .status()
            .await?;
        Ok(status.success())
    }

    pub async fn capture(&self, args: &[&str]) -> Result<String> {
        let cmd_args = self.build_args(args);
        if self.debug {
            let cmd_name = self.adb_path.file_name().and_then(|f| f.to_str()).unwrap_or("adb");
            println!("\n{} [模拟] 捕获输出: {} {}", ">>".yellow(), cmd_name, cmd_args.join(" "));
            if cmd_args.contains(&"devices".to_string()) {
                return Ok("List of devices attached\nEMULATOR12345\tdevice".to_string());
            }
            if cmd_args.contains(&"getprop".to_string()) {
                return Ok("EMULATOR_MODEL".to_string());
            }
            return Ok("".to_string());
        }
        let output = Command::new(&self.adb_path)
            .args(&cmd_args)
            .output()
            .await?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(FlashError::AdbError(stderr.to_string()))
        }
    }

    pub async fn list_devices(&self) -> Result<Vec<ConnectedDevice>> {
        let mut devices = Vec::new();

        if let Ok(output) = self.capture(&["devices"]).await {
            for line in output.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let serial = parts[0].to_string();
                    let status = parts[1].to_string();
                    let mode = DeviceMode::ADB;

                    let mut dev = ConnectedDevice {
                        serial: serial.clone(),
                        mode,
                        status,
                        product: None,
                        current_slot: None,
                    };

                    if let Ok(model) = self.get_prop(&serial, "ro.product.model").await {
                        dev.product = Some(model);
                    }

                    devices.push(dev);
                }
            }
        }

        Ok(devices)
    }

    pub async fn shell(&self, serial: &str, command: &str) -> Result<String> {
        self.capture(&["-s", serial, "shell", command]).await
    }

    async fn get_prop(&self, serial: &str, prop: &str) -> Result<String> {
        self.capture(&["-s", serial, "shell", "getprop", prop]).await
    }

    pub async fn install(&self, serial: &str, apk_path: &str) -> Result<bool> {
        self.run(&["-s", serial, "install", "-r", apk_path]).await
    }

    pub async fn reboot(&self, serial: &str, target: Option<&str>) -> Result<bool> {
        let mut args = vec!["-s", serial, "reboot"];
        if let Some(t) = target {
            args.push(t);
        }
        self.run(&args).await
    }

    pub async fn scrcpy(&self, serial: Option<&str>) -> Result<bool> {
        let mut scrcpy_path = env::current_dir()?;
        scrcpy_path.push("scrcpy");
        let exe = if cfg!(target_os = "windows") { scrcpy_path.join("scrcpy.exe") } else { scrcpy_path.join("scrcpy") };

        if !exe.exists() {
            return Err(FlashError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "未找到 scrcpy 工具")));
        }

        let serial_display = serial.unwrap_or("默认设备");
        let title = format!("RuaFlashTool - {}", serial_display);
        let mut args = Vec::new();
        if let Some(s) = serial {
            args.push("-s");
            args.push(s);
        }
        args.push("--window-title");
        args.push(&title);

        if self.debug {
            let cmd_name = exe.file_name().and_then(|f| f.to_str()).unwrap_or("scrcpy");
            println!("\n{} [模拟] 执行: {} {}", ">>".yellow(), cmd_name, args.join(" "));
            return Ok(true);
        }

        let status = Command::new(exe)
            .args(&args)
            .status()
            .await?;
        Ok(status.success())
    }

    pub async fn activate_shizuku(&self, serial: &str) -> Result<String> {
        self.shell(serial, "sh /sdcard/Android/data/moe.shizuku.privileged.api/files/start.sh").await
    }

    pub async fn is_app_installed(&self, serial: &str, pkg_name: &str) -> Result<bool> {
        let path_cmd = format!("pm path {}", pkg_name);
        let output = self.shell(serial, &path_cmd).await?;
        Ok(output.contains("package:"))
    }

    pub async fn activate_axmanager(&self, serial: &str) -> Result<String> {
        let pkg_name = "frb.axeron.manager";
        if !self.is_app_installed(serial, pkg_name).await? {
            return Err(FlashError::AdbError(
                format!("未找到 {}，请确认是否已安装", pkg_name)
            ));
        }

        let path_cmd = format!("pm path {}", pkg_name);
        let output = self.shell(serial, &path_cmd).await?;

        let apk_path = output.trim().replace("package:", "");
        let pkg_dir = std::path::Path::new(&apk_path).parent().unwrap();
        let lib_path = pkg_dir.join("lib/arm64/libaxeron.so");
        let lib_path_str = lib_path.to_str().unwrap().replace("\\", "/");

        self.shell(serial, &lib_path_str).await?;
        Ok("已尝试启动 AxManager".to_string())
    }

    pub async fn activate_demon_mode(&self, serial: &str) -> Result<String> {
        self.shell(serial, "sh /sdcard/Android/data/web1n.stopapp/files/demon.sh").await
    }

    pub async fn activate_icebox_adb(&self, serial: &str) -> Result<String> {
        self.shell(serial, "sh /sdcard/Android/data/com.catchingnow.icebox/files/start.sh").await
    }

    pub async fn activate_brevent(&self, serial: &str) -> Result<String> {
        let pkg_name = "me.piebridge.brevent";
        if !self.is_app_installed(serial, pkg_name).await? {
            return Err(FlashError::AdbError(
                format!("未找到 {}，请确认是否已安装", pkg_name)
            ));
        }
        self.shell(serial, "sh /data/data/me.piebridge.brevent/brevent.sh").await
    }

    pub async fn activate_demon_admin(&self, serial: &str) -> Result<String> {
        self.shell(serial, "dpm set-device-owner web1n.stopapp/.receiver.AdminReceiver").await
    }

    pub async fn activate_icebox_admin(&self, serial: &str) -> Result<String> {
        self.shell(serial, "dpm set-device-owner com.catchingnow.icebox/.receiver.DPMReceiver").await
    }
}
