use std::process::{Command, Output};
use std::path::PathBuf;
use std::env;
use std::io::{self};
 
use crate::ui::{step, ok, warn, err};

pub struct FastbootManager {
    executable_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub serial: String,
    pub mode: String,   // "Fastboot", "ADB", "Recovery", "Sideload"
    pub status: String, // "device", "unauthorized", "offline", etc.
}

impl FastbootManager {
    /// 初始化 Fastboot 管理器
    pub fn new() -> io::Result<Self> {
        // 首先尝试从当前工作目录寻找
        let mut path = env::current_dir()?;
        path.push("platform-tools");
        
        #[cfg(target_os = "windows")]
        path.push("fastboot.exe");
        #[cfg(not(target_os = "windows"))]
        path.push("fastboot");

        // 如果当前目录没有，则尝试从可执行文件所在目录寻找
        if !path.exists() {
            if let Ok(mut exe_path) = env::current_exe() {
                exe_path.pop();
                exe_path.push("platform-tools");
                #[cfg(target_os = "windows")]
                exe_path.push("fastboot.exe");
                #[cfg(not(target_os = "windows"))]
                exe_path.push("fastboot");
                
                if exe_path.exists() {
                    path = exe_path;
                }
            }
        }

        Ok(Self {
            executable_path: path,
        })
    }



    /// 检查 Fastboot 环境是否正常
    pub fn check_env(&self) -> bool {
        if !self.executable_path.exists() {
            err(&format!("错误: 未找到 fastboot 可执行文件，期待路径: {:?}", self.executable_path));
            return false;
        }
        true
    }

    /// 执行 fastboot 命令，输出实时显示在控制台 (stdout/stderr 继承)
    pub fn run_cmd(&self, args: &[&str]) -> io::Result<bool> {
        if !self.check_env() {
            return Ok(false);
        }

        let status = Command::new(&self.executable_path)
            .args(args)
            .status()?;
        
        Ok(status.success())
    }

    /// 执行 fastboot 命令并捕获输出内容 (不打印到控制台)
    pub fn capture_cmd(&self, args: &[&str]) -> io::Result<Output> {
        Command::new(&self.executable_path)
            .args(args)
            .output()
    }

    /// 执行 adb 命令并捕获输出内容 (不打印到控制台)
    pub fn capture_adb_cmd(&self, args: &[&str]) -> io::Result<Output> {
        let path = self.get_adb_path();
        Command::new(path)
            .args(args)
            .output()
    }

    /// 获取 ADB 设备的系统属性
    pub fn get_adb_var(&self, sn: &str, prop: &str) -> io::Result<String> {
        let output = self.capture_adb_cmd(&["-s", sn, "shell", "getprop", prop])?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Property not found"));
        }
        Ok(stdout)
    }

    // --- 便捷接口 ---

    /// 获取已连接的设备列表 (包含 Fastboot 和 ADB 模式)
    pub fn get_devices(&self) -> io::Result<Vec<Device>> {
        let mut devices = Vec::new();

        // 1. 获取 Fastboot 设备
        if let Ok(output) = self.capture_cmd(&["devices"]) {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 1 {
                    devices.push(Device {
                        serial: parts[0].to_string(),
                        mode: "Fastboot".to_string(),
                        status: "fastboot".to_string(),
                    });
                }
            }
        }

        // 2. 获取 ADB 设备
        if let Ok(output) = self.capture_adb_cmd(&["devices"]) {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // adb devices 输出第一行是 "List of devices attached"，需要跳过
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let serial = parts[0].to_string();
                    let status = parts[1].to_string();
                    
                    // 如果 Fastboot 列表里已经有了（虽然序列号可能冲突，但通常不会同时出现），则以 Fastboot 为准或并存
                    // 这里我们允许并存，或者更新信息
                    let mode = match status.as_str() {
                        "device" => "ADB",
                        "recovery" => "Recovery",
                        "sideload" => "Sideload",
                        "unauthorized" => "ADB (未授权)",
                        _ => "ADB",
                    };

                    devices.push(Device {
                        serial,
                        mode: mode.to_string(),
                        status,
                    });
                }
            }
        }

        Ok(devices)
    }

    /// 刷入分区
    pub fn flash(&self, partition: &str, file_path: &str) -> io::Result<bool> {
        step(&format!("正在刷入 {} 分区: {}", partition, file_path));
        self.run_cmd(&["flash", partition, file_path])
    }



    /// 重启设备
    pub fn reboot(&self, target: Option<&str>) -> io::Result<bool> {
        let mut args = vec!["reboot"];
        if let Some(t) = target {
            args.push(t);
        }
        step(&format!("正在重启设备{}...", target.map(|t| format!(" 到 {}", t)).unwrap_or_default()));
        self.run_cmd(&args)
    }

    /// 获取变量
    pub fn get_var(&self, var: &str) -> io::Result<String> {
        let output = self.capture_cmd(&["getvar", var])?;
        // fastboot getvar 输出通常在 stderr
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        
        let combined = format!("{}{}", stdout, stderr);
        for line in combined.lines() {
            if line.contains(var) && line.contains(":") {
                let parts: Vec<&str> = line.splitn(2, ':').collect();
                if parts.len() == 2 {
                    return Ok(parts[1].trim().to_string());
                }
            }
        }
        Ok("unknown".to_string())
    }

    /// 等待设备连接
    pub fn wait_for_device(&self) -> io::Result<bool> {
        step("正在等待设备连接 (Fastboot 模式)...");
        // 虽然 fastboot 有 wait-for-device 指令，但某些版本支持不佳
        // 我们通过循环 get_devices 来实现更可靠的等待
        use std::thread;
        use std::time::Duration;

        for _ in 0..60 { // 最多等待 60 秒
            let devices = self.get_devices()?;
            if !devices.is_empty() {
                ok(&format!("已发现设备: {} ({})", devices[0].serial, devices[0].mode));
                return Ok(true);
            }
            thread::sleep(Duration::from_secs(1));
        }
        
        err("错误: 等待超时，未发现设备");
        Ok(false)
    }

    /// 获取当前的插槽 (A/B 分区设备)
    pub fn get_current_slot(&self) -> io::Result<String> {
        self.get_var("current-slot")
    }

    /// 切换插槽
    pub fn set_active_slot(&self, slot: &str) -> io::Result<bool> {
        step(&format!("正在设置活动插槽为: {}", slot));
        self.run_cmd(&["--set-active", slot])
    }





    /// Fastboot Flashing Unlock (现代设备常用)
    pub fn flashing_unlock(&self) -> io::Result<bool> {
        warn("正在尝试执行 flashing unlock...");
        self.run_cmd(&["flashing", "unlock"])
    }

    /// Fastboot Flashing Lock
    pub fn flashing_lock(&self) -> io::Result<bool> {
        warn("正在尝试执行 flashing lock...");
        self.run_cmd(&["flashing", "lock"])
    }

    /// 清除数据 (fastboot -w)
    pub fn format_w(&self) -> io::Result<bool> {
        warn("正在执行恢复出厂设置 (fastboot -w)...");
        self.run_cmd(&["-w"])
    }

    /// 检查是否处于 FastbootD 模式
    pub fn is_fastbootd(&self) -> bool {
        self.get_var("is-userspace").unwrap_or_default() == "yes"
    }

    /// 获取 ADB 可执行文件路径
    pub fn get_adb_path(&self) -> PathBuf {
        let mut path = self.executable_path.clone();
        path.pop();
        #[cfg(target_os = "windows")]
        path.push("adb.exe");
        #[cfg(not(target_os = "windows"))]
        path.push("adb");
        path
    }

    /// 执行 adb 命令
    pub fn run_adb_cmd(&self, args: &[&str]) -> io::Result<bool> {
        let path = self.get_adb_path();
        if !path.exists() {
            err(&format!("错误: 未找到 adb 可执行文件, 路径: {:?}", path));
            return Ok(false);
        }
        let status = Command::new(path).args(args).status()?;
        Ok(status.success())
    }

    /// 重启到 EDL (9008)
    pub fn reboot_edl(&self) -> io::Result<bool> {
        warn("正在尝试重启到 EDL 模式...");
        // 尝试多种常用命令
        if let Ok(success) = self.run_cmd(&["oem", "edl"]) {
            if success { return Ok(true); }
        }
        if let Ok(success) = self.run_adb_cmd(&["reboot", "edl"]) {
            if success { return Ok(true); }
        }
        self.run_cmd(&["reboot", "edl"])
    }
}

#[allow(dead_code)]
pub mod ext {
    use super::*;
    use std::path::PathBuf;

    impl FastbootManager {
        pub fn get_path(&self) -> &PathBuf {
            &self.executable_path
        }
        pub fn erase(&self, partition: &str) -> io::Result<bool> {
            step(&format!("正在擦除 {} 分区...", partition));
            self.run_cmd(&["erase", partition])
        }
        pub fn oem_unlock(&self) -> io::Result<bool> {
            warn("正在尝试解锁 Bootloader...");
            self.run_cmd(&["oem", "unlock"])
        }
        pub fn oem_lock(&self) -> io::Result<bool> {
            warn("正在尝试锁定 Bootloader...");
            self.run_cmd(&["oem", "lock"])
        }
    }
}
