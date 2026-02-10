mod constants;
mod fastboot_exec;
mod flash_part;
mod payload_utils;
mod ui;

use crate::constants::*;
use crate::fastboot_exec::FastbootManager;
use crate::flash_part::Flasher;
use clap::Parser;
use uuid::Uuid;
use colored::*;
use figlet_rs::FIGfont;
use rfd::FileDialog;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};

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

#[cfg(target_os = "windows")]
fn set_console_window_properties() {
    unsafe {
        let console_handle: HANDLE = GetStdHandle(STD_OUTPUT_HANDLE);
        if console_handle == std::ptr::null_mut() {
            return;
        }

        // Enable ANSI escape sequence processing
        let mut mode: u32 = 0;
        if GetConsoleMode(console_handle, &mut mode) != 0 {
            SetConsoleMode(console_handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }

        // Set console output code page to UTF-8
        SetConsoleOutputCP(65001);

        // Adjust font size on the current font
        let mut font_info_ex: CONSOLE_FONT_INFOEX = std::mem::zeroed();
        font_info_ex.cbSize = std::mem::size_of::<CONSOLE_FONT_INFOEX>() as u32;
        if GetCurrentConsoleFontEx(console_handle, FALSE, &mut font_info_ex) != FALSE {
            font_info_ex.dwFontSize.X = 0;
            font_info_ex.dwFontSize.Y = 18; // 调小字体
            SetCurrentConsoleFontEx(console_handle, FALSE, &font_info_ex);
        }

        let mut csbi: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(console_handle, &mut csbi) == 0 {
            return;
        }

        // Desired console window size (columns, rows)
        let new_cols: i16 = 100;
        let new_rows: i16 = 52; // 根据内容精确调整高度，防止产生滚动条

        // Set screen buffer size
        let new_buffer_size = COORD { X: new_cols, Y: 2000 }; 
        if SetConsoleScreenBufferSize(console_handle, new_buffer_size) == FALSE {
            let fallback_buffer_size = COORD { X: new_cols, Y: new_rows };
            SetConsoleScreenBufferSize(console_handle, fallback_buffer_size);
        }

        // Get actual font size to calculate window dimensions correctly
        let mut font_info_actual: CONSOLE_FONT_INFOEX = std::mem::zeroed();
        font_info_actual.cbSize = std::mem::size_of::<CONSOLE_FONT_INFOEX>() as u32;
        GetCurrentConsoleFontEx(console_handle, FALSE, &mut font_info_actual);
        
        let font_w = if font_info_actual.dwFontSize.X == 0 { 12 } else { font_info_actual.dwFontSize.X as i32 };
        let font_h = font_info_actual.dwFontSize.Y as i32;

        // Set console window size
        let mut console_window_rect = SMALL_RECT {
            Left: 0,
            Top: 0,
            Right: new_cols - 1,
            Bottom: new_rows - 1,
        };
        SetConsoleWindowInfo(console_handle, FALSE, &mut console_window_rect);

        // Try to resize using ANSI escape sequence (works in some Windows Terminal configurations)
        // Repeat it to increase the chance of the terminal responding
        for _ in 0..3 {
            print!("\x1b[8;{};{}t", new_rows, new_cols);
            let _ = io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Center the console window with proper frame adjustment
        let hwnd: HWND = GetConsoleWindow();
        if hwnd != std::ptr::null_mut() {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut monitor_info: MONITORINFO = std::mem::zeroed();
            monitor_info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            
            if GetMonitorInfoW(monitor, &mut monitor_info) != FALSE {
                let screen_width = monitor_info.rcMonitor.right - monitor_info.rcMonitor.left;
                let screen_height = monitor_info.rcMonitor.bottom - monitor_info.rcMonitor.top;

                // Account for window decorations (title bar, borders) - approx 40px width, 80px height
                let window_width = (new_cols as i32 * font_w) + 40;
                let window_height = (new_rows as i32 * font_h) + 80;

                let x = (screen_width - window_width) / 2;
                let y = (screen_height - window_height) / 2;

                MoveWindow(hwnd, x, y, window_width, window_height, 1);
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::SetTrue)]
    debug: bool,
}

fn main() {
    #[cfg(target_os = "windows")]
    set_console_window_properties();

    let args = Args::parse();
    
    // 设置 Ctrl+C 处理
    ctrlc::set_handler(move || {
        if INTERRUPTED.load(Ordering::SeqCst) {
            // 如果已经是中断状态，第二次按下则强行退出
            std::process::exit(130);
        }
        INTERRUPTED.store(true, Ordering::SeqCst);
        println!("{}", "\n\n>> [中断] 收到推出信号，退出中...".yellow().bold());
    }).expect("Error setting Ctrl-C handler");

    if let Err(e) = run_interactive_loop(args.debug) {
        eprintln!("{}", format!("程序发生异常错误: {:?}", e).red());
    }
}

fn clear_screen() {
    // \x1b[3J 清除滚动回溯缓冲区
    // \x1b[2J 清除当前屏幕内容
    // \x1b[H  将光标重置到左上角 (1,1)
    // \x1b[?1049h 切换到备用屏幕缓冲区 (类似于 vim/less)，这样就不会有滚动条
    print!("\x1b[?1049h\x1b[3J\x1b[2J\x1b[H");
    let _ = io::stdout().flush();
}

fn print_header() {
    println!(); // 仅保留一个空行，防止顶部过于拥挤
    // 使用库生成的标准 FIGlet 字体
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
}

fn refresh_ui() {
    clear_screen();
    print_header();
    let divider = "=".repeat(100).white();
    println!("{}", divider); // 移除这里的额外 \n
    for warning in WARNING_TEXTS {
        println!("{}", warning.red().bold());
    }
    println!("{}", divider);
    for info in INFO_TEXTS {
        println!("{}", info.green());
    }
    println!("{}", divider);

    for (id, desc) in MENU_OPTIONS.iter() {
        let item_prefix = format!("{:>2}. ", id);
        let colored_prefix = item_prefix.bright_cyan();
        println!("{}{}", colored_prefix, desc);
    }

    println!("{}", divider);
}

fn run_interactive_loop(debug: bool) -> Result<(), ReadlineError> {
    let mut rl = DefaultEditor::new()?;
    loop {
        refresh_ui();
        let prompt = format!("> ");
        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                INTERRUPTED.store(false, Ordering::SeqCst); // 每次新指令开始前重置状态
                let input = line.trim();
                if input.is_empty() { continue; }
                let _ = rl.add_history_entry(input);
                match input.to_lowercase().as_str() {
                    "0" => {
                        println!("{}", "\n喵呜~ 下次再见！".green());
                        break;
                    }
                    choice => {
                        handle_menu_action(choice, debug);
                        if INTERRUPTED.load(Ordering::SeqCst) {
                            println!("{}", "\n>> 任务已被用户取消。".yellow());
                        }
                        pause_before_back();
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("{}", "\n已通过 Ctrl+C 退出".yellow());
                break;
            }
            Err(ReadlineError::Eof) => {
                println!("{}", "\n已通过结束符退出".yellow());
                break;
            },
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn handle_menu_action(choice: &str, _debug: bool) {
    println!(); 
    
    let fb = match FastbootManager::new() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{}", format!("[!] 无法初始化 Fastboot 管理器: {:?}", e).red());
            return;
        }
    };
    let flasher = Flasher::new().unwrap();

    // 统一的 Fastboot 设备检测逻辑
    let requires_fastboot_device = matches!(choice, "1" | "3" | "4" | "6" | "7" | "9" | "10" | "11" | "12" | "13");
    if requires_fastboot_device {
        println!("{}", "\n>> 正在检测设备连接...".cyan());
        if !fb.wait_for_device().unwrap_or(false) {
            eprintln!("{}", "[!] 未检测到 Fastboot 设备，操作已取消。".red());
            return;
        }
    }

    match choice {
        "1" => {
            println!("{}", ">> [1] Fastboot一键刷入线刷包 (小米专用)".bright_green());
            if let Some(dir) = select_directory() {
                let mut available_scripts = Vec::new();
                let scripts = [
                    ("flash_all.bat", "刷入全部并清除Data (flash_all.bat)"),
                    ("flash_all_lock.bat", "刷入全部并回锁 (flash_all_lock.bat)"),
                    ("flash_all_expect_storage.bat", "刷入全部并保留Data (flash_all_expect_storage.bat)"),
                ];

                for (filename, desc) in scripts {
                    if dir.join(filename).exists() {
                        available_scripts.push((filename, desc));
                    }
                }

                if available_scripts.is_empty() {
                    eprintln!("{}", "错误: 未在目录下找到任何小米线刷脚本 (.bat)".red());
                } else {
                    println!("{}", ">> 请选择要执行的刷机脚本:".cyan());
                    for (i, (_, desc)) in available_scripts.iter().enumerate() {
                        println!("   {}. {}", i + 1, desc);
                    }

                    let mut rl = DefaultEditor::new().unwrap();
                    let choice_idx = match rl.readline("请输入序号 (直接回车取消): ") {
                        Ok(line) => line.trim().parse::<usize>().unwrap_or(0),
                        Err(_) => 0,
                    };

                    if choice_idx > 0 && choice_idx <= available_scripts.len() {
                        let (selected_bat, _) = available_scripts[choice_idx - 1];
                        println!("{}", format!(">> 正在执行脚本: {}", selected_bat).cyan());
                        let status = Command::new("cmd")
                            .arg("/c")
                            .arg(selected_bat)
                            .current_dir(&dir)
                            .status();
                        match status {
                            Ok(s) if s.success() => println!("{}", "成功: 脚本执行完成".green()),
                            _ => eprintln!("{}", "失败: 脚本执行过程中出错".red()),
                        }
                    } else {
                        println!("{}", "已取消操作。".yellow());
                    }
                }
            }
        }
        "2" => {
            println!("{}", ">> [2] Fastboot一键刷入卡刷包 (适用小米、一加)".bright_green());
            println!("{}", ">> 请选择卡刷包 (.zip) 或 payload.bin (此操作也会尝试进入 FastbootD 模式)...".yellow());
            
            if let Some(file_path) = select_file_ext(&["zip", "bin"]) {
                let out_dir = Path::new("extracted_images");
                
                // 1. 解包
                println!("{}", ">> 正在准备解包环境...".cyan());
                let rt = tokio::runtime::Runtime::new().unwrap();
                if let Err(e) = rt.block_on(crate::payload_utils::unpack_payload(&file_path, out_dir)) {
                    eprintln!("{}", format!("错误: 解包失败: {:?}", e).red());
                    return;
                }
                
                // 2. 准备刷机环境 (参考选项 4)
                println!("{}", "\n>> 正在检测设备连接...".cyan());
                loop {
                    if let Ok(devices) = fb.get_devices() {
                        if !devices.is_empty() {
                            break;
                        }
                    }
                    
                    if INTERRUPTED.load(Ordering::SeqCst) {
                         println!("{}", ">> 等待已被用户取消。".yellow());
                         return; // 或者 break 继续后面的逻辑（如果不强制依赖设备）
                    }
                    
                    print!("\r{}", ">> 未检测到设备，请连接 USB 线... (按 Ctrl+C 取消)".yellow());
                    let _ = io::stdout().flush();
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                println!(); // 换行

                if !fb.is_fastbootd() {
                    println!("{}", "\n>> 检测到当前并非 FastbootD 模式，卡刷包内的逻辑分区刷入可能会失败。".yellow());
                    println!("{}", ">> 是否尝试切换到 FastbootD 模式？(y/n)".cyan());
                    let mut rl = DefaultEditor::new().unwrap();
                    if let Ok(line) = rl.readline("> ") {
                        if line.trim().to_lowercase() == "y" {
                            println!("{}", ">> 正在进入 FastbootD 模式...".cyan());
                            if let Ok(_) = fb.reboot(Some("fastboot")) {
                                println!("{}", ">> 等待设备确认...".yellow());
                                let _ = fb.wait_for_device();
                            }
                        }
                    }
                }
                
                // 3. 按照选项 4 进行刷入
                println!("{}", "\n>> 开始刷入解包后的镜像...".green());
                let _ = flash_all_images_in_dir(&fb, out_dir);
                
                println!("{}", "\n>> 卡刷包刷入完成！建议清除数据以防无限重启喵~".bright_green());
            }
        }
        "3" | "4" => {
            let is_fastbootd_target = choice == "4";
            let title = if is_fastbootd_target { "[4] FastbootD 模式一键刷机" } else { "[3] Fastboot 一键刷机" };
            crate::ui::title(title);
            
            if is_fastbootd_target {
                if fb.is_fastbootd() {
                    println!("{}", ">> 检测到已处于 FastbootD 模式，直接开始刷机...".green());
                } else {
                    crate::ui::step("正在进入 FastbootD 模式...");
                    if let Ok(_) = fb.reboot(Some("fastboot")) {
                        crate::ui::warn("已发送重启指令，等待设备进入 FastbootD...");
                        let _ = fb.wait_for_device();
                    }
                }
            }
            
            if let Some(dir) = select_directory() {
                let _ = flash_all_images_in_dir(&fb, &dir);
            }
        }
        "5" => {
            println!("{}", ">> [5] 正在启动驱动安装程序...".bright_green());
            let mut driver_path = std::env::current_dir().unwrap();
            driver_path.push("drivers");
            driver_path.push("QcomMtk_Driver_Setup_3.2.1.exe");
            if driver_path.exists() {
                let _ = Command::new("cmd").arg("/c").arg("start").arg("").arg(driver_path).spawn();
            } else {
                eprintln!("{}", "错误: 未找到驱动安装程序".red());
            }
        }
        "6" => {
            println!("{}", "⚠️  警告: 解锁 Bootloader 会清除手机所有数据！".red().bold());
            println!("{}", ">> 确定要执行吗？(y/n)".cyan());
            let mut rl = DefaultEditor::new().unwrap();
            if let Ok(line) = rl.readline("> ") {
                if line.trim().to_lowercase() == "y" {
                    println!("{}", ">> [6] 通用一键解锁 Bootloader".bright_green());
                    let _ = fb.flashing_unlock();
                } else {
                    println!("{}", "已取消操作。".green());
                }
            }
        }
        "7" => {
            println!("{}", "⚠️  警告: 回锁 Bootloader 极其危险！\n如果系统未恢复官方原版，可能导致永久变砖且无法救回！".red().bold());
            println!("{}", ">> 确定要执行吗？(y/n)".cyan());
            let mut rl = DefaultEditor::new().unwrap();
            if let Ok(line) = rl.readline("> ") {
                if line.trim().to_lowercase() == "y" {
                    println!("{}", ">> [7] 通用一键回锁 Bootloader".bright_green());
                    let _ = fb.flashing_lock();
                } else {
                    println!("{}", "已取消操作。".green());
                }
            }
        }
        "8" => {
            println!("{}", ">> [8] 正在跳转到小米解锁工具下载页面...".bright_green());
            let _ = webbrowser::open("https://www.miui.com/unlock/download.html");
        }
        "9" | "10" | "11" | "12" => {
            let (label, path_name) = match choice {
                "9" => ("Magisk", "Magisk/Magisk"),
                "10" => ("Magisk Alpha", "Magisk/Alpha"),
                "11" => ("Kitsune Mask", "Magisk/Kitsune"),
                "12" => ("APatch", "APatch"),
                _ => unreachable!(),
            };
            println!("{}", format!(">> [{}] 一键修补并刷入 {}", choice, label).bright_green());
            
            if choice == "12" {
                // 1. 询问 SuperKey
                let mut rl = DefaultEditor::new().unwrap();
                let skey = match rl.readline("请输入 SuperKey (直接回车随机生成): ") {
                    Ok(line) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            let uuid = Uuid::new_v4().to_string();
                            println!("{}", format!(">> 未输入，已随机生成 SuperKey: {}", uuid).yellow());
                            uuid
                        } else {
                            trimmed.to_string()
                        }
                    },
                    Err(_) => {
                         let uuid = Uuid::new_v4().to_string();
                         println!("{}", format!(">> 读取失败，已随机生成 SuperKey: {}", uuid).yellow());
                         uuid
                    },
                };

                // 2. 询问分区
                println!("{}", ">> 请选择目标分区类型:".cyan());
                println!("   1. boot   (默认，一般机型)");
                println!("   2. kernel (适用于华为/荣耀等)");
                let target_partition = match rl.readline("请输入序号 (默认为 1): ") {
                    Ok(line) => if line.trim() == "2" { "kernel" } else { "boot" },
                    Err(_) => "boot"
                };

                println!("{}", format!(">> 请选择要修补的原始 {} 镜像 (.img)...", target_partition).yellow());
                if let Some(boot_img) = select_file_ext(&["img"]) {
                     if let Err(e) = flasher.apatch_patch(boot_img.to_str().unwrap(), &skey, target_partition) {
                         eprintln!("{}", format!("错误: APatch 修补失败: {:?}", e).red());
                     }
                }
            } else {
                if let Some(apk) = select_apk_from_dir(path_name) {
                    println!("{}", ">> 请选择目标分区类型:".cyan());
                    println!("   1. boot      (适用于 Android 12 及以下，或部分无 init_boot 机型)");
                    println!("   2. ramdisk   (适用于部分华为/荣耀设备)");
                    println!("   3. init_boot (适用于 Android 13+ 出厂机型，默认)");
                    
                    let mut rl = DefaultEditor::new().unwrap();
                    let target_partition = match rl.readline("请输入序号 (默认为 3): ") {
                         Ok(line) => {
                             match line.trim() {
                                 "1" => "boot",
                                 "2" => "ramdisk",
                                 _ => "init_boot",
                             }
                         },
                         Err(_) => "init_boot", 
                    };

                    println!("{}", format!(">> 请选择要修补的原始 {} 镜像 (.img)...", target_partition).yellow());
                    if let Some(boot_img) = select_file_ext(&["img"]) {
                        if let Err(e) = flasher.magisk_patch(boot_img.to_str().unwrap(), apk.to_str().unwrap(), target_partition) {
                            eprintln!("{}", format!("错误: 修补执行失败: {:?}", e).red());
                        }
                    }
                }
            }
        }
        "13" => {
            println!("{}", ">> [13] 一键 Root 刷入 KernelSU LKM 模式".bright_green());
            
            // 1. 动态扫描 LKM 目录下的分支
            let lkm_base = Path::new("LKM");
            let mut variants = Vec::new();

            if let Ok(entries) = fs::read_dir(lkm_base) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let path = entry.path();
                        if path.is_dir() {
                            let name = path.file_name().unwrap().to_str().unwrap().to_string();
                            // 读取版本号
                            let version_file = path.join("VERSION");
                            let version = if version_file.exists() {
                                fs::read_to_string(version_file).unwrap_or_default().trim().to_string()
                            } else {
                                "未知版本".to_string()
                            };
                            variants.push((name, version));
                        }
                    }
                }
            }
            variants.sort_by(|a, b| b.0.cmp(&a.0)); // 降序排列

            if variants.is_empty() {
                println!("{}", "[!] 未在 LKM 目录下发现任何分支目录。".red());
            } else {
                crate::ui::step("请选择要使用的 KernelSU 分支:");
                for (i, (name, ver)) in variants.iter().enumerate() {
                    println!("   {}. {:<15} (版本号: {})", i + 1, name.bright_white(), ver.yellow());
                }

                let mut rl = DefaultEditor::new().unwrap();
                let variant_idx = if let Ok(line) = rl.readline("请输入序号: ") {
                    line.trim().parse::<usize>().unwrap_or(0)
                } else { 0 };

                if variant_idx == 0 || variant_idx > variants.len() {
                    println!("{}", "[!] 无效选择，已取消。".red());
                } else {
                    let (variant_name, _) = &variants[variant_idx - 1];
                    let variant_path = format!("LKM/{}", variant_name);

                    // 2. 扫描并选择 KMI
                    if let Some(selected_ko) = select_lkm_kmi(&variant_path) {
                        let kmi = selected_ko.file_stem().unwrap().to_str().unwrap().replace("_kernelsu", "");
                        println!("{}", format!(">> 已选择 KMI: {}", kmi).green());
                        
                        // 3. 询问分区 (同 Magisk)
                        crate::ui::step("请选择目标分区类型:");
                        println!("   1. boot");
                        println!("   2. ramdisk");
                        println!("   3. init_boot (默认)");
                         let target_partition = match rl.readline("请输入序号 (默认为 3): ") {
                             Ok(line) => {
                                 match line.trim() {
                                     "1" => "boot",
                                     "2" => "ramdisk",
                                     _ => "init_boot",
                                 }
                             },
                             Err(_) => "init_boot", 
                        };

                        crate::ui::warn(&format!("请选择原始 {} 镜像 (.img)...", target_partition));
                        if let Some(orig_img) = select_file_ext(&["img"]) {
                            let mut ksuinit_base = std::env::current_dir().unwrap();
                            ksuinit_base.push("ksuinit");
                            ksuinit_base.push(variant_name);
                            let ksuinit_path = ksuinit_base.join("ksuinit");
                            let ksuinit_d_path = ksuinit_base.join("ksuinit.d");
                            if !ksuinit_path.exists() {
                                crate::ui::err(&format!("[!] 未找到 ksuinit: {:?}", ksuinit_path));
                            } else {
                                let ksuinit_d_opt = if ksuinit_d_path.exists() { Some(ksuinit_d_path.to_str().unwrap().to_string()) } else { None };
                                let ksuinit_d_opt_ref = ksuinit_d_opt.as_ref().map(|s| s.as_str());
                                if let Err(e) = flasher.kernelsu_lkm_install(
                                    orig_img.to_str().unwrap(),
                                    ksuinit_path.to_str().unwrap(),
                                    ksuinit_d_opt_ref,
                                    selected_ko.to_str().unwrap(),
                                    target_partition
                                ) {
                                    crate::ui::err(&format!("错误: KernelSU LKM 修补失败: {:?}", e));
                                }
                            }
                        }
                    }
                }
            }
        }
        "14" => {
            println!("{}", ">> [14] 一键 Root 自选 AnyKernel3 刷入".bright_green());
            
            // 询问分区 (同 APatch)
            println!("{}", ">> 请选择目标分区类型:".cyan());
            println!("   1. boot   (默认)");
            println!("   2. kernel (适用于华为/荣耀等)");
            let mut rl = DefaultEditor::new().unwrap();
            let target_partition = match rl.readline("请输入序号 (默认为 1): ") {
                Ok(line) => if line.trim() == "2" { "kernel" } else { "boot" },
                Err(_) => "boot"
            };

            if let Some(zip_path) = select_file_ext(&["zip"]) {
                println!("{}", format!(">> 请选择要修补的原始 {} 镜像 (.img)...", target_partition).yellow());
                if let Some(boot_img_path) = select_file_ext(&["img"]) {
                    let _ = flasher.anykernel3_root(zip_path.to_str().unwrap(), boot_img_path.to_str().unwrap(), target_partition);
                }
            }
        }
        "15" => {
            println!("{}", ">> [15] 一键刷入 Boot".bright_green());
            if let Some(file_path) = select_file() {
                let _ = flasher.flash_boot(file_path.to_str().unwrap());
            }
        }
        "16" => {
            println!("{}", ">> [16] 一键关闭 AVB (刷入 vbmeta 并禁用校验)".bright_green());
            if let Some(file_path) = select_file() {
                let _ = flasher.flash_vbmeta(file_path.to_str().unwrap());
            }
        }
        "17" => {
            println!("{}", ">> [17] 自定义选择分区刷入".bright_green());
            let mut rl = DefaultEditor::new().unwrap();
            if let Ok(partition) = rl.readline("请输入分区名 (例如 boot): ") {
                let partition = partition.trim();
                if !partition.is_empty() {
                    if let Some(file_path) = select_file() {
                        let _ = flasher.flash_partition(partition, file_path.to_str().unwrap());
                    }
                }
            }
        }
        "18" => {
            println!("{}", ">> [18] 正在打开 platform-tools 命令行窗口...".bright_green());
            let mut pt_path = std::env::current_dir().unwrap();
            pt_path.push("platform-tools");
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("cmd").arg("/c").arg("start").arg("cmd").current_dir(pt_path).spawn();
            }
        }
        "19" => {
            println!("{}", ">> [19] 正在检测设备连接状态...".bright_green());
            match fb.get_devices() {
                Ok(devices) if devices.is_empty() => {
                    println!("{}", "[!] 未发现处于 Fastboot/ADB 模式的设备。".red());
                }
                Ok(devices) => {
                    println!("{}", format!(">> 已发现 {} 个设备:", devices.len()).green());
                    for (i, dev) in devices.iter().enumerate() {
                        print!("   {}. 序列号: {}", i + 1, dev.serial.bright_cyan());
                        
                        // 根据模式安全地获取型号
                        if dev.mode == "Fastboot" {
                            if let Ok(prod) = fb.get_var("product") {
                                print!("  型号: {} (Fastboot)", prod.yellow());
                            } else {
                                print!(" (Fastboot)");
                            }
                        } else if dev.mode == "ADB" && dev.status == "device" {
                            if let Ok(model) = fb.get_adb_var(&dev.serial, "ro.product.model") {
                                print!("  型号: {} (ADB)", model.yellow());
                            } else {
                                print!(" (ADB)");
                            }
                        } else {
                            print!(" ({})", dev.mode.cyan());
                        }

                        // 仅在 Fastboot 模式尝试获取槽位
                        if dev.mode == "Fastboot" {
                            if let Ok(slot) = fb.get_current_slot() {
                                print!("  当前槽位: {}", slot.magenta());
                            }
                        }
                        println!();
                    }
                }
                Err(e) => eprintln!("{}", format!("[!] 检测失败: {:?}", e).red()),
            }
        }
        "20" => {
            println!("{}", ">> [20] 正在启动 ADB 投屏 (scrcpy)...".bright_green());
            let mut scrcpy_path = std::env::current_dir().unwrap();
            scrcpy_path.push("scrcpy");
            
            let exe_name = if cfg!(target_os = "windows") { "scrcpy.exe" } else { "scrcpy" };
            let exe_path = scrcpy_path.join(exe_name);

            if exe_path.exists() {
                #[cfg(target_os = "windows")]
                {
                    // 使用 cmd /c start 以新窗口/分离模式启动
                    let _ = Command::new("cmd")
                        .arg("/c")
                        .arg("start")
                        .arg("") // 标题占位符
                        .arg(exe_name)
                        .current_dir(&scrcpy_path)
                        .spawn();
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = Command::new(&exe_path).current_dir(&scrcpy_path).spawn();
                }
            } else {
                eprintln!("{}", "错误: 未找到 scrcpy 执行文件".red());
            }
        }
        "21" => {
            println!("{}", ">> [21] 一键安装 APK (ADB)".bright_green());
            if let Some(file_path) = select_file_ext(&["apk"]) {
                let _ = fb.run_adb_cmd(&["install", file_path.to_str().unwrap()]);
            }
        }
        "22" => {
            println!("{}", ">> [22] 恢复出厂设置 (Fastboot)".bright_green());
            let _ = fb.format_w();
        }
        "23" => {
            println!("{}", ">> [23] 正在重启到 系统...".bright_green());
            let _ = fb.reboot(None);
        }
        "24" => {
            println!("{}", ">> [24] 正在重启到 Recovery...".bright_green());
            let _ = fb.reboot(Some("recovery"));
        }
        "25" => {
            println!("{}", ">> [25] 正在重启到 FastbootD...".bright_green());
            let _ = fb.reboot(Some("fastboot"));
        }
        "26" => {
            println!("{}", ">> [26] 正在重启到 Bootloader...".bright_green());
            let _ = fb.reboot(Some("bootloader"));
        }
        "27" => {
            println!("{}", ">> [27] 正在重启到 9008 (EDL)/深刷模式...".bright_green());
            let _ = fb.reboot_edl();
        }
        "28" => {
            println!("{}", ">> [28] 切换活动槽位 (极其危险)".bright_red().bold());
            let current = fb.get_current_slot().unwrap_or_else(|_| "unknown".to_string());
            println!("{}", format!(">> 当前活动槽位: {}", current.magenta()).yellow());
            
            println!("{}", "请输入目标槽位 (a 或 b):".cyan());
            let mut rl = DefaultEditor::new().unwrap();
            if let Ok(target) = rl.readline("> ") {
                let target = target.trim().to_lowercase();
                if target == "a" || target == "b" {
                    println!("{}", format!("⚠️  警告: 即将切换到槽位 {}，确定吗？(y/n)", target).red().bold());
                    if let Ok(confirm) = rl.readline("> ") {
                        if confirm.trim().to_lowercase() == "y" {
                            let _ = fb.set_active_slot(&target);
                        } else {
                            println!("{}", "已取消操作。".green());
                        }
                    }
                } else {
                    println!("{}", "错误: 无效的槽位名称。".red());
                }
            }
        }
        "29" => {
            println!("{}", ">> [29] 激活 Shizuku 功能".bright_green());
            println!("{}", ">> 正在检测 Shizuku 安装状态...".cyan());
            let installed = fb.run_adb_cmd(&["shell", "ls", "/sdcard/Android/data/moe.shizuku.privileged.api/start.sh"]).unwrap_or(false);
            if !installed {
                eprintln!("{}", "错误: 未检测到 Shizuku 安装或启动脚本不存在".red());
            } else {
                println!("{}", ">> 正在启动 Shizuku...".cyan());
                match fb.run_adb_cmd(&["shell", "sh", "/sdcard/Android/data/moe.shizuku.privileged.api/start.sh"]) {
                    Ok(true) => println!("{}", "成功: 已尝试启动 Shizuku".green()),
                    _ => eprintln!("{}", "失败: 启动 Shizuku 时出错".red()),
                }
            }
        }
        "30" => {
            println!("{}", ">> [30] 激活 AxManager 功能".bright_green());
            println!("{}", ">> 正在查找 AxManager 包路径...".cyan());
            
            // 使用 pm path 获取包的实际安装路径
            let pkg_name = "frb.axeron.manager";
            let pt_cmd = fb.get_adb_path();
            let output = std::process::Command::new(pt_cmd)
                .args(["shell", "pm", "path", pkg_name])
                .output();

            if let Ok(out) = output {
                let path_str = String::from_utf8_lossy(&out.stdout);
                if path_str.contains("package:") {
                    // 格式通常是 package:/data/app/~~.../base.apk
                    // 我们需要提取路径并定位到 lib/arm64/libaxeron.so
                    let apk_path = path_str.trim().replace("package:", "");
                    let pkg_dir = std::path::Path::new(&apk_path).parent().unwrap();
                    let lib_path = pkg_dir.join("lib/arm64/libaxeron.so");
                    let lib_path_str = lib_path.to_str().unwrap().replace("\\", "/");

                    println!("{}", format!(">> 找到路径: {}", lib_path_str).green());
                    println!("{}", ">> 正在启动 AxManager...".cyan());
                    
                    match fb.run_adb_cmd(&["shell", &lib_path_str]) {
                        Ok(true) => println!("{}", "成功: 已尝试启动 AxManager".green()),
                        _ => eprintln!("{}", "失败: 启动 AxManager 时出错".red()),
                    }
                } else {
                    eprintln!("{}", "错误: 未找到 AxManager 安装路径，请确认是否已安装 Axeron Manager".red());
                }
            } else {
                eprintln!("{}", "错误: 无法获取包信息".red());
            }
        }
        "31" => {
            println!("{}", ">> [31] 正在打开设备管理器...".bright_green());
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("cmd")
                    .args(["/c", "start", "devmgmt.msc"])
                    .spawn();
            }
        }

        _ if MENU_OPTIONS.iter().any(|(id, _)| *id == choice) => {
            println!("{}", format!(">> 功能 [{}] 正在开发中，敬请期待喵~", choice).yellow());
        }
        _ => println!("{}", format!("[!] 无效选项 '{}'，请重新选择。", choice).red()),
    }
}

fn select_apk_from_dir(dir_name: &str) -> Option<PathBuf> {
    let mut path = std::env::current_dir().unwrap();
    path.push(dir_name);
    if !path.exists() {
        // 如果目录不存在，尝试直接在当前目录找 APK
        path = std::env::current_dir().unwrap();
    }
    let entries = fs::read_dir(&path).ok()?;
    let mut files = Vec::new();
    for entry in entries {
        if let Ok(entry) = entry {
            let p = entry.path();
            if p.is_file() && p.extension().map_or(false, |ext| ext == "apk") {
                files.push(p);
            }
        }
    }
    if files.is_empty() { return None; }
    
    // 按文件名降序排列 (版本号高的在前)
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    println!("{}", format!(">> 请选择要使用的 {} 版本:", dir_name).cyan());
    for (i, f) in files.iter().enumerate() {
        println!("   {}. {}", i + 1, f.file_name().unwrap().to_str().unwrap());
    }
    let mut rl = DefaultEditor::new().unwrap();
    if let Ok(line) = rl.readline("请输入序号: ") {
        if let Ok(idx) = line.trim().parse::<usize>() {
            if idx > 0 && idx <= files.len() {
                return Some(files[idx - 1].clone());
            }
        }
    }
    None
}

fn select_directory() -> Option<PathBuf> {
    println!("{}", ">> 请在弹出的对话框中选择目录:".cyan());
    if let Some(path) = FileDialog::new().pick_folder() {
        return Some(path);
    }
    println!("{}", ">> 请手动输入路径:".yellow());
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let path = PathBuf::from(input.trim());
        if path.exists() && path.is_dir() { return Some(path); }
    }
    None
}

fn select_file_ext(extensions: &[&str]) -> Option<PathBuf> {
    if let Some(path) = FileDialog::new().add_filter("支持的文件", extensions).pick_file() {
        return Some(path);
    }
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let path = PathBuf::from(input.trim());
        if path.exists() { return Some(path); }
    }
    None
}

fn select_file() -> Option<PathBuf> { select_file_ext(&["img", "bin"]) }

fn flash_all_images_in_dir(fb: &FastbootManager, dir: &Path) -> io::Result<()> {
    let entries = fs::read_dir(dir)?;
    for entry in entries {
        if INTERRUPTED.load(Ordering::SeqCst) {
            println!("{}", ">> [警告] 刷机已被用户中断！".red().bold());
            return Err(io::Error::new(io::ErrorKind::Interrupted, "User interrupted"));
        }
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |ext| ext == "img") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem == "userdata" { continue; }
                let _ = fb.flash(stem, path.to_str().unwrap_or_default());
            }
        }
    }
    Ok(())
}

fn pause_before_back() {
    print!("\n{}", "按回车键返回主菜单...".bright_black());
    let _ = io::stdout().flush();
    let mut unused = String::new();
    let _ = io::stdin().read_line(&mut unused);
}
fn select_lkm_kmi(dir_path: &str) -> Option<PathBuf> {
    let mut path = std::env::current_dir().unwrap();
    path.push(dir_path);
    if !path.exists() {
        println!("{}", format!("[!] 目录不存在: {:?}", path).red());
        return None;
    }

    let entries = fs::read_dir(&path).ok()?;
    let mut files = Vec::new();
    for entry in entries {
        if let Ok(entry) = entry {
            let p = entry.path();
            if p.is_file() && p.extension().map_or(false, |ext| ext == "ko") {
                files.push(p);
            }
        }
    }

    if files.is_empty() { return None; }

    // 按名称排序，方便查找
    files.sort();

    println!("{}", "\n>> 请选择内核 KMI 版本:".cyan());
    for (i, f) in files.iter().enumerate() {
        let name = f.file_stem().unwrap().to_str().unwrap();
        // 去掉 _kernelsu 后缀展示给用户看
        let kmi = name.replace("_kernelsu", "");
        println!("   {:>2}. {}", i + 1, kmi);
    }

    let mut rl = DefaultEditor::new().unwrap();
    if let Ok(line) = rl.readline("请输入序号: ") {
        if let Ok(idx) = line.trim().parse::<usize>() {
            if idx > 0 && idx <= files.len() {
                return Some(files[idx - 1].clone());
            }
        }
    }
    None
}
