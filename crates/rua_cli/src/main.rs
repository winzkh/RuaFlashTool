mod ui;
mod utils;

use crate::utils::file_finder::FileFinder;
use clap::Parser;
use colored::*;
use figlet_rs::FIGfont;
use rua_core::constants::*;
use rua_core::fastboot::FastbootClient;
use rua_core::flasher::Flasher;
use rua_core::ConnectedDevice;
use rustyline::DefaultEditor;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use rua_core::payload::{self, ProgressReporter};
use std::sync::Arc;
use std::path::{Path, PathBuf};

struct ConsoleReporter;
impl ProgressReporter for ConsoleReporter {
    fn on_start(&self, name: &str, _total: u64) {
        println!(">> 开始解包分区: {}", name);
    }
    fn on_progress(&self, name: &str, current: u64, total: u64) {
        if current % 100 == 0 || current == total {
            print!("\r>> 解包 {}: {}/{}", name, current, total);
            let _ = io::stdout().flush();
        }
    }
    fn on_complete(&self, name: &str, _total: u64) {
        println!("\r>> 解包分区 {} 完成！            ", name);
    }
    fn on_warning(&self, name: &str, _idx: usize, msg: String) {
        println!("\n>> [警告] 分区 {}: {}", name, msg);
    }
}

#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Console::{
    GetConsoleWindow, GetStdHandle, SetConsoleScreenBufferSize, SetConsoleWindowInfo,
    STD_OUTPUT_HANDLE, CONSOLE_SCREEN_BUFFER_INFO, SMALL_RECT, COORD, GetConsoleScreenBufferInfo,
    GetCurrentConsoleFontEx, SetCurrentConsoleFontEx, CONSOLE_FONT_INFOEX,
    GetConsoleMode, SetConsoleMode, SetConsoleOutputCP, ENABLE_VIRTUAL_TERMINAL_PROCESSING
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::{HANDLE, HWND, FALSE};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::WindowsAndMessaging::MoveWindow;

pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {}

#[cfg(target_os = "windows")]
fn set_console_window_properties() {
    unsafe {
        let console_handle: HANDLE = GetStdHandle(STD_OUTPUT_HANDLE);
        if console_handle == std::ptr::null_mut() {
            return;
        }

        let mut mode: u32 = 0;
        if GetConsoleMode(console_handle, &mut mode) != 0 {
            SetConsoleMode(console_handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }

        SetConsoleOutputCP(65001);

        let mut font_info_ex: CONSOLE_FONT_INFOEX = std::mem::zeroed();
        font_info_ex.cbSize = std::mem::size_of::<CONSOLE_FONT_INFOEX>() as u32;
        if GetCurrentConsoleFontEx(console_handle, FALSE, &mut font_info_ex) != FALSE {
            font_info_ex.dwFontSize.X = 0;
            font_info_ex.dwFontSize.Y = 18;
            SetCurrentConsoleFontEx(console_handle, FALSE, &font_info_ex);
        }

        let mut csbi: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(console_handle, &mut csbi) == 0 {
            return;
        }

        let new_cols: i16 = 100;
        let new_rows: i16 = 52;

        let new_buffer_size = COORD { X: new_cols, Y: 2000 }; 
        if SetConsoleScreenBufferSize(console_handle, new_buffer_size) == FALSE {
            let fallback_buffer_size = COORD { X: new_cols, Y: new_rows };
            SetConsoleScreenBufferSize(console_handle, fallback_buffer_size);
        }

        let mut font_info_actual: CONSOLE_FONT_INFOEX = std::mem::zeroed();
        font_info_actual.cbSize = std::mem::size_of::<CONSOLE_FONT_INFOEX>() as u32;
        GetCurrentConsoleFontEx(console_handle, FALSE, &mut font_info_actual);
        
        let font_w = if font_info_actual.dwFontSize.X == 0 { 12 } else { font_info_actual.dwFontSize.X as i32 };
        let font_h = font_info_actual.dwFontSize.Y as i32;

        let mut console_window_rect = SMALL_RECT {
            Left: 0,
            Top: 0,
            Right: new_cols - 1,
            Bottom: new_rows - 1,
        };
        SetConsoleWindowInfo(console_handle, FALSE, &mut console_window_rect);

        for _ in 0..3 {
            print!("\x1b[8;{};{}t", new_rows, new_cols);
            let _ = io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let hwnd: HWND = GetConsoleWindow();
        if hwnd != std::ptr::null_mut() {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut monitor_info: MONITORINFO = std::mem::zeroed();
            monitor_info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            
            if GetMonitorInfoW(monitor, &mut monitor_info) != FALSE {
                let screen_width = monitor_info.rcMonitor.right - monitor_info.rcMonitor.left;
                let screen_height = monitor_info.rcMonitor.bottom - monitor_info.rcMonitor.top;

                let window_width = (new_cols as i32 * font_w) + 40;
                let window_height = (new_rows as i32 * font_h) + 80;

                let x = (screen_width - window_width) / 2;
                let y = (screen_height - window_height) / 2;

                MoveWindow(hwnd, x, y, window_width, window_height, 1);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    set_console_window_properties();

    let _args = Args::parse();
    
    ctrlc::set_handler(move || {
        if INTERRUPTED.load(Ordering::SeqCst) {
            std::process::exit(130);
        }
        INTERRUPTED.store(true, Ordering::SeqCst);
        println!("{}", "\n\n>> [中断] 收到退出信号，正在尝试停止...".yellow().bold());
    }).expect("Error setting Ctrl-C handler");

    let client = FastbootClient::new()?;
    
    if let Err(e) = run_interactive_loop(client).await {
        ui::err(&format!("程序发生异常错误: {:?}", e));
    }
    
    Ok(())
}

async fn run_interactive_loop(client: FastbootClient) -> anyhow::Result<()> {
    let mut rl = DefaultEditor::new()?;
    loop {
        refresh_ui();
        let readline = rl.readline("> ");
        match readline {
            Ok(line) => {
                INTERRUPTED.store(false, Ordering::SeqCst);
                let input = line.trim();
                if input.is_empty() { continue; }
                let _ = rl.add_history_entry(input);
                match input.to_lowercase().as_str() {
                    "0" => {
                        println!("{}", "\n喵呜~ 下次再见！".green());
                        break;
                    }
                    choice => {
                        handle_menu_action(choice, &client).await;
                        pause_before_back();
                    }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("{}", "\n已通过 Ctrl+C 退出".yellow());
                break;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("{}", "\n已通过结束符退出".yellow());
                break;
            },
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn refresh_ui() {
    print!("\x1b[?1049h\x1b[3J\x1b[2J\x1b[H");
    let _ = io::stdout().flush();

    println!();
    let standard_font = FIGfont::standard().unwrap();
    if let Some(art) = standard_font.convert(APP_NAME) {
        println!("{}", art.to_string().cyan());
    } else {
        println!("{}", APP_NAME.cyan().bold());
    }
    println!("版本: {}  作者: {}  ", VERSION.yellow(), AUTHOR.yellow());
    if !QQ_GROUPS.is_empty() {
        println!("QQ交流群: {}", QQ_GROUPS.join(", ").blue());
    }

    let divider = "=".repeat(100).white();
    println!("{}", divider);
    for warning in WARNING_TEXTS {
        println!("{}", warning.red().bold());
    }
    println!("{}", divider);
    for info in INFO_TEXTS {
        println!("{}", info.green());
    }
    println!("{}", divider);

    for (id, desc) in MENU_OPTIONS.iter() {
        println!("{}{}", format!("{:>2}. ", id).bright_cyan(), desc);
    }
    println!("{}", divider);
}

async fn handle_menu_action(choice: &str, client: &FastbootClient) {
    let flasher = Flasher::new(client.clone());
    println!();
    match choice {
        "1" => flash_xiaomi_fastboot().await,
        "2" => unpack_payload().await,
        "3" => flash_all_partitions(&flasher, true).await,
        "4" => flash_all_partitions(&flasher, false).await,
        "5" => manage_bootloader(client).await,
        "6" => download_miui_unlock_tool(),
        "7" => flash_magisk(&flasher).await,
        "8" => flash_apatch(&flasher).await,
        "9" => flash_kernelsu_lkm(&flasher).await,
        "10" => flash_anykernel3(&flasher).await,
        "11" => flash_custom_partition(&flasher).await,
        "12" => install_usb_driver(),
        "13" => disable_avb(&flasher).await,
        "14" => open_cmd(),
        "15" => detect_device(client).await,
        "16" => start_scrcpy().await,
        "17" => install_apk().await,
        "18" => factory_reset(client).await,
        "19" => reboot_device(client).await,
        "20" => switch_slot(client).await,
        "21" => activate_shizuku().await,
        "22" => open_device_manager(),
        "0" => ui::ok("感谢使用 RuaFlashTool，再见！"),
        _ => ui::warn(&format!("未知选项: {}", choice)),
    }
}

async fn flash_xiaomi_fastboot() {
    ui::step("小米线刷包一键刷入...");
    if let Some(dir) = ui::select_directory("请选择小米线刷包解压后的目录") {
        let bat_files = [
            ("flash_all.bat", "刷机并清除所有数据"),
            ("flash_all_lock.bat", "刷机、清除数据并回锁 Bootloader"),
            ("flash_all_except_storage.bat", "刷机并保留个人数据"),
        ];

        let available_bats: Vec<(String, String)> = bat_files
            .iter()
            .filter(|(name, _)| dir.join(name).exists())
            .map(|(name, desc)| (name.to_string(), desc.to_string()))
            .collect();

        if available_bats.is_empty() {
            ui::err("未在目录下找到任何刷机脚本文件 (flash_all.bat / flash_all_lock.bat / flash_all_except_storage.bat)");
        } else {
            println!("\n检测到以下可用的刷机脚本:");
            let divider = "=".repeat(60).white();
            println!("{}", divider);
            for (i, (_, desc)) in available_bats.iter().enumerate() {
                println!("{}{}", format!("{:>2}. ", i + 1).bright_cyan(), desc);
            }
            println!("{}", divider);

            print!("请选择刷机方式 (输入序号): ");
            let _ = io::stdout().flush();
            let mut input = String::new();
            let _ = io::stdin().read_line(&mut input);

            let choice: usize = input.trim().parse().unwrap_or(0);
            if choice > 0 && choice <= available_bats.len() {
                let selected_bat = &available_bats[choice - 1].0;
                let bat_path = dir.join(selected_bat);

                let mut should_proceed = true;

                if selected_bat == "flash_all_except_storage.bat" {
                    ui::warn("警告: 此选项将保留设备上的所有个人数据！");
                    ui::warn("如果系统版本与当前设备不匹配，可能导致开机异常。");
                    if !ui::confirm("确定要保留数据刷入吗？", true) {
                        ui::warn("已取消刷机操作。");
                        should_proceed = false;
                    }
                } else if selected_bat == "flash_all_lock.bat" {
                    ui::warn("警告: 此选项将在刷机完成后回锁 Bootloader！");
                    ui::warn("回锁后可能需要重新解锁才能刷入第三方固件。");
                    if !ui::confirm("确定要回锁 Bootloader 吗？", false) {
                        ui::warn("已取消刷机操作。");
                        should_proceed = false;
                    }
                } else {
                    ui::warn("警告: 此操作将清除设备上的所有个人数据！");
                    if !ui::confirm("确定要继续刷机吗？", false) {
                        ui::warn("已取消刷机操作。");
                        should_proceed = false;
                    }
                }

                if should_proceed {
                    ui::step(&format!("正在启动 {} ...", selected_bat));
                    let _ = tokio::process::Command::new("cmd")
                        .arg("/c")
                        .arg("start")
                        .arg("/wait")
                        .arg(&bat_path)
                        .spawn();
                    ui::ok("刷机脚本已启动，请在手机屏幕上确认操作。");
                }
            } else {
                ui::err("无效的选择。");
            }
        }
    }
}

async fn unpack_payload() {
    if let Some(path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) {
        let output_dir = Path::new("extracted_payload").to_path_buf();
        let _ = fs::create_dir_all(&output_dir);
        ui::step(&format!("正在处理 Payload 到 {} ...", output_dir.display()));
        
        let path_clone = path.clone();
        tokio::spawn(async move {
            let reporter = Arc::new(ConsoleReporter);
            if let Err(e) = payload::unpack_payload(&path_clone, &output_dir, reporter).await {
                eprintln!("\n处理失败: {:?}", e);
            } else {
                println!("\n处理完成！文件保存在: {}", output_dir.display());
            }
        });
        println!("{}", "任务已在后台启动，您可以继续其他操作。".green());
    }
}

async fn flash_all_partitions(flasher: &Flasher, fastboot_mode: bool) {
    let mode_str = if fastboot_mode { "Fastboot" } else { "FastbootD" };
    ui::step(&format!("正在目录下查找分区镜像刷入 ({})...", mode_str));
    if let Some(dir) = ui::select_directory("请选择包含分区镜像 (.img) 的目录") {
        let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        
        for entry in entries {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext == "img") {
                let partition = path.file_stem().unwrap().to_str().unwrap();
                ui::step(&format!("正在刷入 {}: {} ...", partition, path.display()));
                if let Err(e) = flasher.flash_partition("", partition, &path.to_string_lossy()).await {
                    ui::err(&format!("✗ {} 刷入失败: {:?}", partition, e));
                } else {
                    ui::ok(&format!("✓ {} 刷入成功", partition));
                }
            }
        }
        ui::ok("全部刷入尝试完成。");
    }
}

async fn manage_bootloader(client: &FastbootClient) {
    println!("请选择操作:");
    println!("1. 解锁 Bootloader");
    println!("2. 回锁 Bootloader");
    print!("请输入选择 (1-2): ");
    let _ = io::stdout().flush();
    let mut choice = String::new();
    let _ = io::stdin().read_line(&mut choice);

    match choice.trim() {
        "1" => {
            if ui::confirm("确定要解锁 Bootloader 吗？这将清除所有数据！", false) {
                ui::step("正在尝试解锁 Bootloader...");
                if let Err(e) = client.run(&["flashing", "unlock"]).await {
                    ui::err(&format!("指令执行失败: {:?}", e));
                }
                if let Err(e) = client.run(&["oem", "unlock"]).await {
                    ui::err(&format!("指令执行失败: {:?}", e));
                }
                ui::ok("已发送解锁指令，请查看手机屏幕确认。");
            }
        }
        "2" => {
            if ui::confirm("确定要回锁 Bootloader 吗？请确保系统为原厂且未修改！", false) {
                ui::step("正在尝试回锁 Bootloader...");
                if let Err(e) = client.run(&["flashing", "lock"]).await {
                    ui::err(&format!("指令执行失败: {:?}", e));
                }
                if let Err(e) = client.run(&["oem", "lock"]).await {
                    ui::err(&format!("指令执行失败: {:?}", e));
                }
                ui::ok("已发送回锁指令，请查看手机屏幕确认。");
            }
        }
        _ => ui::err("无效的选择。"),
    }
}

fn download_miui_unlock_tool() {
    ui::step("正在打开小米解锁工具官网...");
    let _ = tokio::process::Command::new("cmd")
        .args(&["/c", "start", "https://www.miui.com/unlock/index.html"])
        .spawn();
}

async fn flash_magisk(flasher: &Flasher) {
    let exe_path = env::current_exe().unwrap_or(std::path::PathBuf::from("rua_flash_tool.exe"));
    let exe_dir = exe_path.parent().unwrap_or(std::path::Path::new("."));
    let mut magisk_root = exe_dir.join("Magisk");

    if !magisk_root.exists() || !magisk_root.is_dir() {
        let exe_str = exe_path.to_string_lossy();
        let is_dev_mode = exe_str.contains("target\\debug") || exe_str.contains("target\\release");

        if is_dev_mode {
            ui::warn("检测到开发环境运行 (cargo run)，正在查找项目目录下的 Magisk 文件夹...");
            let project_magisk = exe_dir.join("..").join("..").join("Magisk").canonicalize().unwrap_or_default();
            if project_magisk.exists() && project_magisk.is_dir() {
                ui::ok(&format!("已找到 Magisk 文件夹: {}", project_magisk.display()));
                magisk_root = project_magisk;
            } else {
                ui::err("未在项目目录下找到 Magisk 文件夹");
                println!("{}", "请手动选择 Magisk 文件夹".cyan());
                magisk_root = match ui::select_directory("请选择 Magisk 文件夹") {
                    Some(path) => path,
                    None => return,
                };
            }
        } else {
            ui::err(&format!("未在程序目录下找到 Magisk 文件夹: {}", magisk_root.display()));
            println!("{}", "请手动选择 Magisk 文件夹 (包含 Alpha/Kitsune/Magisk 等子文件夹)".cyan());
            magisk_root = match ui::select_directory("请选择 Magisk 文件夹") {
                Some(path) => path,
                None => return,
            };
        }
    } else {
        ui::step(&format!("已找到 Magisk 文件夹: {}", magisk_root.display()));
    }

    ui::step("正在扫描 Magisk 分支和版本...");
    let versions = scan_magisk_folders(&magisk_root);

    let branches: Vec<String> = versions.iter()
        .map(|v| v.branch.clone())
        .collect::<std::collections::HashSet<String>>()
        .into_iter()
        .collect();

    if branches.is_empty() {
        ui::err("未在文件夹中找到任何 Magisk 版本。");
        return;
    }

    println!("\n{} {}", ">>".cyan().bold(), "请选择 Magisk 分支:".bright_white());
    let divider = "=".repeat(60).white();
    println!("{}", divider);
    for (i, branch) in branches.iter().enumerate() {
        println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), branch.yellow());
    }
    println!("{}{}", format!("{:>3}. ", branches.len() + 1).bright_cyan(), "自定义 APK 文件".magenta());
    println!("{}", divider);

    print!("请选择: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);

    let choice: usize = input.trim().parse().unwrap_or(0);

    if choice > 0 && choice <= branches.len() {
        let branch_name = branches[choice - 1].clone();

        let branch_versions: Vec<&MagiskVersion> = versions.iter()
            .filter(|v| v.branch == branch_name)
            .collect();

        println!("\n{} {}:", ">>".cyan().bold(), format!("{} 分支版本列表:", branch_name).bright_white());
        let divider2 = "=".repeat(60).white();
        println!("{}", divider2);
        for (i, ver) in branch_versions.iter().enumerate() {
            println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), ver.version_name.bright_white());
        }
        println!("{}", divider2);

        print!("请选择版本: ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        let _ = io::stdin().read_line(&mut input);

        let ver_choice: usize = input.trim().parse().unwrap_or(0);
        if ver_choice > 0 && ver_choice <= branch_versions.len() {
            let selected_ver = branch_versions[ver_choice - 1];
            let ver_folder = &selected_ver.path;

            let files = get_magisk_files_from_folder(ver_folder);
            if files.is_empty() {
                ui::err("该版本文件夹中未找到任何 Magisk 文件。");
                return;
            }

            let partition = select_partition();
            if partition.is_empty() {
                return;
            }

            let Some(boot_path) = ui::select_file("请选择要修补的 Boot 镜像", &["img"]) else {
                return;
            };

            let boot_path_str = boot_path.to_string_lossy().to_string();
            let boot_file_name = boot_path.file_name().unwrap_or_default().to_string_lossy();

            ui::step("正在修补镜像...");
            match flasher.magisk_patch_with_files(&boot_path_str, &files, "").await {
                Ok(patched_path) => {
                    ui::ok("镜像修补成功！");

                    println!("\n{}", "=".repeat(60).white());
                    println!("{}", "📱 Magisk 刷入确认".bright_white().bold());
                    println!("{}", "=".repeat(60).white());
                    println!("{}", format!("  📦 Magisk 版本: {} - {}", branch_name, selected_ver.version_name).cyan());
                    println!("{}", format!("  📁 源镜像: {}", boot_file_name).cyan());
                    println!("{}", format!("  💾 目标分区: {}", partition).cyan());
                    println!("{}", format!("  📝 修补后镜像: {}", patched_path).cyan());
                    println!("{}", "=".repeat(60).white());

                    if !ui::confirm("确定要继续刷入吗？", true) {
                        ui::warn("已取消刷入操作，修补镜像已保存。");
                        return;
                    }

                    let target_device = select_device(&flasher.client).await;
                    if target_device.is_empty() {
                        ui::warn("未检测到设备，无法刷入。修补镜像已保存。");
                        return;
                    }

                    ui::step(&format!("正在刷入 {} 分区...", partition));
                    match flasher.flash_partition(&target_device, &partition, &patched_path).await {
                        Ok(_) => ui::ok("刷入成功！"),
                        Err(e) => ui::err(&format!("刷入失败: {:?}", e)),
                    }
                },
                Err(e) => ui::err(&format!("镜像修补失败: {:?}", e)),
            }
        } else {
            ui::err("无效的选择。");
        }
    } else if choice == branches.len() + 1 {
        if let Some(apk) = ui::select_file("请选择 Magisk APK 文件", &["apk"]) {
            let partition = select_partition();
            if partition.is_empty() {
                return;
            }

            let Some(boot_path) = ui::select_file("请选择要修补的 Boot 镜像", &["img"]) else {
                return;
            };

            let boot_path_str = boot_path.to_string_lossy().to_string();
            let boot_file_name = boot_path.file_name().unwrap_or_default().to_string_lossy();

            ui::step("正在修补镜像...");
            match flasher.magisk_patch(&boot_path_str, &apk.to_string_lossy(), "").await {
                Ok(patched_path) => {
                    ui::ok("镜像修补成功！");

                    println!("\n{}", "=".repeat(60).white());
                    println!("{}", "📱 Magisk 刷入确认 (自定义 APK)".bright_white().bold());
                    println!("{}", "=".repeat(60).white());
                    println!("{}", format!("  📁 源镜像: {}", boot_file_name).cyan());
                    println!("{}", format!("  💾 目标分区: {}", partition).cyan());
                    println!("{}", format!("  📝 修补后镜像: {}", patched_path).cyan());
                    println!("{}", "=".repeat(60).white());

                    if !ui::confirm("确定要继续刷入吗？", true) {
                        ui::warn("已取消刷入操作，修补镜像已保存。");
                        return;
                    }

                    let target_device = select_device(&flasher.client).await;
                    if target_device.is_empty() {
                        ui::warn("未检测到设备，无法刷入。修补镜像已保存。");
                        return;
                    }

                    ui::step(&format!("正在刷入 {} 分区...", partition));
                    match flasher.flash_partition(&target_device, &partition, &patched_path).await {
                        Ok(_) => ui::ok("刷入成功！"),
                        Err(e) => ui::err(&format!("刷入失败: {:?}", e)),
                    }
                },
                Err(e) => ui::err(&format!("镜像修补失败: {:?}", e)),
            }
        }
    } else {
        ui::err("无效的选择。");
    }
}

async fn flash_apatch(flasher: &Flasher) {
    println!("请选择修补模式:");
    println!("1. boot 分区 (标准 Android)");
    println!("2. kernel 分区 (部分华为等设备)");
    print!("请选择 [1/2]: ");
    let _ = io::stdout().flush();
    let mut mode = String::new();
    let _ = io::stdin().read_line(&mut mode);
    let is_raw_kernel = mode.trim() == "2";
    let target_partition = if is_raw_kernel { "kernel" } else { "boot" };

    print!("请输入 SuperKey (若未输入将自动生成): ");
    let _ = io::stdout().flush();
    let mut skey = String::new();
    let _ = io::stdin().read_line(&mut skey);
    let skey = skey.trim().to_string();
    
    let skey = if skey.is_empty() {
        let uuid = uuid::Uuid::new_v4().to_string();
        println!("SuperKey 为空，已自动生成: {}", uuid);
        uuid
    } else {
        skey
    };
    
    let prompt = if is_raw_kernel { "请选择原始 Kernel 镜像" } else { "请选择要修补的 Boot 镜像" };
    if let Some(boot_path) = ui::select_file(prompt, &["img"]) {
        ui::step("正在使用 APatch 修补...");
        
        // 先修补，不自动刷入，以便后面询问
        match flasher.apatch_patch(&boot_path.to_string_lossy(), &skey, target_partition, is_raw_kernel, false).await {
             Ok(_) => {
                 ui::ok("APatch 修补成功！");
                 println!("您的 SuperKey 为: {}", skey);
                  
                  print!("是否立即刷入到 {} 分区? [Y/n]: ", target_partition);
                  let _ = io::stdout().flush();
                  let mut confirm = String::new();
                  let _ = io::stdin().read_line(&mut confirm);
                  let confirm = confirm.trim().to_lowercase();
                  if confirm.is_empty() || confirm == "y" {
                      ui::step(&format!("正在刷入到 {} 分区...", target_partition));
                      let out_name = format!("apatch_patched_{}.img", target_partition);
                      match flasher.client.run(&["flash", target_partition, &out_name]).await {
                          Ok(true) => {
                              ui::ok("刷入成功！");
                              println!("刷写完毕！请牢记您的 SuperKey: {}", skey);
                              let _ = std::fs::remove_file(&out_name);
                          }
                          _ => ui::err("刷入失败，请检查 fastboot 连接"),
                      }
                  } else {
                      println!("已取消刷入。");
                  }
             }
            Err(e) => ui::err(&format!("APatch 修补失败: {:?}", e)),
        }
    }
}

async fn flash_kernelsu_lkm(flasher: &Flasher) {
    let exe_path = env::current_exe().unwrap_or(PathBuf::from("rua_flash_tool.exe"));
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
    
    // 兼容开发环境
    let base_dir = if exe_path.to_string_lossy().contains("target\\") {
        exe_dir.join("..").join("..").canonicalize().unwrap_or(exe_dir.to_path_buf())
    } else {
        exe_dir.to_path_buf()
    };

    ui::step("正在扫描 KernelSU LKM 分支和版本...");
    let branches = FileFinder::find_ksu_lkm_branches(&base_dir);

    if branches.is_empty() {
        ui::err("未在 KSUINIT 或 LKM 文件夹中找到任何版本。");
        ui::warn(&format!("请确保根目录下存在 KSUINIT 和 LKM 文件夹，且结构正确。"));
        return;
    }

    // 1. 选择分支
    println!("\n{} {}", ">>".cyan().bold(), "请选择 KernelSU 分支:".bright_white());
    let divider = "=".repeat(60).white();
    println!("{}", divider);
    for (i, branch) in branches.iter().enumerate() {
        println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), branch.name.yellow());
    }
    println!("{}", divider);

    print!("请选择: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
    let branch_idx: usize = input.trim().parse().unwrap_or(0);

    if branch_idx == 0 || branch_idx > branches.len() {
        ui::err("无效的选择。");
        return;
    }
    let selected_branch = &branches[branch_idx - 1];

    // 2. 选择版本
    println!("\n{} {}:", ">>".cyan().bold(), format!("{} 分支版本列表:", selected_branch.name).bright_white());
    println!("{}", divider);
    for (i, ver) in selected_branch.versions.iter().enumerate() {
        println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), ver.version_name.bright_white());
    }
    println!("{}", divider);

    print!("请选择版本: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
    let ver_idx: usize = input.trim().parse().unwrap_or(0);

    if ver_idx == 0 || ver_idx > selected_branch.versions.len() {
        ui::err("无效的选择。");
        return;
    }
    let selected_ver = &selected_branch.versions[ver_idx - 1];

    // 3. 选择 Boot 镜像
    let Some(boot_path) = ui::select_file("请选择要修补的 Boot 镜像", &["img"]) else {
        return;
    };

    // 4. 自动识别 KMI
    ui::step("正在分析 Boot 镜像 KMI...");
    let detected_kmi: Option<String> = match Flasher::detect_kmi_from_boot_img(&boot_path.to_string_lossy()) {
        Ok(Some(kmi)) => {
            ui::ok(&format!("检测到 KMI: {}", kmi));
            Some(kmi)
        }
        _ => {
            ui::warn("无法从镜像中自动识别 KMI。");
            None
        }
    };

    // 5. 选择 KMI (.ko 文件)
    println!("\n{} {}", ">>".cyan().bold(), "请选择匹配的 KMI (.ko):".bright_white());
    println!("{}", divider);
    
    let mut recommended_idx = None;
    for (i, ko) in selected_ver.ko_files.iter().enumerate() {
        let mut label = ko.kmi.clone();
        if let Some(ref dkmi) = detected_kmi {
            // 如果检测到的 KMI 包含在文件名中，标记为推荐
            if dkmi.contains(&ko.kmi) || ko.kmi.contains(dkmi) {
                label = format!("{} (推荐)", label).green().to_string();
                recommended_idx = Some(i + 1);
            }
        }
        println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), label);
    }
    println!("{}", divider);

    let default_idx = recommended_idx.unwrap_or(1);
    print!("请选择 [默认: {}]: ", default_idx);
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
    let ko_choice = input.trim();
    
    let ko_idx = if ko_choice.is_empty() {
        default_idx
    } else {
        ko_choice.parse().unwrap_or(0)
    };

    if ko_idx == 0 || ko_idx > selected_ver.ko_files.len() {
        ui::err("无效的选择。");
        return;
    }
    let selected_ko = &selected_ver.ko_files[ko_idx - 1];

    // 6. 执行修补
    let partition = select_partition();
    if partition.is_empty() { return; }

    ui::step("正在使用 KernelSU LKM 修补...");
    match flasher.kernelsu_lkm_install(
        &boot_path.to_string_lossy(),
        &selected_ver.ksuinit_path.to_string_lossy(),
        Some(&selected_ver.ksuinit_d_path.to_string_lossy()),
        &selected_ko.ko_path.to_string_lossy(),
        &partition,
        false
    ).await {
        Ok(_) => {
            ui::ok("KernelSU LKM 修补成功！");
            
            let out_name = format!("ksu_lkm_patched_{}.img", partition);
            println!("\n{}", "=".repeat(60).white());
            println!("{}", "📱 KernelSU LKM 刷入确认".bright_white().bold());
            println!("{}", "=".repeat(60).white());
            println!("{}", format!("  📦 分支: {}", selected_branch.name).cyan());
            println!("{}", format!("  🔢 版本: {}", selected_ver.version_name).cyan());
            println!("{}", format!("  🔧 KMI: {}", selected_ko.kmi).cyan());
            println!("{}", format!("  💾 目标分区: {}", partition).cyan());
            println!("{}", format!("  📝 修补后镜像: {}", out_name).cyan());
            println!("{}", "=".repeat(60).white());

            if ui::confirm("确定要继续刷入吗？", true) {
                let target_device = select_device(&flasher.client).await;
                if target_device.is_empty() {
                    ui::warn("未检测到设备，无法刷入。修补镜像已保存。");
                    return;
                }

                ui::step(&format!("正在刷入 {} 分区...", partition));
                match flasher.flash_partition(&target_device, &partition, &out_name).await {
                    Ok(_) => ui::ok("刷入成功！"),
                    Err(e) => ui::err(&format!("刷入失败: {:?}", e)),
                }
            }
        }
        Err(e) => ui::err(&format!("KernelSU LKM 修补失败: {:?}", e)),
    }
}

async fn flash_anykernel3(flasher: &Flasher) {
    println!("请选择修补模式:");
    println!("1. boot 分区 (标准 Android)");
    println!("2. kernel 分区 (部分华为等设备)");
    print!("请选择 [1/2]: ");
    let _ = io::stdout().flush();
    let mut mode = String::new();
    let _ = io::stdin().read_line(&mut mode);
    let is_raw_kernel = mode.trim() == "2";
    let target_partition = if is_raw_kernel { "kernel" } else { "boot" };

    if let Some(zip_path) = ui::select_file("请选择 AnyKernel3 ZIP 包", &["zip"]) {
        let prompt = if is_raw_kernel { "请选择原始 Kernel 镜像" } else { "请选择原始 Boot 镜像" };
        if let Some(boot_path) = ui::select_file(prompt, &["img"]) {
            ui::step("正在解压 AnyKernel3 并修补内核...");
            match flasher.anykernel3_root(&zip_path.to_string_lossy(), &boot_path.to_string_lossy(), target_partition, is_raw_kernel, false).await {
                Ok(out_name) => {
                    ui::ok("内核修补成功！");
                    
                    print!("是否立即刷入到 {} 分区? [Y/n]: ", target_partition);
                    let _ = io::stdout().flush();
                    let mut confirm = String::new();
                    let _ = io::stdin().read_line(&mut confirm);
                    let confirm = confirm.trim().to_lowercase();
                    if confirm.is_empty() || confirm == "y" {
                        ui::step(&format!("正在刷入到 {} 分区...", target_partition));
                        match flasher.client.run(&["flash", target_partition, &out_name]).await {
                            Ok(true) => {
                                ui::ok("刷入成功！");
                                let _ = std::fs::remove_file(&out_name);
                            }
                            _ => ui::err("刷入失败，请检查 fastboot 连接"),
                        }
                    } else {
                        println!("已取消刷入，修补镜像已保存为: {}", out_name);
                    }
                }
                Err(e) => ui::err(&format!("AnyKernel3 修补失败: {:?}", e)),
            }
        }
    }
    pause_before_back();
}

async fn flash_custom_partition(flasher: &Flasher) {
    if ui::confirm("确定要继续吗？此操作将刷入自定义分区镜像。", true) {
        if let Some(path) = ui::select_file("请选择要刷入的自定义分区镜像", &["img"]) {
            print!("请输入分区名 (如 recovery/system/vendor): ");
            let _ = io::stdout().flush();
            let mut partition = String::new();
            let _ = io::stdin().read_line(&mut partition);
            let partition = partition.trim();
            
            if !partition.is_empty() {
                ui::step(&format!("正在刷入 {}: {} ...", partition, path.display()));
                match flasher.flash_partition("", partition, &path.to_string_lossy()).await {
                    Ok(_) => ui::ok("刷入成功！"),
                    Err(e) => ui::err(&format!("刷入失败: {:?}", e)),
                }
            }
        }
    }
}

fn install_usb_driver() {
    ui::step("正在安装驱动...");
    let driver_exe = Path::new("drivers/QcomMtk_Driver_Setup_3.2.1.exe");
    if driver_exe.exists() {
        let _ = tokio::process::Command::new(driver_exe).spawn();
    } else {
        ui::err("未找到驱动安装包 (drivers/usb_driver_setup.exe)");
    }
}

async fn disable_avb(flasher: &Flasher) {
    if let Some(vbmeta_path) = ui::select_file("请选择 vbmeta.img", &["img"]) {
        ui::step("正在刷入 vbmeta.img 并关闭 AVB 校验...");
        if let Err(e) = flasher.flash_partition("", "vbmeta", &vbmeta_path.to_string_lossy()).await {
            ui::err(&format!("vbmeta 刷入失败: {:?}", e));
        } else {
            ui::ok("vbmeta 刷入成功，AVB 校验已禁用。");
        }
    }
}

fn open_cmd() {
    ui::step("正在打开新命令行窗口...");
    // 在 Windows 下使用 start 命令启动新的 cmd 窗口
    let _ = std::process::Command::new("cmd")
        .args(&["/c", "start", "cmd.exe"])
        .spawn();
}

async fn detect_device(client: &FastbootClient) {
    ui::step("正在检测设备连接状态 (轮询 10s)...");
    
    let mut found = false;
    let start = std::time::Instant::now();
    let client_clone = client.clone();
    
    // 进度条显示
    let pb = indicatif::ProgressBar::new(20);
    pb.set_style(indicatif::ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos:>2}/{len:2} {msg}")
        .unwrap()
        .progress_chars("#>-"));
    pb.set_message("正在扫描 ADB 和 Fastboot 设备...");

    while start.elapsed().as_secs() < 10 {
        let mut devices = Vec::new();
        
        // 同时检测 Fastboot 和 ADB
        if let Ok(mut fb_devs) = client_clone.list_devices().await {
            devices.append(&mut fb_devs);
        }
        
        if let Ok(adb) = rua_core::AdbClient::new() {
            if let Ok(mut adb_devs) = adb.list_devices().await {
                devices.append(&mut adb_devs);
            }
        }

        if !devices.is_empty() {
            pb.finish_and_clear();
            println!("\n{} 检测到 {} 个设备已连接：", "✔".green().bold(), devices.len());
            let divider = "─".repeat(60).white();
            println!("{}", divider);
            for dev in devices {
                let mode_str = match dev.mode {
                    rua_core::device::DeviceMode::Fastboot => "Fastboot".yellow(),
                    rua_core::device::DeviceMode::FastbootD => "FastbootD".yellow(),
                    rua_core::device::DeviceMode::ADB => "ADB (系统)".green(),
                    rua_core::device::DeviceMode::Recovery => "Recovery".magenta(),
                    _ => format!("{:?}", dev.mode).white(),
                };
                let product = dev.product.unwrap_or_else(|| "未知型号".to_string());
                println!("  {}  序列号: {}  型号: {}", mode_str, dev.serial.cyan(), product.bright_white());
            }
            println!("{}", divider);
            found = true;
            break;
        }
        
        pb.inc(1);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    
    if !found {
        pb.finish_and_clear();
        ui::err("10s 内未检测到任何设备连接。请检查数据线和驱动。");
    }
    
    pause_before_back();
}

async fn start_scrcpy() {
    ui::step("正在查找可用设备...");
    let mut adb_devs = Vec::new();
    if let Ok(adb) = rua_core::AdbClient::new() {
        if let Ok(devs) = adb.list_devices().await {
            adb_devs = devs;
        }
    }

    if adb_devs.is_empty() {
        ui::err("未发现 ADB 模式的设备，请确保已开启 USB 调试。");
    } else {
        let dev = if adb_devs.len() == 1 {
            &adb_devs[0]
        } else {
            println!("\n{} 检测到多个 ADB 设备，请选择:", ">>".cyan());
            for (i, d) in adb_devs.iter().enumerate() {
                println!("  {}. {} ({})", i + 1, d.serial, d.product.as_deref().unwrap_or("未知"));
            }
            print!("请选择: ");
            let _ = io::stdout().flush();
            let mut input = String::new();
            let _ = io::stdin().read_line(&mut input);
            let idx: usize = input.trim().parse().unwrap_or(0);
            if idx == 0 || idx > adb_devs.len() {
                ui::err("选择无效。");
                return;
            }
            &adb_devs[idx - 1]
        };

        ui::step(&format!("正在启动投屏: {} ...", dev.serial));
        if let Ok(adb) = rua_core::AdbClient::new() {
            let _ = adb.scrcpy(Some(&dev.serial)).await;
        }
    }
    pause_before_back();
}

async fn install_apk() {
    ui::step("正在查找可用设备...");
    let mut adb_devs = Vec::new();
    if let Ok(adb) = rua_core::AdbClient::new() {
        if let Ok(devs) = adb.list_devices().await {
            adb_devs = devs;
        }
    }

    if adb_devs.is_empty() {
        ui::err("未发现 ADB 模式的设备。");
    } else if let Some(apk_path) = ui::select_file("请选择要安装的 APK 文件", &["apk"]) {
        let dev = if adb_devs.len() == 1 {
            &adb_devs[0]
        } else {
            println!("\n{} 检测到多个 ADB 设备，请选择:", ">>".cyan());
            for (i, d) in adb_devs.iter().enumerate() {
                println!("  {}. {} ({})", i + 1, d.serial, d.product.as_deref().unwrap_or("未知"));
            }
            print!("请选择: ");
            let _ = io::stdout().flush();
            let mut input = String::new();
            let _ = io::stdin().read_line(&mut input);
            let idx: usize = input.trim().parse().unwrap_or(0);
            if idx == 0 || idx > adb_devs.len() {
                ui::err("选择无效。");
                return;
            }
            &adb_devs[idx - 1]
        };

        ui::step(&format!("正在安装 APK 到 {}: {} ...", dev.serial, apk_path.display()));
        if let Ok(adb) = rua_core::AdbClient::new() {
            match adb.install(&dev.serial, &apk_path.to_string_lossy()).await {
                Ok(_) => ui::ok("安装成功！"),
                Err(e) => ui::err(&format!("安装失败: {:?}", e)),
            }
        }
    }
    pause_before_back();
}

async fn factory_reset(client: &FastbootClient) {
    if ui::confirm("确定要恢复出厂设置吗？这将清除所有数据！", false) {
        ui::step("正在检测 Fastboot 设备...");
        let target_device = select_device(client).await;
        if target_device.is_empty() {
            ui::err("未检测到 Fastboot 设备，无法执行清除操作。");
            pause_before_back();
            return;
        }

        ui::step("正在清除 Data 分区...");
        if let Err(e) = client.erase("userdata").await {
            ui::err(&format!("清除失败: {:?}", e));
        }
        ui::step("正在格式化 Data 分区...");
        if let Err(e) = client.format("userdata").await {
            ui::err(&format!("格式化失败: {:?}", e));
        }
        ui::ok("恢复出厂设置操作完成。");
    }
    pause_before_back();
}

async fn reboot_device(client: &FastbootClient) {
    // 1. 获取所有可用设备 (ADB + Fastboot)
    let mut all_devs = Vec::new();
    if let Ok(mut fb_devs) = client.list_devices().await {
        all_devs.append(&mut fb_devs);
    }
    if let Ok(adb) = rua_core::AdbClient::new() {
        if let Ok(mut adb_devs) = adb.list_devices().await {
            all_devs.append(&mut adb_devs);
        }
    }

    if all_devs.is_empty() {
        ui::err("未检测到任何 ADB 或 Fastboot 设备。");
        pause_before_back();
        return;
    }

    // 2. 选择设备
    let selected_dev = if all_devs.len() == 1 {
        &all_devs[0]
    } else {
        println!("\n{} 请选择要重启的设备:", ">>".cyan());
        for (i, d) in all_devs.iter().enumerate() {
            let mode_str = match d.mode {
                rua_core::device::DeviceMode::ADB => "ADB".green(),
                _ => "Fastboot".yellow(),
            };
            println!("  {}. [{}] {} ({})", i + 1, mode_str, d.serial, d.product.as_deref().unwrap_or("未知"));
        }
        print!("请选择: ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        let _ = io::stdin().read_line(&mut input);
        let idx: usize = input.trim().parse().unwrap_or(0);
        if idx == 0 || idx > all_devs.len() {
            ui::err("选择无效。");
            pause_before_back();
            return;
        }
        &all_devs[idx - 1]
    };

    // 3. 选择模式
    println!("\n请选择重启模式:");
    println!("1. 系统 (normal)");
    println!("2. Recovery");
    println!("3. FastbootD");
    println!("4. Bootloader");
    println!("5. EDL (深刷模式)");
    print!("请输入选择 (1-5): ");
    let _ = io::stdout().flush();
    let mut mode_input = String::new();
    let _ = io::stdin().read_line(&mut mode_input);
    
    let target = match mode_input.trim() {
        "2" => Some("recovery"),
        "3" => Some("fastboot"),
        "4" => Some("bootloader"),
        "5" => Some("edl"),
        _ => None,
    };
    
    ui::step(&format!("正在重启设备 {} ...", selected_dev.serial));
    
    let res = match selected_dev.mode {
        rua_core::device::DeviceMode::ADB => {
            if let Ok(adb) = rua_core::AdbClient::new() {
                adb.reboot(&selected_dev.serial, target).await
            } else {
                Err(rua_core::FlashError::AdbError("无法连接 ADB".to_string()))
            }
        }
        _ => {
            let mut fb = client.clone();
            fb.set_serial(Some(selected_dev.serial.clone()));
            fb.reboot(target).await
        }
    };

    match res {
        Ok(_) => ui::ok("重启指令已发送。"),
        Err(e) => ui::err(&format!("重启失败: {:?}", e)),
    }
    
    pause_before_back();
}

async fn switch_slot(client: &FastbootClient) {
    ui::step("正在检测 Fastboot 设备...");
    let target_device = select_device(client).await;
    if target_device.is_empty() {
        ui::err("未检测到 Fastboot 设备，无法切换槽位。");
        pause_before_back();
        return;
    }

    print!("请输入要切换到的槽位 (a/b): ");
    let _ = io::stdout().flush();
    let mut slot = String::new();
    let _ = io::stdin().read_line(&mut slot);
    let slot = slot.trim().to_lowercase();
    if slot == "a" || slot == "b" {
        ui::step(&format!("正在切换到槽位 {} ...", slot));
        let mut fb = client.clone();
        fb.set_serial(Some(target_device));
        match fb.set_active(&slot).await {
            Ok(_) => ui::ok("切换成功！"),
            Err(e) => ui::err(&format!("切换失败: {:?}", e)),
        }
    } else {
        ui::err("无效的槽位标识。");
    }
    pause_before_back();
}

async fn activate_shizuku() {
    ui::step("正在激活 Shizuku...");
    let mut adb_devs = Vec::new();
    if let Ok(adb) = rua_core::AdbClient::new() {
        if let Ok(devs) = adb.list_devices().await {
            adb_devs = devs;
        }
    }

    if adb_devs.is_empty() {
        ui::err("未发现 ADB 模式的设备。");
    } else {
        let dev = if adb_devs.len() == 1 {
            &adb_devs[0]
        } else {
            println!("\n{} 请选择要激活 Shizuku 的设备:", ">>".cyan());
            for (i, d) in adb_devs.iter().enumerate() {
                println!("  {}. {} ({})", i + 1, d.serial, d.product.as_deref().unwrap_or("未知"));
            }
            print!("请选择: ");
            let _ = io::stdout().flush();
            let mut input = String::new();
            let _ = io::stdin().read_line(&mut input);
            let idx: usize = input.trim().parse().unwrap_or(0);
            if idx == 0 || idx > adb_devs.len() {
                ui::err("选择无效。");
                pause_before_back();
                return;
            }
            &adb_devs[idx - 1]
        };

        if let Ok(adb) = rua_core::AdbClient::new() {
            match adb.activate_shizuku(&dev.serial).await {
                Ok(out) => ui::ok(&format!("Shizuku 激活输出:\n{}", out)),
                Err(e) => ui::err(&format!("激活失败: {:?}", e)),
            }
        }
    }
    pause_before_back();
}

fn open_device_manager() {
    ui::step("正在打开设备管理器...");
    let _ = tokio::process::Command::new("devmgmt.msc").spawn();
}

fn pause_before_back() {
    print!("\n{}", "按回车键返回主菜单...".bright_black());
    let _ = io::stdout().flush();
    let mut unused = String::new();
    let _ = io::stdin().read_line(&mut unused);
}

#[derive(Debug, Clone)]
struct MagiskVersion {
    branch: String,
    version_name: String,
    path: PathBuf,
}

fn scan_magisk_folders(magisk_root: &Path) -> Vec<MagiskVersion> {
    let mut versions = Vec::new();

    if !magisk_root.exists() || !magisk_root.is_dir() {
        return versions;
    }

    for entry in fs::read_dir(magisk_root).unwrap() {
        if let Ok(entry) = entry {
            let branch_path = entry.path();
            if !branch_path.is_dir() {
                continue;
            }

            let branch_name = branch_path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if branch_name.starts_with('.') {
                continue;
            }

            for sub_entry in fs::read_dir(&branch_path).unwrap() {
                if let Ok(sub_entry) = sub_entry {
                    let version_path = sub_entry.path();
                    if !version_path.is_dir() {
                        continue;
                    }

                    let version_name = version_path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    if version_name.starts_with('.') {
                        continue;
                    }

                    versions.push(MagiskVersion {
                        branch: branch_name.clone(),
                        version_name,
                        path: version_path,
                    });
                }
            }
        }
    }

    versions
}

fn get_magisk_files_from_folder(folder: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    if !folder.exists() || !folder.is_dir() {
        return files;
    }

    for entry in fs::read_dir(folder).unwrap() {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.is_file() {
                let name = path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                let key = match name.as_str() {
                    "libmagiskinit.so" => "magiskinit".to_string(),
                    "libmagisk64.so" | "libmagisk.so" => "magiskbin".to_string(),
                    "libinit-ld.so" => "init_ld".to_string(),
                    "stub.apk" => "stub".to_string(),
                    _ => continue,
                };
                files.push((key, path));
            }
        }
    }

    files
}

fn select_partition() -> String {
    println!("\n{} {}", ">>".cyan().bold(), "请选择要修补的分区:".bright_white());
    let divider = "=".repeat(60).white();
    println!("{}", divider);
    println!("{}{}", format!("{:>3}. ", 1).bright_cyan(), "boot");
    println!("{}{}", format!("{:>3}. ", 2).bright_cyan(), "init_boot");
    println!("{}{}", format!("{:>3}. ", 3).bright_cyan(), "ramdisk");
    println!("{}", divider);

    print!("请选择: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);

    match input.trim() {
        "2" => "init_boot".to_string(),
        "3" => "ramdisk".to_string(),
        _ => "boot".to_string(),
    }
}

async fn select_device(client: &FastbootClient) -> String {
    ui::step("正在搜索设备...");
    match client.list_devices().await {
        Ok(devices) => {
            if devices.is_empty() {
                ui::err("未检测到任何设备。");
                return String::new();
            }

            let devices: Vec<&ConnectedDevice> = devices.iter().collect();

            println!("\n{} {}", ">>".cyan().bold(), "检测到以下设备:".bright_white());
            let divider = "=".repeat(60).white();
            println!("{}", divider);
            for (i, device) in devices.iter().enumerate() {
                println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(),
                    format!("{} [{}]", device.serial.yellow(), format!("{:?}", device.mode)).bright_white());
            }
            println!("{}", divider);

            print!("请选择设备: ");
            let _ = io::stdout().flush();
            let mut input = String::new();
            let _ = io::stdin().read_line(&mut input);

            match input.trim().parse::<usize>() {
                Ok(num) if num > 0 && num <= devices.len() => {
                    devices[num - 1].serial.clone()
                }
                _ => {
                    ui::err("无效的选择。");
                    String::new()
                }
            }
        }
        Err(e) => {
            ui::err(&format!("搜索设备失败: {:?}", e));
            String::new()
        }
    }
}
