use crate::fastboot::FastbootClient;
use crate::error::{FlashError, Result};
use crate::utils;
use std::path::{Path, PathBuf};
use std::fs::{self, File};
use std::io::{Read, Write, Cursor};
use android_bootimg::parser::BootImage;
use android_bootimg::patcher::BootImagePatchOption;
use zip::ZipArchive;
use sha1::{Sha1, Digest};
use flate2::write::GzEncoder;
use flate2::Compression;
use colored::Colorize;

fn detect_and_skip_cpio_header(data: &[u8]) -> usize {
    if data.len() < 10 {
        return 0;
    }

    // magiskboot 风格：扫描整个 ramdisk 寻找 CPIO 魔数 "070701"
    // 这样可以跳过任何厂商自定义的头部
    for offset in 0..data.len().saturating_sub(6) {
        if &data[offset..offset+6] == b"070701" {
            if offset > 0 {
                println!("[DEBUG] Found CPIO magic at offset {}, skipping vendor header", offset);
                println!("[DEBUG] Vendor header bytes: {:02x?}", &data[0..std::cmp::min(offset, 16)]);
            } else {
                println!("[DEBUG] Standard newc CPIO format detected at offset 0");
            }
            return offset;
        }
    }

    // 如果没找到 070701，尝试旧的 cpio 魔数 (070707) 或其他变体 (magiskboot 也会检查这些)
    for offset in 0..data.len().saturating_sub(6) {
        if &data[offset..offset+6] == b"070707" || &data[offset..offset+6] == b"070702" {
            println!("[DEBUG] Found legacy/alternative CPIO magic at offset {}", offset);
            return offset;
        }
    }

    println!("[DEBUG] No CPIO magic found in ramdisk data");
    0
}

pub struct Flasher {
    pub client: FastbootClient,
}

impl Flasher {
    pub fn new(client: FastbootClient) -> Self {
        Self { client }
    }

    pub async fn flash_boot(&self, path: &str) -> Result<()> {
        self.flash_partition("", "boot", path).await
    }

    pub async fn flash_vbmeta(&self, path: &str) -> Result<()> {
        if self.client.run(&["flash", "vbmeta", "--disable-verity", "--disable-verification", path]).await? {
            Ok(())
        } else {
            Err(FlashError::FastbootError("Failed to flash vbmeta".into()))
        }
    }

    pub async fn list_devices(&self) -> Result<Vec<super::ConnectedDevice>> {
        self.client.list_devices().await
    }

    pub async fn flash_raw_data(&self, partition: &str, data: &[u8]) -> Result<()> {
        let temp_name = format!("temp_{}.img", partition);
        fs::write(&temp_name, data)?;
        let res = self.client.run(&["flash", partition, &temp_name]).await;
        let _ = fs::remove_file(&temp_name);
        if res? {
            Ok(())
        } else {
            Err(FlashError::FastbootError(format!("Failed to flash raw data to {}", partition)))
        }
    }

    pub async fn disable_avb(&self, vbmeta_path: &str) -> Result<()> {
        if self.client.run(&["flash", "vbmeta", "--disable-verity", "--disable-verification", vbmeta_path]).await? {
            Ok(())
        } else {
            Err(FlashError::FastbootError("Failed to disable AVB".into()))
        }
    }

    pub fn detect_kmi_from_kernel(kernel_data: &[u8]) -> Option<String> {
        let printable_strings: Vec<&str> = kernel_data
            .split(|&b| b == 0)
            .filter_map(|slice| std::str::from_utf8(slice).ok())
            .filter(|s| s.chars().all(|c| c.is_ascii_graphic() || c == ' '))
            .collect();

        let re = regex::Regex::new(r"(?:.* )?(\d+\.\d+)(?:\S+)?(android\d+)").ok()?;
        for s in printable_strings {
            if let Some(caps) = re.captures(s)
                && let (Some(kernel_version), Some(android_version)) = (caps.get(1), caps.get(2))
            {
                return Some(format!("{}-{}", android_version.as_str(), kernel_version.as_str()));
            }
        }
        None
    }

    pub fn detect_kmi_from_boot_img(boot_img_path: &str) -> Result<Option<String>> {
        let mut boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
        let boot_img = BootImage::parse(&boot_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
        if let Some(kernel) = boot_img.get_blocks().get_kernel() {
            let kernel_data = kernel.get_data();
            return Ok(Self::detect_kmi_from_kernel(kernel_data));
        }
        Ok(None)
    }

    fn is_magisk_patched(entries: &[(String, u32, Vec<u8>)]) -> bool {
        entries.iter().any(|(name, _, _)| name == ".backup/.magisk")
    }

    fn is_kernelsu_patched(entries: &[(String, u32, Vec<u8>)]) -> bool {
        entries.iter().any(|(name, _, _)| name == "kernelsu.ko")
    }

    pub async fn kernelsu_lkm_install(
        &self,
        boot_img_path: &str,
        ksuinit_path: &str,
        ksuinit_d_dir: Option<&str>,
        ko_path: &str,
        target_partition: &str,
        force: bool
    ) -> Result<()> {
        let mut boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
        let boot_img = BootImage::parse(&boot_data).map_err(|e| FlashError::PatchError(e.to_string()))?;

        if let Some(kernel) = boot_img.get_blocks().get_kernel() {
            let kernel_data = kernel.get_data();
            if let Some(kmi) = Self::detect_kmi_from_kernel(kernel_data) {
                println!("- KMI: {}", kmi);
            }
        }

        let rd = boot_img.get_blocks().get_ramdisk().ok_or_else(|| FlashError::PatchError("no ramdisk".into()))?;
        let rd_raw = rd.get_data();
        let fmt = utils::detect_ramdisk_format(rd_raw);
        let rd_decomp = utils::decompress_ramdisk(rd_raw)?;

        let (mut entries, old_init_info) = if rd_decomp.is_empty() {
            (Vec::new(), None)
        } else {
            utils::cpio_load_with_threecpio(&rd_decomp)?
        };

        if Self::is_magisk_patched(&entries) {
            if force {
                println!("- 警告: 检测到 Magisk 已修补此镜像，将继续安装（可能导致冲突）");
            } else {
                return Err(FlashError::PatchError(
                    "检测到 Magisk 已修补此镜像，KernelSU 可能与 Magisk 冲突。\n如需强制安装，请使用 --force 参数。".to_string()
                ));
            }
        }

        if Self::is_kernelsu_patched(&entries) {
            println!("- 警告: 此镜像可能已由 KernelSU 修补");
        }

        entries.retain(|(name, _, _)| name != "init");
        
        if let Some((mode, old_data)) = old_init_info {
            entries.push(("init.real".to_string(), mode as u32, old_data));
        }
        
        let ksuinit_bytes = fs::read(ksuinit_path)?;
        entries.push(("init".to_string(), 0o755, ksuinit_bytes));
        
        let ko_bytes = fs::read(ko_path)?;
        entries.push(("kernelsu.ko".to_string(), 0o755, ko_bytes));
        
        if let Some(dir) = ksuinit_d_dir {
            let base = Path::new(dir);
            if base.exists() && base.is_dir() {
                for entry in fs::read_dir(base)? {
                    let entry = entry?;
                    let p = entry.path();
                    if p.is_file() {
                        if let Some(file_name) = p.file_name().and_then(|s| s.to_str()) {
                            let target = format!("ksuinit.d/{}", file_name);
                            let content = fs::read(&p)?;
                            entries.push((target, 0o755, content));
                        }
                    }
                }
            }
        }
        
        let new_cpio = utils::cpio_create_with_threecpio(&entries)?;
        let final_ramdisk = utils::compress_ramdisk(fmt, &new_cpio)?;
        let mut patcher = BootImagePatchOption::new(&boot_img);
        patcher.replace_ramdisk(Box::new(Cursor::new(final_ramdisk)), true);
        let mut output_data = Cursor::new(Vec::new());
        patcher.patch(&mut output_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
        
        let out_name = format!("ksu_lkm_patched_{}.img", target_partition);
        fs::write(&out_name, output_data.into_inner())?;
        
        let res = self.client.run(&["flash", target_partition, &out_name]).await;
        let _ = fs::remove_file(&out_name);
        
        if res? {
            Ok(())
        } else {
            Err(FlashError::FastbootError("Failed to flash patched KSU image".into()))
        }
    }

    pub async fn apatch_patch(&self, boot_img_path: &str, skey: &str, target_partition: &str, is_raw_kernel: bool, auto_flash: bool) -> Result<()> {
        let mut new_kernel_data;
        let mut was_compressed = false;

        if is_raw_kernel {
            // 如果是原始内核 (Huawei 等设备)
            let mut kernel_data = Vec::new();
            File::open(boot_img_path)?.read_to_end(&mut kernel_data)?;
            
            let mut raw_kernel = kernel_data.clone();
            if let Ok(decompressed) = utils::decompress_ramdisk(&kernel_data)
                && decompressed.len() != kernel_data.len() {
                    raw_kernel = decompressed;
                    was_compressed = true;
                }
            
            new_kernel_data = self.run_kptools(&raw_kernel, skey, target_partition).await?;
            
            if was_compressed {
                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&new_kernel_data)?;
                new_kernel_data = encoder.finish()?;
            }

            let out_name = format!("apatch_patched_{}.img", target_partition);
            fs::write(&out_name, new_kernel_data)?;

            if auto_flash {
                let res = self.client.run(&["flash", target_partition, &out_name]).await;
                let _ = fs::remove_file(&out_name);
                if res? {
                    Ok(())
                } else {
                    Err(FlashError::FastbootError("Failed to flash patched APatch image".into()))
                }
            } else {
                println!("[INFO] 修补完成，镜像已保存为: {}", out_name);
                Ok(())
            }
        } else {
            // 如果是标准 Boot 镜像
            let mut boot_data = Vec::new();
            File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
            let boot_img = BootImage::parse(&boot_data).map_err(|e| FlashError::PatchError(e.to_string()))?;

            let kernel_data = boot_img.get_blocks().get_kernel()
                .map(|k| k.get_data().to_vec())
                .unwrap_or_else(Vec::new);
            
            if kernel_data.is_empty() {
                return Err(FlashError::PatchError("未在镜像中找到内核数据".into()));
            }

            let mut raw_kernel = kernel_data.clone();
            if let Ok(decompressed) = utils::decompress_ramdisk(&kernel_data)
                && decompressed.len() != kernel_data.len() {
                    raw_kernel = decompressed;
                    was_compressed = true;
                }

            let patched_raw_kernel = self.run_kptools(&raw_kernel, skey, target_partition).await?;
            
            new_kernel_data = patched_raw_kernel;
            if was_compressed {
                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&new_kernel_data)?;
                new_kernel_data = encoder.finish()?;
            }

            let out_name = format!("apatch_patched_{}.img", target_partition);
            let mut patcher = BootImagePatchOption::new(&boot_img);
            patcher.replace_kernel(Box::new(Cursor::new(new_kernel_data)), false);
            let mut output_data = Cursor::new(Vec::new());
            patcher.patch(&mut output_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
            fs::write(&out_name, output_data.into_inner())?;

            if auto_flash {
                let res = self.client.run(&["flash", target_partition, &out_name]).await;
                let _ = fs::remove_file(&out_name);
                if res? {
                    Ok(())
                } else {
                    Err(FlashError::FastbootError("Failed to flash patched APatch image".into()))
                }
            } else {
                println!("[INFO] 修补完成，镜像已保存为: {}", out_name);
                Ok(())
            }
        }
    }

    async fn run_kptools(&self, raw_kernel: &[u8], skey: &str, target_partition: &str) -> Result<Vec<u8>> {
        let temp_kernel = format!("temp_kernel_{}", target_partition);
        let patched_kernel = format!("temp_kernel_patched_{}", target_partition);
        fs::write(&temp_kernel, raw_kernel)?;

        let kptools = if cfg!(target_os = "windows") { "KernelPatch/kptools-msys2.exe" } else { "KernelPatch/kptools-linux-x86_64" };
        let kpimg = "KernelPatch/kpimg-android";
        
        if !Path::new(kptools).exists() || !Path::new(kpimg).exists() {
             let _ = fs::remove_file(&temp_kernel);
             return Err(FlashError::PatchError("找不到 KernelPatch 工具或 kpimg".into()));
        }

        println!("[INFO] 正在运行 KernelPatch...");
        let output = tokio::process::Command::new(kptools)
            .args(["-p", "--image", &temp_kernel, "--skey", skey, "--kpimg", kpimg, "--out", &patched_kernel])
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stdout.is_empty() {
            for line in stdout.lines() {
                println!("[KernelPatch] {}", line);
            }
        }
        if !stderr.is_empty() {
            for line in stderr.lines() {
                println!("[KernelPatch] {}", line);
            }
        }

        if !output.status.success() {
            let _ = fs::remove_file(&temp_kernel);
            return Err(FlashError::PatchError(format!("KernelPatch 修补失败: {}", stderr)));
        }
        
        let new_kernel_data = fs::read(&patched_kernel)?;
        let _ = fs::remove_file(&temp_kernel);
        let _ = fs::remove_file(&patched_kernel);
        Ok(new_kernel_data)
    }

    pub async fn anykernel3_root(&self, zip_path: &str, boot_img_path: &str, target_partition: &str, is_raw_kernel: bool, auto_flash: bool) -> Result<String> {
        let zip_file = File::open(zip_path)?;
        let mut archive = ZipArchive::new(zip_file).map_err(|e| FlashError::PatchError(e.to_string()))?;
        let mut kernel_data = Vec::new();
        let mut found = false;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| FlashError::PatchError(e.to_string()))?;
            if file.name() == "Image" || file.name().ends_with("/Image") {
                file.read_to_end(&mut kernel_data)?;
                found = true;
                break;
            }
        }
        if !found { return Err(FlashError::PatchError("ZIP 中未找到 Image 文件".into())); }
 
        let mut old_boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut old_boot_data)?;

        // 打印原内核版本
        if is_raw_kernel {
            if let Some(v) = Self::detect_kmi_from_kernel(&old_boot_data) {
                println!("{}", format!("- 原始内核版本: {}", v).cyan());
            }
        } else {
            let boot_img = BootImage::parse(&old_boot_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
            if let Some(kernel) = boot_img.get_blocks().get_kernel() {
                if let Some(v) = Self::detect_kmi_from_kernel(kernel.get_data()) {
                    println!("{}", format!("- 原始内核版本: {}", v).cyan());
                }
            }
        }

        // 打印新内核版本
        if let Some(v) = Self::detect_kmi_from_kernel(&kernel_data) {
            println!("{}", format!("- 新内核版本:   {}", v).green());
        }

        let out_name = format!("ak3_patched_{}.img", target_partition);
        if is_raw_kernel {
            // 原始内核模式，直接写出 Image
            fs::write(&out_name, &kernel_data)?;
        } else {
            // 标准 Boot 模式，替换内核重新打包
            let boot_img = BootImage::parse(&old_boot_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
            let mut patcher = BootImagePatchOption::new(&boot_img);
            patcher.replace_kernel(Box::new(Cursor::new(kernel_data)), false);
     
            let mut output_data = Cursor::new(Vec::new());
            patcher.patch(&mut output_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
            fs::write(&out_name, output_data.into_inner())?;
        }
 
        if auto_flash {
            let res = self.client.run(&["flash", target_partition, &out_name]).await;
            let _ = fs::remove_file(&out_name);
            if res? {
                Ok(out_name)
            } else {
                Err(FlashError::FastbootError("Failed to flash AnyKernel3 image".into()))
            }
        } else {
            Ok(out_name)
        }
    }

    pub async fn magisk_patch(&self, boot_img_path: &str, apk_path: &str, _target_partition: &str) -> Result<String> {
        let apk_file = File::open(apk_path)?;
        let mut archive = ZipArchive::new(apk_file).map_err(|e| FlashError::PatchError(e.to_string()))?;
        let (mut magiskinit, mut magiskbin, mut stub, mut init_ld) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| FlashError::PatchError(e.to_string()))?;
            let name = file.name();
            if name.contains("libmagiskinit.so") && name.contains("arm64-v8a") {
                file.read_to_end(&mut magiskinit)?;
            } else if name == "assets/magisk64" || name.contains("libmagisk.so") {
                magiskbin.clear();
                file.read_to_end(&mut magiskbin)?;
            } else if name == "assets/stub.apk" {
                file.read_to_end(&mut stub)?;
            } else if name == "assets/init-ld" || name.contains("libinit-ld.so") {
                file.read_to_end(&mut init_ld)?;
            }
        }
        if magiskinit.is_empty() { return Err(FlashError::PatchError("APK 中未找到关键资产 (libmagiskinit.so)".into())); }

        self.do_magisk_patch(boot_img_path, magiskinit, magiskbin, stub, init_ld, "").await
    }

    pub async fn magisk_patch_with_files(&self, boot_img_path: &str, files: &[(String, PathBuf)], _target_partition: &str) -> Result<String> {
        let mut magiskinit = Vec::new();
        let mut magiskbin = Vec::new();
        let mut stub = Vec::new();
        let mut init_ld = Vec::new();

        for (key, path) in files {
            let mut content = Vec::new();
            File::open(path)?.read_to_end(&mut content)?;

            match key.as_str() {
                "magiskinit" => magiskinit = content,
                "magiskbin" => magiskbin = content,
                "stub" => stub = content,
                "init_ld" => init_ld = content,
                _ => {}
            }
        }

        if magiskinit.is_empty() { return Err(FlashError::PatchError("未找到 libmagiskinit.so".into())); }

        self.do_magisk_patch(boot_img_path, magiskinit, magiskbin, stub, init_ld, "").await
    }

    pub async fn flash_partition(&self, device_id: &str, partition: &str, image_path: &str) -> Result<()> {
        let temp_boot = format!("{}_temp_boot.img", partition);
        std::fs::copy(image_path, &temp_boot)?;

        let args = if device_id.is_empty() {
            vec!["flash", partition, &temp_boot]
        } else {
            vec!["-s", device_id, "flash", partition, &temp_boot]
        };

        let res = self.client.run(&args).await;
        let _ = std::fs::remove_file(&temp_boot);

        if res? {
            Ok(())
        } else {
            Err(FlashError::FastbootError("Failed to flash partition".into()))
        }
    }

    async fn do_magisk_patch(
        &self,
        boot_img_path: &str,
        magiskinit: Vec<u8>,
        magiskbin: Vec<u8>,
        stub: Vec<u8>,
        init_ld: Vec<u8>,
        target_partition: &str
    ) -> Result<String> {
        println!("{}", ">> 正在读取 Boot 镜像...".cyan().bold());
        let mut boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
        println!("{}", format!(">> Boot 镜像大小: {} bytes", boot_data.len()).green());

        let sha1_sum = {
            let mut hasher = Sha1::new();
            hasher.update(&boot_data);
            let sum = format!("{:x}", hasher.finalize());
            println!("{}", format!(">> 原始 SHA1: {}", sum).cyan());
            sum
        };

        println!("{}", ">> 正在解析 BootImage 格式...".cyan().bold());
        let boot_img = BootImage::parse(&boot_data).map_err(|e| FlashError::PatchError(e.to_string()))?;

        let has_kernel = boot_img.get_blocks().get_kernel().map(|k| !k.get_data().is_empty()).unwrap_or(false);
        let is_init_boot = !has_kernel;

        if is_init_boot {
            println!("{}", ">> 检测到 init_boot 分区（无 Kernel，仅 Ramdisk）".cyan().bold());
        }

        println!("{}", ">> 正在解压 Ramdisk...".cyan().bold());
        let mut ramdisk_data = Vec::new();
        if let Some(rd) = boot_img.get_blocks().get_ramdisk() {
            let raw_rd = rd.get_data();
            println!("{}", format!(">> 原始 Ramdisk 大小: {} bytes", raw_rd.len()).green());
            println!("{}", format!(">> Ramdisk 魔数: {:02x?}", &raw_rd[0..std::cmp::min(16, raw_rd.len())]).yellow());

            let magic_u32 = if raw_rd.len() >= 4 {
                Some(u32::from_le_bytes([raw_rd[0], raw_rd[1], raw_rd[2], raw_rd[3]]))
            } else {
                None
            };
            if let Some(magic) = magic_u32 {
                println!("{}", format!(">> Ramdisk 魔数 (LE): 0x{:08x}", magic).yellow());
            } else {
                println!("{}", ">> Ramdisk 魔数: 数据太短".yellow());
            }

            match utils::decompress_ramdisk(raw_rd) {
                Ok(data) => {
                    let raw_len = raw_rd.len();
                    if data.len() != raw_len {
                        ramdisk_data = data;
                        let ratio = 100.0 * (1.0 - ramdisk_data.len() as f64 / raw_len as f64);
                        println!("{}", format!(">> Ramdisk decompress OK: {} bytes (ratio: {:.1})",
                            ramdisk_data.len(), ratio).green());
                    } else {
                        println!("{}", ">> Ramdisk not compressed or unknown format, using raw data".yellow());
                        ramdisk_data = raw_rd.to_vec();
                    }
                    println!("{}", format!(">> Uncompressed CPIO magic: {:02x?}", &ramdisk_data[0..std::cmp::min(16, ramdisk_data.len())]).yellow());
                }
                Err(e) => {
                    println!("{}", format!(">> Decompress Ramdisk failed: {:?}, using raw data", e).yellow());
                    ramdisk_data = raw_rd.to_vec();
                }
            }

            println!("{}", ">> 正在解析 CPIO 归档...".cyan().bold());
            let cpio_start = detect_and_skip_cpio_header(&ramdisk_data);
            if cpio_start > 0 {
                println!("{}", format!(">> 检测到 {} 字节自定义头部，已跳过", cpio_start).yellow());
                let cpio_data = &ramdisk_data[cpio_start..];
                ramdisk_data = cpio_data.to_vec();
                println!("{}", format!(">> CPIO 数据大小: {} bytes", ramdisk_data.len()).green());
            }
        } else {
            println!("{}", ">> No Ramdisk data found".yellow());
        }

        let (mut entries, _) = if ramdisk_data.is_empty() {
            (Vec::new(), None)
        } else {
            utils::cpio_load_with_threecpio(&ramdisk_data)?
        };
        
        Self::patch_ramdisk_entries(&mut entries, &magiskinit, &magiskbin, &stub, &init_ld, &sha1_sum, &ramdisk_data)?;

        println!("{}", ">> 正在重新打包 Ramdisk (CPIO)...".cyan().bold());
        let new_cpio_data = utils::cpio_create_with_threecpio(&entries)?;
        println!("{}", format!(">> CPIO 包大小: {} bytes", new_cpio_data.len()).green());

        println!("{}", ">> 正在压缩 Ramdisk (GZip)...".cyan().bold());
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&new_cpio_data)?;
        let final_ramdisk = encoder.finish()?;
        println!("{}", format!(">> 最终 Ramdisk 大小: {} bytes", final_ramdisk.len()).green());

        if is_init_boot {
            println!("{}", ">> 正在修补 BootImage (init_boot)...".cyan().bold());
            // 使用 preserve_all=true 确保 V4 Header 字段被完整保留
            let mut patcher = BootImagePatchOption::new(&boot_img);
            patcher.replace_ramdisk(Box::new(Cursor::new(final_ramdisk)), true);

            let mut output_data = Cursor::new(Vec::new());
            patcher.patch(&mut output_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
            let patched_image = output_data.into_inner();
            println!("{}", format!(">> 修补后镜像大小: {} bytes", patched_image.len()).green());

            let out_name = format!("magisk_patched_{}.img", if target_partition.is_empty() { "init_boot" } else { target_partition });
            fs::write(&out_name, &patched_image)?;
            println!("{}", format!(">> Saved patched image: {}", out_name).green());

            if target_partition.is_empty() {
                println!("{}", ">> Skipping flash step (patch only)".yellow());
                return Ok(out_name);
            }

            println!("{}", format!(">> Flashing {} partition...", target_partition).cyan().bold());
            let res = self.client.run(&["flash", target_partition, &out_name]).await;
            let _ = fs::remove_file(&out_name);

            if res? {
                Ok(out_name)
            } else {
                Err(FlashError::FastbootError("Failed to flash patched init_boot image".into()))
            }
        } else {
            println!("{}", ">> 正在修补 BootImage...".cyan().bold());
            // 普通 boot 分区也使用 preserve_all=true，以确保最大的兼容性
            let mut patcher = BootImagePatchOption::new(&boot_img);
            patcher.replace_ramdisk(Box::new(Cursor::new(final_ramdisk)), true);

            let mut output_data = Cursor::new(Vec::new());
            patcher.patch(&mut output_data).map_err(|e| FlashError::PatchError(e.to_string()))?;
            let patched_image = output_data.into_inner();
            println!("{}", format!(">> 修补后镜像大小: {} bytes", patched_image.len()).green());

            let out_name = format!("magisk_patched_{}.img", if target_partition.is_empty() { "boot" } else { target_partition });
            fs::write(&out_name, &patched_image)?;
            println!("{}", format!(">> Saved patched image: {}", out_name).green());

            if target_partition.is_empty() {
                println!("{}", ">> Skipping flash step (patch only)".yellow());
                return Ok(out_name);
            }

            println!("{}", format!(">> Flashing {} partition...", target_partition).cyan().bold());
            let res = self.client.run(&["flash", target_partition, &out_name]).await;
            let _ = fs::remove_file(&out_name);

            if res? {
                Ok(out_name)
            } else {
                Err(FlashError::FastbootError("Failed to flash patched Magisk image".into()))
            }
        }
    }

    fn patch_ramdisk_entries(
        entries: &mut Vec<(String, u32, Vec<u8>)>,
        magiskinit: &[u8],
        magiskbin: &[u8],
        stub: &[u8],
        init_ld: &[u8],
        sha1_sum: &str,
        ramdisk_data: &[u8]
    ) -> Result<()> {
        entries.retain(|(name, _, _)| name != "init");
        entries.push(("init".to_string(), 0o750, magiskinit.to_vec()));
        println!("{}", ">> 已替换 init 为 Magiskinit".green());

        entries.retain(|(name, _, _)| !name.starts_with("overlay.d") && !name.starts_with(".backup"));
        println!("{}", ">> 已清理旧的 overlay.d 和 .backup".green());

        if !magiskbin.is_empty() {
            println!("{}", ">> 正在压缩 Magisk 二进制 (XZ)...".cyan().bold());
            let mut compressed = Vec::new();
            lzma_rs::xz_compress(&mut &magiskbin[..], &mut compressed).map_err(|e| FlashError::PatchError(format!("XZ compression failed: {:?}", e)))?;
            entries.push(("overlay.d/sbin/magisk.xz".to_string(), 0o644, compressed));
            println!("{}", ">> 已添加 overlay.d/sbin/magisk.xz".green());
        }

        if !stub.is_empty() {
            println!("{}", ">> 正在压缩 Stub APK (XZ)...".cyan().bold());
            let mut compressed = Vec::new();
            lzma_rs::xz_compress(&mut &stub[..], &mut compressed).map_err(|e| FlashError::PatchError(format!("XZ compression failed: {:?}", e)))?;
            entries.push(("overlay.d/sbin/stub.xz".to_string(), 0o644, compressed));
            println!("{}", ">> 已添加 overlay.d/sbin/stub.xz".green());
        }

        if !init_ld.is_empty() {
            println!("{}", ">> 正在压缩 init-ld (XZ)...".cyan().bold());
            let mut compressed = Vec::new();
            lzma_rs::xz_compress(&mut &init_ld[..], &mut compressed).map_err(|e| FlashError::PatchError(format!("XZ compression failed: {:?}", e)))?;
            entries.push(("overlay.d/sbin/init-ld.xz".to_string(), 0o644, compressed));
            println!("{}", ">> 已添加 overlay.d/sbin/init-ld.xz".green());
        }

        let config = format!("KEEPVERITY=false\nKEEPFORCEENCRYPT=false\nRECOVERYMODE=false\nVENDORBOOT=false\nSHA1={}\n", sha1_sum);
        entries.push((".backup/.magisk".to_string(), 0o000, config.into_bytes()));
        println!("{}", ">> 已添加 .magisk 配置".green());

        if let Some(sepolicy_data) = crate::sepolicy::extract_sepolicy(ramdisk_data) {
            match crate::sepolicy::Sepolicy::parse(&sepolicy_data) {
                Ok(mut sepolicy) => {
                    println!("{}", ">> 正在注入 Magisk SELinux 规则...".cyan().bold());
                    sepolicy.add_magisk_rules();
                    entries.push(("sepolicy".to_string(), 0o644, sepolicy.data));
                    println!("{}", ">> 已添加 sepolicy (含 Magisk 规则)".green());
                }
                Err(_) => {
                    entries.push(("sepolicy".to_string(), 0o644, sepolicy_data));
                    println!("{}", ">> 已添加 sepolicy".green());
                }
            }
        } else {
            println!("{}", ">> 未找到 sepolicy，跳过".yellow());
        }

        Ok(())
    }

    pub async fn is_in_fastbootd_mode(&self) -> Result<bool> {
        match self.client.list_devices().await {
            Ok(devices) => {
                Ok(devices.iter().any(|d| d.mode == crate::device::DeviceMode::FastbootD))
            }
            Err(_) => Ok(false)
        }
    }

    pub async fn is_in_fastboot_mode(&self) -> Result<bool> {
        match self.client.list_devices().await {
            Ok(devices) => {
                Ok(devices.iter().any(|d| d.mode == crate::device::DeviceMode::Fastboot))
            }
            Err(_) => Ok(false)
        }
    }

    pub async fn reboot_to_fastbootd(&self) -> Result<bool> {
        self.client.run(&["reboot", "fastboot"]).await
    }
}
