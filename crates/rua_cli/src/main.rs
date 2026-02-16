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
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};
use std::collections::{HashMap, HashSet};
use std::time::{Instant, Duration};

struct PartitionStat { total: u64, start: Instant, elapsed: Option<Duration> }
struct ConsoleReporter { pb: Mutex<Option<ProgressBar>>, stats: Mutex<HashMap<String, PartitionStat>> }
impl ConsoleReporter {
    fn new() -> Self { Self { pb: Mutex::new(None), stats: Mutex::new(HashMap::new()) } }
    fn clear_current(&self, msg: &str) {
        if let Some(pb) = self.pb.lock().unwrap().take() {
            pb.finish_and_clear();
            println!("{}", msg);
        }
    }
    fn print_summary(&self) {
        let stats = self.stats.lock().unwrap();
        if stats.is_empty() { return; }
        let mut total_bytes: u128 = 0;
        let mut total_secs: f64 = 0.0;
        let mut max_speed: f64 = 0.0;
        let mut max_name = String::new();
        let mut min_speed: f64 = f64::MAX;
        let mut min_name = String::new();
        for (name, s) in stats.iter() {
            if let Some(el) = s.elapsed {
                let secs = el.as_secs_f64().max(1e-6);
                let speed = (s.total as f64) / secs / (1024.0 * 1024.0);
                total_bytes += s.total as u128;
                total_secs += secs;
                if speed > max_speed { max_speed = speed; max_name = name.clone(); }
                if speed < min_speed { min_speed = speed; min_name = name.clone(); }
            }
        }
        if total_secs > 0.0 {
            let avg = (total_bytes as f64) / total_secs / (1024.0 * 1024.0);
            println!("\n统计: 分区数 {}  平均速度 {:.2} MiB/s  最高 {:.2} MiB/s [{}]  最低 {:.2} MiB/s [{}]",
                stats.len(), avg, max_speed, max_name, min_speed, min_name);
        } else {
            println!("\n统计: 分区数 {}", stats.len());
        }
    }
}
impl ProgressReporter for ConsoleReporter {
    fn should_cancel(&self) -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }
    fn on_start(&self, name: &str, total: u64) {
        let pb = if total > 0 { ProgressBar::new(total) } else { ProgressBar::new_spinner() };
        let style = ProgressStyle::with_template("{spinner} {msg} [{elapsed_precise}<{eta_precise}] {wide_bar} {bytes}/{total_bytes} {bytes_per_sec}").unwrap()
            .tick_strings(&["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"]);
        pb.set_style(style);
        pb.set_message(format!("解包 {}", name));
        *self.pb.lock().unwrap() = Some(pb);
        self.stats.lock().unwrap().insert(name.to_string(), PartitionStat { total, start: Instant::now(), elapsed: None });
    }
    fn on_progress(&self, _name: &str, current: u64, total: u64) {
        if let Some(pb) = self.pb.lock().unwrap().as_ref() {
            if total > 0 { pb.set_position(current); }
            pb.tick();
        }
    }
    fn on_complete(&self, name: &str, _total: u64) {
        if let Some(pb) = self.pb.lock().unwrap().take() {
            pb.finish_with_message(format!("{} 完成", name));
        }
        if let Some(s) = self.stats.lock().unwrap().get_mut(name) {
            s.elapsed = Some(s.start.elapsed());
        }
    }
    fn on_warning(&self, name: &str, _idx: usize, msg: String) {
        if let Some(pb) = self.pb.lock().unwrap().as_ref() {
            pb.println(format!("[警告] {}: {}", name, msg));
        } else {
            println!("[警告] {}: {}", name, msg);
        }
    }
}

#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Console::{
    GetStdHandle, GetConsoleMode, SetConsoleMode, SetConsoleOutputCP, GetConsoleScreenBufferInfo,
    SetConsoleScreenBufferSize, SetConsoleWindowInfo, STD_OUTPUT_HANDLE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    CONSOLE_SCREEN_BUFFER_INFO, SMALL_RECT, COORD,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::HANDLE;

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
        if windows_sys::Win32::System::Console::GetConsoleMode(console_handle, &mut mode) != 0 {
            let _ = SetConsoleMode(console_handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }

        SetConsoleOutputCP(65001);

        let (need_cols, need_rows) = compute_required_console_size();
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(console_handle, &mut info) == 0 {
            return;
        }
        let cur_cols = (info.srWindow.Right - info.srWindow.Left + 1) as i16;
        let cur_rows = (info.srWindow.Bottom - info.srWindow.Top + 1) as i16;
        let cur_buf_cols = info.dwSize.X;
        let cur_buf_rows = info.dwSize.Y;
        let target_cols = (need_cols.min(160)) as i16;
        let target_rows = (need_rows.min(60)) as i16;
        let mut rect = SMALL_RECT { Left: 0, Top: 0, Right: target_cols - 1, Bottom: target_rows - 1 };
        if target_cols > cur_buf_cols || target_rows > cur_buf_rows {
            let buf = COORD { X: target_cols.max(cur_buf_cols), Y: target_rows.max(cur_buf_rows) };
            let _ = SetConsoleScreenBufferSize(console_handle, buf);
        }
        if target_cols > cur_cols || target_rows > cur_rows {
            let _ = SetConsoleWindowInfo(console_handle, 1, &mut rect as *mut _);
        }
        let mut info2: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(console_handle, &mut info2) == 0 {
            return;
        }
        let cur_cols2 = (info2.srWindow.Right - info2.srWindow.Left + 1) as i16;
        let cur_rows2 = (info2.srWindow.Bottom - info2.srWindow.Top + 1) as i16;
        if (target_cols < cur_cols2 || target_rows < cur_rows2)
            && target_cols <= info2.dwSize.X
            && target_rows <= info2.dwSize.Y
        {
            rect = SMALL_RECT { Left: 0, Top: 0, Right: target_cols - 1, Bottom: target_rows - 1 };
            let _ = SetConsoleWindowInfo(console_handle, 1, &mut rect as *mut _);
        }
    }
}

#[cfg(target_os = "windows")]
fn compute_required_console_size() -> (i32, i32) {
    use rua_core::constants::*;
    let mut maxw = 100usize;
    for s in WARNING_TEXTS {
        maxw = maxw.max(s.chars().count() + 6);
    }
    for s in INFO_TEXTS {
        maxw = maxw.max(s.chars().count() + 4);
    }
    for (_id, desc) in MENU_OPTIONS {
        let w = 4 + desc.chars().count();
        maxw = maxw.max(w);
    }
    let cols = (maxw as i32).clamp(100, 200);
    let rows = (MENU_OPTIONS.len() as i32 + 22).clamp(30, 80);
    (cols, rows)
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
        "21" => activate_adb_menu().await,
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
                    ui::step("正在检测 Fastboot 设备...");
                    let serial = match FastbootClient::new() {
                        Ok(client) => {
                            let s = select_device(&client).await;
                            if s.is_empty() {
                                ui::warn("未选择设备，取消刷机。");
                                return;
                            }
                            s
                        }
                        Err(e) => {
                            ui::err(&format!("初始化 Fastboot 客户端失败: {:?}", e));
                            return;
                        }
                    };
                    ui::step(&format!("已选择设备: {}", serial));

                    ui::step(&format!("正在启动 {} ...", selected_bat));
                    // 使用 start "" /wait "<bat>" -s <serial>，把序列号透传给脚本中的 fastboot %*
                    let _ = tokio::process::Command::new("cmd")
                        .arg("/c")
                        .arg("start")
                        .arg("")
                        .arg("/wait")
                        .arg(&bat_path)
                        .arg("-s")
                        .arg(&serial)
                        .spawn();
                    ui::ok("刷机脚本已启动，并已指定目标设备序列号。");
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
        if output_dir.exists() {
            let msg = format!("检测到上次解包目录已存在: {}\n是否删除后重新解包？ [Y/n]", output_dir.display());
            if ui::confirm(&msg, true) {
                if let Err(e) = fs::remove_dir_all(&output_dir) {
                    ui::err(&format!("删除旧目录失败: {:?}", e));
                    return;
                }
            } else {
                ui::warn("已取消解包操作。");
                return;
            }
        }
        if let Err(e) = fs::create_dir_all(&output_dir) {
            ui::err(&format!("创建输出目录失败: {:?}", e));
            return;
        }
        ui::step(&format!("正在处理 Payload 到 {} ...", output_dir.display()));

        let reporter = Arc::new(ConsoleReporter::new());
        let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
        if let Err(e) = payload::unpack_payload(&path, &output_dir, reporter_dyn).await {
            if INTERRUPTED.load(Ordering::SeqCst) {
                reporter.clear_current(">> 已取消解包");
                ui::warn("已取消解包操作。");
            } else {
                ui::err(&format!("处理失败: {:?}", e));
            }
        } else {
            ui::ok(&format!("处理完成！文件保存在: {}", output_dir.display()));
            reporter.print_summary();
            if let Ok(client) = FastbootClient::new() {
                let flasher = Flasher::new(client.clone());
                flash_select_partitions_in_dir(&flasher, &output_dir, false).await;
            } else {
                ui::err("无法初始化 Fastboot 客户端");
            }
        }
    }
}

async fn flash_all_partitions(flasher: &Flasher, fastboot_mode: bool) {
    let mode_str = if fastboot_mode { "Fastboot" } else { "FastbootD" };
    ui::step(&format!("正在目录下查找分区镜像刷入 ({})...", mode_str));
    if let Some(dir) = ui::select_directory("请选择包含分区镜像 (.img) 的目录") {
        let mut entries: Vec<_> = fs::read_dir(&dir).unwrap().flatten()
            .filter(|e| e.path().is_file() && e.path().extension().map_or(false, |ext| ext == "img"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        let parts: Vec<(String, String)> = entries.iter().map(|e| {
            let p = e.path();
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            (name, p.to_string_lossy().to_string())
        }).collect();
        if parts.is_empty() {
            ui::warn("目录下未发现任何 .img 文件");
            return;
        }
        println!("\n待刷入分区列表:");
        let divider = "=".repeat(60).white();
        println!("{}", divider);
        for (i, (n, _)) in parts.iter().enumerate() {
            println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), n);
        }
        println!("{}", divider);
        if !ui::confirm("确认开始刷入吗？", false) { ui::warn("已取消刷入。"); return; }
        let target_device = select_device(&flasher.client).await;
        if target_device.is_empty() {
            ui::warn("未选择设备，取消刷入。");
            return;
        }
        print!("输入要跳过的分区名，逗号分隔，直接回车全部刷入: ");
        let _ = io::stdout().flush();
        let mut skip_line = String::new();
        let _ = io::stdin().read_line(&mut skip_line);
        let skip_set: HashSet<String> = skip_line
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        for (name, path) in parts {
            if skip_set.contains(&name.to_lowercase()) {
                ui::warn(&format!("跳过 {}", name));
                continue;
            }
            ui::step(&format!("正在刷入 {}: {} ...", name, path));
            if let Err(e) = flasher.flash_partition(&target_device, &name, &path).await {
                ui::err(&format!("✗ {} 刷入失败: {:?}", name, e));
            } else {
                ui::ok(&format!("✓ {} 刷入成功", name));
            }
        }
        ui::ok("刷入完成。");
    }
}

async fn flash_select_partitions_in_dir(flasher: &Flasher, dir: &Path, fastboot_mode: bool) {
    let mode_str = if fastboot_mode { "Fastboot" } else { "FastbootD" };
    ui::step(&format!("从目录选择分区刷入 ({}) ...", mode_str));
    let mut entries: Vec<_> = match fs::read_dir(dir) {
        Ok(rd) => rd.flatten()
            .filter(|e| e.path().is_file() && e.path().extension().map_or(false, |ext| ext == "img"))
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort_by_key(|e| e.file_name());
    let parts: Vec<(String, String)> = entries.iter().map(|e| {
        let p = e.path();
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        (name, p.to_string_lossy().to_string())
    }).collect();
    if parts.is_empty() {
        ui::warn("目录下未发现任何 .img 文件");
        return;
    }
    println!("\n解包得到的分区列表:");
    let divider = "=".repeat(60).white();
    println!("{}", divider);
    for (i, (n, _)) in parts.iter().enumerate() {
        println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), n);
    }
    println!("{}", divider);
    print!("请输入要刷入的分区序号或名称，逗号分隔，直接回车表示全部: ");
    let _ = io::stdout().flush();
    let mut sel = String::new();
    let _ = io::stdin().read_line(&mut sel);
    let sel = sel.trim();
    let selected: Vec<(String, String)> = if sel.is_empty() {
        parts.clone()
    } else {
        let tokens: Vec<String> = sel.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let mut picked = Vec::new();
        for t in tokens {
            if let Ok(idx) = t.parse::<usize>() {
                if idx >= 1 && idx <= parts.len() {
                    picked.push(parts[idx - 1].clone());
                }
            } else {
                if let Some(p) = parts.iter().find(|(n, _)| n.eq_ignore_ascii_case(&t)) {
                    picked.push(p.clone());
                }
            }
        }
        if picked.is_empty() { parts.clone() } else { picked }
    };
    if selected.is_empty() {
        ui::warn("未选择任何分区。");
        return;
    }
    println!("\n即将刷入以下分区:");
    println!("{}", divider);
    for (n, _) in &selected {
        println!("{}", n);
    }
    println!("{}", divider);
    if !ui::confirm("确认开始刷入吗？", true) { ui::warn("已取消刷入。"); return; }
    let target_device = select_device(&flasher.client).await;
    if target_device.is_empty() {
        ui::warn("未选择设备，取消刷入。");
        return;
    }
    for (name, path) in selected {
        ui::step(&format!("正在刷入 {}: {} ...", name, path));
        if let Err(e) = flasher.flash_partition(&target_device, &name, &path).await {
            ui::err(&format!("✗ {} 刷入失败: {:?}", name, e));
        } else {
            ui::ok(&format!("✓ {} 刷入成功", name));
        }
    }
    ui::ok("刷入完成。");
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
                println!("\n要使用的解锁方式？");
                println!("1. fastboot flashing unlock (通用命令)");
                println!("2. fastboot oem unlock (部分华为设备等)");
                println!("3. fastboot flash unlock (部分 Nexus 和其他机型)");
                print!("请选择 (1-3, 默认 1): ");
                let _ = io::stdout().flush();
                let mut m = String::new();
                let _ = io::stdin().read_line(&mut m);
                let method = m.trim();

                ui::step("正在尝试解锁 Bootloader...");
                match method {
                    "2" => {
                        if let Err(e) = client.run(&["oem", "unlock"]).await {
                            ui::err(&format!("指令执行失败: {:?}", e));
                        } else {
                            ui::ok("已发送解锁指令，请查看手机屏幕确认。");
                        }
                    }
                    "3" => {
                        if let Some(f) = ui::select_file("请选择 unlock 文件（可跳过）", &["bin","img","txt","dat"]) {
                            if let Err(e) = client.run(&["flash", "unlock", &f.to_string_lossy()]).await {
                                ui::err(&format!("指令执行失败: {:?}", e));
                            } else {
                                ui::ok("已发送解锁指令，请查看手机屏幕确认。");
                            }
                        } else {
                            if let Err(e) = client.run(&["flash", "unlock"]).await {
                                ui::err(&format!("指令执行失败: {:?}", e));
                            } else {
                                ui::ok("已发送解锁指令，请查看手机屏幕确认。");
                            }
                        }
                    }
                    _ => {
                        if let Err(e) = client.run(&["flashing", "unlock"]).await {
                            ui::err(&format!("指令执行失败: {:?}", e));
                        } else {
                            ui::ok("已发送解锁指令，请查看手机屏幕确认。");
                        }
                    }
                }
            }
        }
        "2" => {
            if ui::confirm("确定要回锁 Bootloader 吗？请确保系统为原厂且未修改！", false) {
                println!("\n要使用的回锁方式？");
                println!("1. fastboot flashing lock (通用命令)");
                println!("2. fastboot oem lock (部分设备)");
                println!("3. fastboot flash lock (部分机型)");
                print!("请选择 (1-3, 默认 1): ");
                let _ = io::stdout().flush();
                let mut m = String::new();
                let _ = io::stdin().read_line(&mut m);
                let method = m.trim();

                ui::step("正在尝试回锁 Bootloader...");
                match method {
                    "2" => {
                        if let Err(e) = client.run(&["oem", "lock"]).await {
                            ui::err(&format!("指令执行失败: {:?}", e));
                        } else {
                            ui::ok("已发送回锁指令，请查看手机屏幕确认。");
                        }
                    }
                    "3" => {
                        if let Some(f) = ui::select_file("请选择 lock 文件（可跳过）", &["bin","img","txt","dat"]) {
                            if let Err(e) = client.run(&["flash", "lock", &f.to_string_lossy()]).await {
                                ui::err(&format!("指令执行失败: {:?}", e));
                            } else {
                                ui::ok("已发送回锁指令，请查看手机屏幕确认。");
                            }
                        } else {
                            if let Err(e) = client.run(&["flash", "lock"]).await {
                                ui::err(&format!("指令执行失败: {:?}", e));
                            } else {
                                ui::ok("已发送回锁指令，请查看手机屏幕确认。");
                            }
                        }
                    }
                    _ => {
                        if let Err(e) = client.run(&["flashing", "lock"]).await {
                            ui::err(&format!("指令执行失败: {:?}", e));
                        } else {
                            ui::ok("已发送回锁指令，请查看手机屏幕确认。");
                        }
                    }
                }
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

            println!("\n{} {}", ">>".cyan().bold(), "请选择镜像来源:".bright_white());
            println!("{}", "=".repeat(60).white());
            println!("{} 本地镜像", "1)".bright_cyan());
            println!("{} 从 Payload/卡刷包 获取", "2)".bright_cyan());
            println!("{}", "=".repeat(60).white());
            print!("请选择 [1/2]: ");
            let _ = io::stdout().flush();
            let mut src_choice = String::new();
            let _ = io::stdin().read_line(&mut src_choice);
            let src_choice = src_choice.trim();

            let boot_path: PathBuf = if src_choice == "2" {
                ui::step(&format!("正在从 Payload 提取 {} 分区镜像...", partition));
                let Some(payload_path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) else {
                    return;
                };
                let out_dir = Path::new("extracted_payload");
                let _ = fs::create_dir_all(out_dir);
                let reporter = Arc::new(ConsoleReporter::new());
                let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
                match rua_core::payload::extract_single_partition(&payload_path, &partition, out_dir, reporter_dyn).await {
                    Ok(p) => {
                        reporter.print_summary();
                        p
                    }
                    Err(e) => {
                        if INTERRUPTED.load(Ordering::SeqCst) {
                            reporter.clear_current(">> 已取消提取");
                            ui::warn("已取消操作。");
                        } else {
                            ui::err(&format!("从 Payload 提取分区失败: {:?}", e));
                        }
                        return;
                    }
                }
            } else {
                match ui::select_file("请选择要修补的 Boot 镜像", &["img"]) {
                    Some(p) => p,
                    None => return,
                }
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

                    let mut final_image_path = patched_path.clone();
                    print!("是否对修补后镜像进行 AVB 签名？[y/N]: ");
                    let _ = io::stdout().flush();
                    let mut sign_ans = String::new();
                    let _ = io::stdin().read_line(&mut sign_ans);
                    let sign_ans = sign_ans.trim().to_lowercase();
                    if sign_ans == "y" || sign_ans == "yes" {
                        match select_avb_key_dir_and_file(exe_dir) {
                            Some((_key_dir, key_path)) => {
                                ui::step(&format!("将使用密钥: {}", key_path.display()));
                                match try_sign_with_external_tools(&flasher.client, None, &final_image_path, &partition, &key_path).await {
                                    Ok(signed_path) => {
                                        ui::ok(&format!("签名成功: {}", signed_path));
                                        final_image_path = signed_path;
                                    }
                                    Err(e) => {
                                        ui::warn(&format!("签名失败或未找到可用工具: {}", e));
                                    }
                                }
                            }
                            None => {
                                ui::warn(&format!("未在 {} 下找到可用密钥或用户取消，跳过签名。", key_dir_fallback(exe_dir).display()));
                            }
                        }
                    }

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
                    match flasher.flash_partition(&target_device, &partition, &final_image_path).await {
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

            // 与分支逻辑保持一致：支持从本地或 Payload/卡刷包中获取镜像
            println!("\n{} {}", ">>".cyan().bold(), "请选择镜像来源:".bright_white());
            println!("{}", "=".repeat(60).white());
            println!("{} 本地镜像", "1)".bright_cyan());
            println!("{} 从 Payload/卡刷包 获取", "2)".bright_cyan());
            println!("{}", "=".repeat(60).white());
            print!("请选择 [1/2]: ");
            let _ = io::stdout().flush();
            let mut src_choice = String::new();
            let _ = io::stdin().read_line(&mut src_choice);
            let src_choice = src_choice.trim();

            let boot_path: PathBuf = if src_choice == "2" {
                ui::step(&format!("正在从 Payload 提取 {} 分区镜像...", partition));
                let Some(payload_path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) else {
                    return;
                };
                let out_dir = Path::new("extracted_payload");
                let _ = fs::create_dir_all(out_dir);
                let reporter = Arc::new(ConsoleReporter::new());
                let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
                match rua_core::payload::extract_single_partition(&payload_path, &partition, out_dir, reporter_dyn).await {
                    Ok(p) => { reporter.print_summary(); p },
                    Err(e) => {
                        if INTERRUPTED.load(Ordering::SeqCst) {
                            reporter.clear_current(">> 已取消提取");
                            ui::warn("已取消操作。");
                        } else {
                            ui::err(&format!("从 Payload 提取分区失败: {:?}", e));
                        }
                        return;
                    }
                }
            } else {
                match ui::select_file("请选择要修补的 Boot 镜像", &["img"]) {
                    Some(p) => p,
                    None => return,
                }
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

                    let mut final_image_path = patched_path.clone();
                    print!("是否对修补后镜像进行 AVB 签名？[y/N]: ");
                    let _ = io::stdout().flush();
                    let mut sign_ans = String::new();
                    let _ = io::stdin().read_line(&mut sign_ans);
                    let sign_ans = sign_ans.trim().to_lowercase();
                    if sign_ans == "y" || sign_ans == "yes" {
                        match select_avb_key_dir_and_file(exe_dir) {
                            Some((_dir, key_path)) => {
                                ui::step(&format!("将使用密钥: {}", key_path.display()));
                                match try_sign_with_external_tools(&flasher.client, None, &final_image_path, &partition, &key_path).await {
                                    Ok(signed_path) => {
                                        ui::ok(&format!("签名成功: {}", signed_path));
                                        final_image_path = signed_path;
                                    }
                                    Err(e) => ui::warn(&format!("签名失败或未找到可用工具: {}", e)),
                                }
                            }
                            None => ui::warn(&format!("未在 {} 下找到可用密钥或用户取消，跳过签名。", key_dir_fallback(exe_dir).display())),
                        }
                    }

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
                    match flasher.flash_partition(&target_device, &partition, &final_image_path).await {
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
    
    let maybe_path: Option<PathBuf> = if is_raw_kernel {
        let prompt = "请选择原始 Kernel 镜像";
        ui::select_file(prompt, &["img"])
    } else {
        println!("\n{} {}", ">>".cyan().bold(), "请选择镜像来源:".bright_white());
        println!("{}", "=".repeat(60).white());
        println!("{} 本地镜像", "1)".bright_cyan());
        println!("{} 从 Payload/卡刷包 获取", "2)".bright_cyan());
        println!("{}", "=".repeat(60).white());
        print!("请选择 [1/2]: ");
        let _ = io::stdout().flush();
        let mut src_choice = String::new();
        let _ = io::stdin().read_line(&mut src_choice);
        let src_choice = src_choice.trim();

        if src_choice == "2" {
            ui::step(&format!("正在从 Payload 提取 {} 分区镜像...", target_partition));
            let Some(payload_path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) else {
                return;
            };
            let out_dir = Path::new("extracted_payload");
            let _ = fs::create_dir_all(out_dir);
            let reporter = Arc::new(ConsoleReporter::new());
            let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
            match rua_core::payload::extract_single_partition(&payload_path, target_partition, out_dir, reporter_dyn).await {
                Ok(p) => { reporter.print_summary(); Some(p) },
                Err(e) => {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        reporter.clear_current(">> 已取消提取");
                        ui::warn("已取消操作。");
                    } else {
                        ui::err(&format!("从 Payload 提取分区失败: {:?}", e));
                    }
                    None
                }
            }
        } else {
            let prompt = "请选择要修补的 Boot 镜像";
            ui::select_file(prompt, &["img"])
        }
    };

    if let Some(boot_path) = maybe_path {
        ui::step("正在使用 APatch 修补...");
        
        // 先修补，不自动刷入，以便后面询问
        match flasher.apatch_patch(&boot_path.to_string_lossy(), &skey, target_partition, is_raw_kernel, false).await {
             Ok(_) => {
                 ui::ok("APatch 修补成功！");
                 println!("您的 SuperKey 为: {}", skey);
                 
                 let exe_path = env::current_exe().unwrap_or(PathBuf::from("rua_flash_tool.exe"));
                 let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
                 let mut final_image_path = format!("apatch_patched_{}.img", target_partition);
                 print!("是否对修补后镜像进行 AVB 签名？[y/N]: ");
                 let _ = io::stdout().flush();
                 let mut sign_ans = String::new();
                 let _ = io::stdin().read_line(&mut sign_ans);
                 let sign_ans = sign_ans.trim().to_lowercase();
                 if sign_ans == "y" || sign_ans == "yes" {
                     match select_avb_key_dir_and_file(exe_dir) {
                         Some((_key_dir, key_path)) => {
                             ui::step(&format!("将使用密钥: {}", key_path.display()));
                             match try_sign_with_external_tools(&flasher.client, None, &final_image_path, target_partition, &key_path).await {
                                 Ok(signed_path) => {
                                     ui::ok(&format!("签名成功: {}", signed_path));
                                     final_image_path = signed_path;
                                 }
                                 Err(e) => ui::warn(&format!("签名失败或未找到可用工具: {}", e)),
                             }
                         }
                         None => ui::warn(&format!("未在 {} 下找到可用密钥或用户取消，跳过签名。", key_dir_fallback(exe_dir).display())),
                     }
                 }

                 print!("是否立即刷入到 {} 分区? [Y/n]: ", target_partition);
                  let _ = io::stdout().flush();
                  let mut confirm = String::new();
                  let _ = io::stdin().read_line(&mut confirm);
                  let confirm = confirm.trim().to_lowercase();
                  if confirm.is_empty() || confirm == "y" {
                      ui::step(&format!("正在刷入到 {} 分区...", target_partition));
                      match flasher.client.run(&["flash", target_partition, &final_image_path]).await {
                          Ok(true) => {
                              ui::ok("刷入成功！");
                              println!("刷写完毕！请牢记您的 SuperKey: {}", skey);
                              let _ = std::fs::remove_file(&final_image_path);
                          }
                          _ => ui::err("刷入失败，请检查 fastboot 连接"),
                      }
                  } else {
                      println!("已取消刷入，修补镜像已保存为: {}", final_image_path);
                  }
             }
            Err(e) => ui::err(&format!("APatch 修补失败: {:?}", e)),
        }
    }
}

fn key_dir_fallback(exe_dir: &Path) -> PathBuf {
    // 多候选路径，兼容 cargo run 情况（项目根目录）
    let mut candidates = Vec::new();
    candidates.push(exe_dir.join("avbkey"));
    candidates.push(exe_dir.join("AVBKEY"));
    candidates.push(exe_dir.join("..").join("..").join("avbkey"));
    candidates.push(exe_dir.join("..").join("..").join("AVBKEY"));
    for p in candidates {
        if p.exists() && p.is_dir() {
            return p;
        }
    }
    exe_dir.join("avbkey")
}

fn select_avb_key_dir_and_file(exe_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let guess_dir = key_dir_fallback(exe_dir);
    let key_dir = if guess_dir.exists() && guess_dir.is_dir() {
        guess_dir
    } else {
        println!("{}", "未在程序目录下找到 avbkey 文件夹，请手动选择密钥目录".cyan());
        ui::select_directory("请选择存放 AVB 密钥 (.pem) 的目录")?
    };

    let mut pem_all: Vec<PathBuf> = std::fs::read_dir(&key_dir).ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map_or(false, |e| e.eq_ignore_ascii_case("pem")))
        .collect();
    pem_all.sort();

    if pem_all.is_empty() {
        ui::err("该目录下未找到任何 .pem 文件。");
        return None;
    }

    let pem_files = pem_all;

    println!("\n{} {}", ">>".cyan().bold(), "检测到以下可用密钥:".bright_white());
    let divider = "=".repeat(60).white();
    println!("{}", divider);
    for (i, p) in pem_files.iter().enumerate() {
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("<unknown>");
        let mut line = name.to_string();
        if name.to_lowercase().contains("pub") {
            line.push_str("  (公钥，一般不可用)");
        }
        println!("{}{}", format!("{:>3}. ", i + 1).bright_cyan(), line);
    }
    println!("{}", divider);
    print!("请选择密钥: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
    let idx: usize = input.trim().parse().unwrap_or(0);
    if idx == 0 || idx > pem_files.len() {
        ui::err("无效的选择。");
        return None;
    }
    let picked = pem_files[idx - 1].clone();
    let picked_name = picked.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if picked_name.to_lowercase().contains("pub") {
        ui::err("选择的是公钥文件，无法用于签名。请使用私钥 .pem。");
        return None;
    }
    Some((key_dir, picked))
}

async fn try_sign_with_external_tools(
    _base_client: &FastbootClient,
    _serial: Option<&str>,
    image_path: &str,
    partition: &str,
    key_path: &Path,
) -> anyhow::Result<String> {
    println!("{}", ">> 开始 AVB 签名流程".cyan());

    let img_len = std::fs::metadata(image_path).map(|m| m.len()).unwrap_or(0);
    let mib = 1024u64 * 1024u64;
    // 兜底：为 vbmeta+footer 预留余量（至少 2 MiB），再按 MiB 向上取整
    let min_slack = 2 * mib;
    let required = img_len.saturating_add(min_slack);
    let part_size_bytes = ((required + mib - 1) / mib) * mib;
    println!("{}", format!(">> 分区大小(兜底，含余量): {} bytes", part_size_bytes).yellow());

    let algo = if key_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|n| n.to_lowercase().contains("rsa4096"))
        .unwrap_or(false)
    {
        "SHA256_RSA4096"
    } else {
        "SHA256_RSA2048"
    };

    let signed = rua_core::avb::add_hash_footer(
        image_path,
        partition,
        part_size_bytes,
        &key_path.to_string_lossy(),
        algo,
    )
    .await
    .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;

    Ok(signed)
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

    // 3. 先选择要修补的分区
    let partition = select_partition();
    if partition.is_empty() { return; }

    // 4. 选择镜像来源（ramdisk 情况不提供 Payload 选项）
    let mut payload_origin: Option<PathBuf> = None;
    let img_path: PathBuf = if partition.eq_ignore_ascii_case("ramdisk") {
        match ui::select_file("请选择要修补的镜像", &["img"]) {
            Some(p) => p,
            None => return,
        }
    } else {
        println!("\n{} {}", ">>".cyan().bold(), "请选择镜像来源:".bright_white());
        println!("{}", "=".repeat(60).white());
        println!("{} 本地镜像", "1)".bright_cyan());
        println!("{} 从 Payload/卡刷包 获取", "2)".bright_cyan());
        println!("{}", "=".repeat(60).white());
        print!("请选择 [1/2]: ");
        let _ = io::stdout().flush();
        let mut src_choice = String::new();
        let _ = io::stdin().read_line(&mut src_choice);
        let src_choice = src_choice.trim();

        if src_choice == "2" {
            ui::step(&format!("正在从 Payload 提取 {} 分区镜像...", partition));
            let Some(payload_path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) else {
                return;
            };
            payload_origin = Some(payload_path.clone());
            let out_dir = Path::new("extracted_payload");
            let _ = fs::create_dir_all(out_dir);
            let reporter = Arc::new(ConsoleReporter::new());
            let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
            match rua_core::payload::extract_single_partition(&payload_path, &partition, out_dir, reporter_dyn).await {
                Ok(p) => { reporter.print_summary(); p },
                Err(e) => {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        reporter.clear_current(">> 已取消提取");
                        ui::warn("已取消操作。");
                    } else {
                        ui::err(&format!("从 Payload 提取分区失败: {:?}", e));
                    }
                    return;
                }
            }
        } else {
            match ui::select_file("请选择要修补的镜像", &["img"]) {
                Some(p) => p,
                None => return,
            }
        }
    };

    // 5. 自动识别 KMI（分区差异化逻辑）
    let mut detected_kmi: Option<String> = None;
    if partition.eq_ignore_ascii_case("ramdisk") {
        ui::warn("ramdisk 分区不支持自动检测 KMI，已跳过。");
    } else if partition.eq_ignore_ascii_case("boot") {
        ui::step("正在读取内核版本并判断 KMI...");
        match Flasher::read_kernel_version_and_kmi_from_boot_img(&img_path.to_string_lossy()) {
            Ok((kmi_opt, full_opt)) => {
                if let Some(full) = full_opt {
                    println!("- 内核版本字符串: {}", full);
                }
                if let Some(kmi) = kmi_opt {
                    ui::ok(&format!("检测到 KMI: {}", kmi));
                    detected_kmi = Some(kmi);
                } else {
                    ui::warn("无法根据内核版本字符串判断 KMI。");
                }
            }
            Err(e) => ui::warn(&format!("读取内核版本失败: {:?}", e)),
        }
    } else if partition.eq_ignore_ascii_case("init_boot") {
        if let Some(payload_path) = payload_origin.clone() {
            ui::step("正在额外提取 boot 分区用于 KMI 检测...");
            let out_dir = Path::new("extracted_payload");
            let _ = fs::create_dir_all(out_dir);
            let reporter = Arc::new(ConsoleReporter::new());
            let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
            match rua_core::payload::extract_single_partition(&payload_path, "boot", out_dir, reporter_dyn).await {
                Ok(boot_img) => {
                    reporter.print_summary();
                    match Flasher::read_kernel_version_and_kmi_from_boot_img(&boot_img.to_string_lossy()) {
                        Ok((kmi_opt, full_opt)) => {
                            if let Some(full) = full_opt {
                                println!("- 内核版本字符串: {}", full);
                            }
                            if let Some(kmi) = kmi_opt {
                                ui::ok(&format!("检测到 KMI: {}", kmi));
                                detected_kmi = Some(kmi);
                            } else {
                                ui::warn("无法根据内核版本字符串判断 KMI。");
                            }
                        }
                        Err(e) => ui::warn(&format!("读取内核版本失败: {:?}", e)),
                    }
                }
                Err(e) => {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        reporter.clear_current(">> 已取消提取");
                        ui::warn("已取消 KMI 检测。");
                    } else {
                        ui::warn(&format!("提取 boot 用于 KMI 检测失败: {:?}", e));
                    }
                }
            }
        } else {
            ui::warn("init_boot 来源为本地镜像，无法自动提取 boot 进行 KMI 检测。");
        }
    }

    // 6. 选择 KMI (.ko 文件)
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

    // 7. 执行修补
    ui::step("正在使用 KernelSU LKM 修补...");
    match flasher.kernelsu_lkm_patch(
        &img_path.to_string_lossy(),
        &selected_ver.ksuinit_path.to_string_lossy(),
        Some(&selected_ver.ksuinit_d_path.to_string_lossy()),
        &selected_ko.ko_path.to_string_lossy(),
        &partition,
        false
    ).await {
        Ok(out_name) => {
            ui::ok("KernelSU LKM 修补成功！");
            println!("\n{}", "=".repeat(60).white());
            println!("{}", "📱 KernelSU LKM 刷入确认".bright_white().bold());
            println!("{}", "=".repeat(60).white());
            println!("{}", format!("  📦 分支: {}", selected_branch.name).cyan());
            println!("{}", format!("  🔢 版本: {}", selected_ver.version_name).cyan());
            if let Some(kmi) = detected_kmi.as_ref() {
                println!("{}", format!("  🔧 检测到 KMI: {}", kmi).cyan());
            }
            println!("{}", format!("  💾 目标分区: {}", partition).cyan());
            println!("{}", format!("  📝 修补后镜像: {}", out_name).cyan());
            println!("{}", "=".repeat(60).white());

            let mut final_image_path = out_name.clone();
            print!("是否对修补后镜像进行 AVB 签名？[y/N]: ");
            let _ = io::stdout().flush();
            let mut sign_ans = String::new();
            let _ = io::stdin().read_line(&mut sign_ans);
            let sign_ans = sign_ans.trim().to_lowercase();
            if sign_ans == "y" || sign_ans == "yes" {
                match select_avb_key_dir_and_file(exe_dir) {
                    Some((_key_dir, key_path)) => {
                        ui::step(&format!("将使用密钥: {}", key_path.display()));
                        match try_sign_with_external_tools(&flasher.client, None, &final_image_path, &partition, &key_path).await {
                            Ok(signed_path) => {
                                ui::ok(&format!("签名成功: {}", signed_path));
                                final_image_path = signed_path;
                            }
                            Err(e) => ui::warn(&format!("签名失败或未找到可用工具: {}", e)),
                        }
                    }
                    None => ui::warn(&format!("未在 {} 下找到可用密钥或用户取消，跳过签名。", key_dir_fallback(exe_dir).display())),
                }
            }

            if ui::confirm("确定要继续刷入吗？", true) {
                let target_device = select_device(&flasher.client).await;
                if target_device.is_empty() {
                    ui::warn("未检测到设备，无法刷入。修补镜像已保存。");
                    return;
                }
                ui::step(&format!("正在刷入 {} 分区...", partition));
                match flasher.flash_partition(&target_device, &partition, &final_image_path).await {
                    Ok(_) => {
                        ui::ok("刷入成功！");
                        let _ = std::fs::remove_file(&final_image_path);
                    }
                    Err(e) => ui::err(&format!("刷入失败: {:?}", e)),
                }
            } else {
                println!("已取消刷入，修补镜像已保存为: {}", final_image_path);
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
        let maybe_boot: Option<PathBuf> = if is_raw_kernel {
            let prompt = "请选择原始 Kernel 镜像";
            ui::select_file(prompt, &["img"])
        } else {
            println!("\n{} {}", ">>".cyan().bold(), "请选择镜像来源:".bright_white());
            println!("{}", "=".repeat(60).white());
            println!("{} 本地镜像", "1)".bright_cyan());
            println!("{} 从 Payload/卡刷包 获取", "2)".bright_cyan());
            println!("{}", "=".repeat(60).white());
            print!("请选择 [1/2]: ");
            let _ = io::stdout().flush();
            let mut src_choice = String::new();
            let _ = io::stdin().read_line(&mut src_choice);
            let src_choice = src_choice.trim();

            if src_choice == "2" {
                ui::step(&format!("正在从 Payload 提取 {} 分区镜像...", target_partition));
                let Some(payload_path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) else {
                    return;
                };
                let out_dir = Path::new("extracted_payload");
                let _ = fs::create_dir_all(out_dir);
                let reporter = Arc::new(ConsoleReporter::new());
                let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
                match rua_core::payload::extract_single_partition(&payload_path, target_partition, out_dir, reporter_dyn).await {
                    Ok(p) => { reporter.print_summary(); Some(p) },
                    Err(e) => {
                        if INTERRUPTED.load(Ordering::SeqCst) {
                            reporter.clear_current(">> 已取消提取");
                            ui::warn("已取消操作。");
                        } else {
                            ui::err(&format!("从 Payload 提取分区失败: {:?}", e));
                        }
                        None
                    }
                }
            } else {
                let prompt = "请选择原始 Boot 镜像";
                ui::select_file(prompt, &["img"])
            }
        };

        if let Some(boot_path) = maybe_boot {
            ui::step("正在解压 AnyKernel3 并修补内核...");
            match flasher.anykernel3_root(&zip_path.to_string_lossy(), &boot_path.to_string_lossy(), target_partition, is_raw_kernel, false).await {
                Ok(out_name) => {
                    ui::ok("内核修补成功！");
                    let exe_path = env::current_exe().unwrap_or(PathBuf::from("rua_flash_tool.exe"));
                    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
                    let mut final_image_path = out_name.clone();
                    print!("是否对修补后镜像进行 AVB 签名？[y/N]: ");
                    let _ = io::stdout().flush();
                    let mut sign_ans = String::new();
                    let _ = io::stdin().read_line(&mut sign_ans);
                    let sign_ans = sign_ans.trim().to_lowercase();
                    if sign_ans == "y" || sign_ans == "yes" {
                        match select_avb_key_dir_and_file(exe_dir) {
                            Some((_key_dir, key_path)) => {
                                ui::step(&format!("将使用密钥: {}", key_path.display()));
                                match try_sign_with_external_tools(&flasher.client, None, &final_image_path, target_partition, &key_path).await {
                                    Ok(signed_path) => {
                                        ui::ok(&format!("签名成功: {}", signed_path));
                                        final_image_path = signed_path;
                                    }
                                    Err(e) => ui::warn(&format!("签名失败或未找到可用工具: {}", e)),
                                }
                            }
                            None => ui::warn(&format!("未在 {} 下找到可用密钥或用户取消，跳过签名。", key_dir_fallback(exe_dir).display())),
                        }
                    }

                    print!("是否立即刷入到 {} 分区? [Y/n]: ", target_partition);
                    let _ = io::stdout().flush();
                    let mut confirm = String::new();
                    let _ = io::stdin().read_line(&mut confirm);
                    let confirm = confirm.trim().to_lowercase();
                    if confirm.is_empty() || confirm == "y" {
                        let target_device = select_device(&flasher.client).await;
                        if target_device.is_empty() {
                            ui::warn("未检测到设备，无法刷入。修补镜像已保存。");
                            return;
                        }
                        ui::step(&format!("正在刷入到 {} 分区...", target_partition));
                        match flasher.flash_partition(&target_device, target_partition, &final_image_path).await {
                            Ok(_) => {
                                ui::ok("刷入成功！");
                                let _ = std::fs::remove_file(&final_image_path);
                            }
                            Err(_) => ui::err("刷入失败，请检查 fastboot 连接"),
                        }
                    } else {
                        println!("已取消刷入，修补镜像已保存为: {}", final_image_path);
                    }
                }
                Err(e) => ui::err(&format!("AnyKernel3 修补失败: {:?}", e)),
            }
        }
    }
    pause_before_back();
}

async fn flash_custom_partition(flasher: &Flasher) {
    if !ui::confirm("确定要继续吗？此操作将刷入自定义分区镜像。", true) { return; }

    print!("请输入分区名 (如 boot/init_boot/recovery/system/vendor): ");
    let _ = io::stdout().flush();
    let mut partition = String::new();
    let _ = io::stdin().read_line(&mut partition);
    let partition = partition.trim().to_string();
    if partition.is_empty() { ui::err("分区名不能为空。"); return; }

    println!("\n{} {}", ">>".cyan().bold(), "请选择镜像来源:".bright_white());
    println!("{}", "=".repeat(60).white());
    println!("{} 本地镜像", "1)".bright_cyan());
    println!("{} 从 Payload/卡刷包 获取", "2)".bright_cyan());
    println!("{}", "=".repeat(60).white());
    print!("请选择 [1/2]: ");
    let _ = io::stdout().flush();
    let mut src_choice = String::new();
    let _ = io::stdin().read_line(&mut src_choice);
    let src_choice = src_choice.trim();

    let img_path: Option<PathBuf> = if src_choice == "2" {
        ui::step(&format!("正在从 Payload 提取 {} 分区镜像...", partition));
        let Some(payload_path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) else { return; };
        let out_dir = Path::new("extracted_payload");
        let _ = fs::create_dir_all(out_dir);
        let reporter = Arc::new(ConsoleReporter::new());
        let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
        match rua_core::payload::extract_single_partition(&payload_path, &partition, out_dir, reporter_dyn).await {
            Ok(p) => { reporter.print_summary(); Some(p) },
            Err(e) => {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    reporter.clear_current(">> 已取消提取");
                    ui::warn("已取消操作。");
                } else {
                    ui::err(&format!("从 Payload 提取分区失败: {:?}", e));
                }
                None
            }
        }
    } else {
        ui::select_file("请选择要刷入的自定义分区镜像", &["img"])
    };

    let Some(path) = img_path else { return; };
    let target_device = select_device(&flasher.client).await;
    if target_device.is_empty() {
        ui::warn("未检测到设备，取消刷入。");
        return;
    }
    ui::step(&format!("正在刷入 {}: {} ...", partition, path.display()));
    match flasher.flash_partition(&target_device, &partition, &path.to_string_lossy()).await {
        Ok(_) => ui::ok("刷入成功！"),
        Err(e) => ui::err(&format!("刷入失败: {:?}", e)),
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
    println!("\n{} {}", ">>".cyan().bold(), "请选择 vbmeta 镜像来源:".bright_white());
    println!("{}", "=".repeat(60).white());
    println!("{} 本地 vbmeta.img", "1)".bright_cyan());
    println!("{} 从 Payload/卡刷包 提取 vbmeta", "2)".bright_cyan());
    println!("{}", "=".repeat(60).white());
    print!("请选择 [1/2]: ");
    let _ = io::stdout().flush();
    let mut src_choice = String::new();
    let _ = io::stdin().read_line(&mut src_choice);
    let src_choice = src_choice.trim();

    let img_path: Option<PathBuf> = if src_choice == "2" {
        ui::step("正在从 Payload 提取 vbmeta 分区镜像...");
        let Some(payload_path) = ui::select_file("请选择 Payload.bin 或卡刷包 ZIP", &["bin", "zip"]) else { return; };
        let out_dir = Path::new("extracted_payload");
        let _ = fs::create_dir_all(out_dir);
        let reporter = Arc::new(ConsoleReporter::new());
        let reporter_dyn: Arc<dyn ProgressReporter> = reporter.clone();
        match rua_core::payload::extract_single_partition(&payload_path, "vbmeta", out_dir, reporter_dyn).await {
            Ok(p) => { reporter.print_summary(); Some(p) },
            Err(e) => {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    reporter.clear_current(">> 已取消提取");
                    ui::warn("已取消操作。");
                } else {
                    ui::err(&format!("从 Payload 提取 vbmeta 失败: {:?}", e));
                }
                None
            }
        }
    } else {
        ui::select_file("请选择 vbmeta.img", &["img"])
    };

    let Some(vbmeta_path) = img_path else { return; };

    let target_device = select_device(&flasher.client).await;
    if target_device.is_empty() {
        ui::err("未检测到 Fastboot 设备，无法执行刷入。");
        return;
    }

    ui::step("正在刷入 vbmeta.img 并关闭 AVB 校验...");
    match flasher.flash_vbmeta(&target_device, &vbmeta_path.to_string_lossy()).await {
        Ok(_) => ui::ok("vbmeta 刷入成功，AVB 校验已禁用。"),
        Err(e) => ui::err(&format!("vbmeta 刷入失败: {:?}", e)),
    }
}

fn open_cmd() {
    ui::step("正在打开新命令行窗口...");
    let exe_path = env::current_exe().unwrap_or(std::path::PathBuf::from("rua_flash_tool.exe"));
    let exe_dir = exe_path.parent().unwrap_or(std::path::Path::new("."));

    let mut platform_tools = crate::utils::path_resolver::resolve_subdir_dev_release("platform-tools")
        .unwrap_or_else(|| exe_dir.join("platform-tools"));
    if !(platform_tools.exists() && platform_tools.is_dir()) {
        if let Ok(mut cd) = env::current_dir() {
            cd.push("platform-tools");
            if cd.exists() && cd.is_dir() {
                platform_tools = cd;
            }
        }
    }

    // 启动新的 cmd 窗口并将工作目录设为 platform-tools（如果存在）
    let target_dir = if platform_tools.exists() && platform_tools.is_dir() {
        platform_tools.to_string_lossy().to_string()
    } else {
        exe_dir.to_string_lossy().to_string()
    };

    let _ = std::process::Command::new("cmd")
        .args(&["/C", "start", "", "/D", &target_dir, "cmd.exe"])
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
    if !ui::confirm("确定要恢复出厂设置吗？这将清除所有数据！", false) {
        pause_before_back();
        return;
    }

    println!("\n{} {}", ">>".cyan().bold(), "注意：部分机型（如 ColorOS、华为）直接擦除 userdata 可能缺少必要文件影响使用。".bright_white());
    println!("{}", "你可以在此指定“无用户数据”的 userdata.img 刷入，或继续直接擦除分区。".bright_black());
    println!("\n请选择操作:");
    println!("1. 直接擦除 userdata 分区（erase + format）");
    println!("2. 指定无用户数据的 userdata.img 刷入");
    print!("请输入选择 (1-2，默认 1): ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
    let choice = input.trim();

    ui::step("正在检测 Fastboot 设备...");
    let target_device = select_device(client).await;
    if target_device.is_empty() {
        ui::err("未检测到 Fastboot 设备，无法继续。");
        pause_before_back();
        return;
    }

    if choice == "2" {
        if let Some(img_path) = ui::select_file("请选择无用户数据的 userdata.img", &["img"]) {
            let flasher = Flasher::new(client.clone());
            ui::step(&format!("正在刷入 userdata: {} ...", img_path.display()));
            match flasher.flash_partition(&target_device, "userdata", &img_path.to_string_lossy()).await {
                Ok(_) => ui::ok("刷入完成。"),
                Err(e) => ui::err(&format!("刷入失败: {:?}", e)),
            }
        } else {
            ui::warn("未选择镜像文件，已取消。");
        }
    } else {
        ui::step("正在清除 Data 分区...");
        if let Err(e) = client.erase("userdata").await {
            ui::err(&format!("清除失败: {:?}", e));
        }
        ui::step("正在格式化 Data 分区...");
        if let Err(e) = client.format("userdata").await {
            ui::err(&format!("格式化失败: {:?}", e));
        }
    }
    ui::ok("恢复出厂设置操作完成。");
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

async fn activate_adb_menu() {
    let mut adb_devs = Vec::new();
    if let Ok(adb) = rua_core::AdbClient::new() {
        if let Ok(devs) = adb.list_devices().await {
            adb_devs = devs;
        }
    }
    if adb_devs.is_empty() {
        ui::err("未发现 ADB 模式的设备。");
        pause_before_back();
        return;
    }

    let dev = if adb_devs.len() == 1 {
        &adb_devs[0]
    } else {
        println!("\n{} 请选择目标设备:", ">>".cyan());
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

    println!("\n{} {}", ">>".cyan().bold(), "请选择需要激活的工具:".bright_white());
    println!("1. Shizuku");
    println!("2. 冰箱 (ADB 模式)");
    println!("3. 冰箱设为设备管理员 (Device Owner)");
    println!("4. 黑阈 (Brevent)");
    println!("5. AXManager");
    println!("6. 小黑屋 (web1n.stopapp)");
    println!("7. 小黑屋设为设备管理员");
    print!("请选择 (1-7): ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    let _ = io::stdin().read_line(&mut input);
    let opt = input.trim();

    if let Ok(adb) = rua_core::AdbClient::new() {
        match opt {
            "2" => {
                ui::step("正在激活 冰箱 (ADB 模式)...");
                match adb.activate_icebox_adb(&dev.serial).await {
                    Ok(out) => ui::ok(&format!("输出:\n{}", out)),
                    Err(e) => ui::err(&format!("激活失败: {:?}", e)),
                }
            }
            "3" => {
                ui::step("正在设置 冰箱 为设备管理员...");
                match adb.activate_icebox_admin(&dev.serial).await {
                    Ok(out) => ui::ok(&format!("输出:\n{}", out)),
                    Err(e) => ui::err(&format!("设置失败: {:?}", e)),
                }
            }
            "4" => {
                ui::step("正在激活 黑阈 (Brevent)...");
                match adb.activate_brevent(&dev.serial).await {
                    Ok(out) => ui::ok(&format!("输出:\n{}", out)),
                    Err(e) => ui::err(&format!("激活失败: {:?}", e)),
                }
            }
            "5" => {
                ui::step("正在激活 AXManager...");
                match adb.activate_axmanager(&dev.serial).await {
                    Ok(out) => ui::ok(&format!("输出:\n{}", out)),
                    Err(e) => ui::err(&format!("激活失败: {:?}", e)),
                }
            }
            "6" => {
                ui::step("正在激活 小黑屋...");
                match adb.activate_demon_mode(&dev.serial).await {
                    Ok(out) => ui::ok(&format!("输出:\n{}", out)),
                    Err(e) => ui::err(&format!("激活失败: {:?}", e)),
                }
            }
            "7" => {
                ui::step("正在将 小黑屋 设为设备管理员...");
                match adb.activate_demon_admin(&dev.serial).await {
                    Ok(out) => ui::ok(&format!("输出:\n{}", out)),
                    Err(e) => ui::err(&format!("设置失败: {:?}", e)),
                }
            }
            _ => {
                ui::step("正在激活 Shizuku...");
                match adb.activate_shizuku(&dev.serial).await {
                    Ok(out) => ui::ok(&format!("Shizuku 激活输出:\n{}", out)),
                    Err(e) => ui::err(&format!("激活失败: {:?}", e)),
                }
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
