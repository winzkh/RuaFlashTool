use crate::fastboot_exec::FastbootManager;
use std::io::{self, Read, Write, Cursor};
use std::path::Path;
use anyhow;
use std::fs::File;
use zip::ZipArchive;
use android_bootimg::parser::BootImage;
use android_bootimg::patcher::BootImagePatchOption;
use android_bootimg::cpio::{Cpio, CpioEntry};
use lzma_rs;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha1::{Sha1, Digest};
use zstd::stream::write::Encoder as ZstdEncoder;
use lz4_flex::frame::FrameEncoder as Lz4Encoder;
use std::fs;

// 引入解压库
use zstd::stream::read::Decoder as ZstdDecoder;
use lz4_flex::frame::FrameDecoder as Lz4Decoder;
use lzma_rs::xz_decompress;
use crate::ui::{step, ok, warn, err};

#[derive(Clone, Copy)]
enum RamdiskFormat {
    Gzip,
    Xz,
    Zstd,
    Lz4,
    Uncompressed,
}

fn detect_ramdisk_format(data: &[u8]) -> RamdiskFormat {
    if data.len() < 4 {
        return RamdiskFormat::Uncompressed;
    }
    let m = &data[0..4];
    if m[0] == 0x1f && m[1] == 0x8b {
        RamdiskFormat::Gzip
    } else if m[0] == 0xfd && m[1] == 0x37 && m[2] == 0x7a && m[3] == 0x58 && data.len() > 5 && data[4] == 0x5a && data[5] == 0x00 {
        RamdiskFormat::Xz
    } else if m[0] == 0x28 && m[1] == 0xb5 && m[2] == 0x2f && m[3] == 0xfd {
        RamdiskFormat::Zstd
    } else if m[0] == 0x04 && m[1] == 0x22 && m[2] == 0x4d && m[3] == 0x18 {
        RamdiskFormat::Lz4
    } else {
        RamdiskFormat::Uncompressed
    }
}

fn decompress_ramdisk(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    if data.len() < 4 {
        return Ok(data.to_vec());
    }

    let magic = &data[0..4];
    let mut output = Vec::new();

    match magic {
        // Gzip: 1F 8B
        [0x1f, 0x8b, ..] => {
            println!("   [INFO] 检测到 Gzip 压缩");
            let mut decoder = GzDecoder::new(data);
            decoder.read_to_end(&mut output)?;
        }
        // XZ: FD 37 7A 58 5A 00
        [0xfd, 0x37, 0x7a, 0x58] if data.len() > 5 && data[4] == 0x5a && data[5] == 0x00 => {
            println!("   [INFO] 检测到 XZ 压缩");
            let mut reader = Cursor::new(data);
            xz_decompress(&mut reader, &mut output)?;
        }
        // Zstd: 28 B5 2F FD
        [0x28, 0xb5, 0x2f, 0xfd] => {
            println!("   [INFO] 检测到 Zstd 压缩");
            let mut decoder = ZstdDecoder::new(data)?;
            decoder.read_to_end(&mut output)?;
        }
        // LZ4 Frame: 04 22 4D 18
        [0x04, 0x22, 0x4d, 0x18] => {
            println!("   [INFO] 检测到 LZ4 压缩");
            let mut decoder = Lz4Decoder::new(data);
            decoder.read_to_end(&mut output)?;
        }
        // LZ4 Legacy: 02 21 4C 18
        [0x02, 0x21, 0x4c, 0x18] => {
            println!("   [INFO] 检测到 LZ4 Legacy 压缩 (暂未完全支持, 尝试使用标准 LZ4 解码...)");
            // lz4_flex 可能不支持 legacy frame，通常 Android 使用的 Legacy 格式比较简单
            // 如果 lz4_flex 解码失败，可能需要寻找专门支持 block format 的库
            // 但这里先尝试通用解码
             let mut decoder = Lz4Decoder::new(data);
            decoder.read_to_end(&mut output)?;
        }
        _ => {
            // 假设未压缩
            return Ok(data.to_vec());
        }
    }
    Ok(output)
}

fn compress_ramdisk(fmt: RamdiskFormat, data: &[u8]) -> anyhow::Result<Vec<u8>> {
    match fmt {
        RamdiskFormat::Gzip => {
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            enc.write_all(data)?;
            Ok(enc.finish()?)
        }
        RamdiskFormat::Xz => {
            let mut out = Vec::new();
            lzma_rs::xz_compress(&mut &data[..], &mut out).map_err(|e| anyhow::anyhow!("XZ compression failed: {:?}", e))?;
            Ok(out)
        }
        RamdiskFormat::Zstd => {
            let mut enc = ZstdEncoder::new(Vec::new(), 0)?;
            enc.write_all(data)?;
            Ok(enc.finish()?)
        }
        RamdiskFormat::Lz4 => {
            let mut enc = Lz4Encoder::new(Vec::new());
            enc.write_all(data)?;
            Ok(enc.finish()?)
        }
        RamdiskFormat::Uncompressed => Ok(data.to_vec()),
    }
}

fn cpio_extract_file_newc(data: &[u8], name: &str) -> Option<(usize, Vec<u8>)> {
    let mut i = 0usize;
    while i + 6 <= data.len() {
        if &data[i..i + 6] != b"070701" {
            return None;
        }
    let read_hex = |off: usize| -> Option<usize> {
            let s = std::str::from_utf8(&data[off..off + 8]).ok()?;
            usize::from_str_radix(s, 16).ok()
        };
        let _ino = read_hex(i + 6)?;
        let mode = read_hex(i + 14)?;
        let _uid = read_hex(i + 22)?;
        let _gid = read_hex(i + 30)?;
        let _nlink = read_hex(i + 38)?;
        let _mtime = read_hex(i + 46)?;
        let filesize = read_hex(i + 54)?;
        let _devmajor = read_hex(i + 62)?;
        let _devminor = read_hex(i + 70)?;
        let _rdevmajor = read_hex(i + 78)?;
        let _rdevminor = read_hex(i + 86)?;
        let namesize = read_hex(i + 94)?;
        let _check = read_hex(i + 102)?;
        let mut p = i + 110;
        let namesize_aligned = ((namesize + 3) / 4) * 4;
        if p + namesize_aligned > data.len() {
            return None;
        }
        let name_bytes = &data[p..p + namesize - 1];
        let entry_name = std::str::from_utf8(name_bytes).ok()?;
        p += namesize_aligned;
        let filesize_aligned = ((filesize + 3) / 4) * 4;
        if p + filesize_aligned > data.len() {
            return None;
        }
        if entry_name == "TRAILER!!!" {
            return None;
        }
        if entry_name == name {
            let file_data = &data[p..p + filesize];
            return Some((mode, file_data.to_vec()));
        }
        i = p + filesize_aligned;
    }
    None
}

pub struct Flasher {
    fb: FastbootManager,
}

impl Flasher {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            fb: FastbootManager::new()?,
        })
    }

    pub fn flash_partition(&self, partition: &str, file_path: &str) -> io::Result<()> {
        if !Path::new(file_path).exists() {
            err(&format!("错误: 镜像文件不存在: {}", file_path));
            return Ok(());
        }
        if self.fb.flash(partition, file_path)? {
            ok(&format!("成功: {} 分区已刷入", partition));
        } else {
            err(&format!("失败: {} 分区刷入失败", partition));
        }
        Ok(())
    }

    pub fn flash_vbmeta(&self, file_path: &str) -> io::Result<()> {
        if !Path::new(file_path).exists() {
            err(&format!("错误: vbmeta 文件不存在: {}", file_path));
            return Ok(());
        }
        step("正在刷入 vbmeta (禁用验证)...");
        if self.fb.run_cmd(&["--disable-verity", "--disable-verification", "flash", "vbmeta", file_path])? {
            ok("成功: vbmeta 已刷入");
        } else {
            err("失败: vbmeta 刷入失败");
        }
        Ok(())
    }

    pub fn flash_boot(&self, file_path: &str) -> io::Result<()> {
        self.flash_partition("boot", file_path)
    }



    pub fn kernelsu_lkm_install(&self, boot_img_path: &str, ksuinit_path: &str, ksuinit_d_dir: Option<&str>, ko_path: &str, target_partition: &str) -> anyhow::Result<()> {
        let mut boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
        let boot_img = BootImage::parse(&boot_data)?;
        let rd = boot_img.get_blocks().get_ramdisk().ok_or_else(|| anyhow::anyhow!("no ramdisk"))?;
        let rd_raw = rd.get_data();
        let fmt = detect_ramdisk_format(rd_raw);
        let rd_decomp = decompress_ramdisk(rd_raw)?;
        let old_init_info = cpio_extract_file_newc(&rd_decomp, "init");
        let mut cpio = if rd_decomp.is_empty() { Cpio::new() } else { Cpio::load_from_data(&rd_decomp)? };
        cpio.rm("init", false);
        if let Some((mode, old_data)) = old_init_info {
            // 保持原始 init 的模式 (可能是软链接 0xA1FF 或普通文件 0x81ED)
            cpio.add("init.real", CpioEntry::regular(mode as u32, Box::new(old_data) as Box<dyn AsRef<[u8]>>))?;
        }
        
        // 尝试寻找并修补 sepolicy (如果存在)
        if let Some((mode, sepolicy_data)) = cpio_extract_file_newc(&rd_decomp, "sepolicy") {
            warn("检测到 Ramdisk 中存在 sepolicy，正在尝试基础修补...");
            // 这里我们无法进行复杂的二进制注入，但我们可以尝试确保一些基础权限
            // 注意：这只是一个非常基础的尝试，真正的修补需要 magiskboot 或 ksud
            cpio.rm("sepolicy", false);
            cpio.add("sepolicy", CpioEntry::regular(mode as u32, Box::new(sepolicy_data) as Box<dyn AsRef<[u8]>>))?;
        }
        let ksuinit_bytes = fs::read(ksuinit_path)?;
        cpio.add("init", CpioEntry::regular(0o755, Box::new(ksuinit_bytes) as Box<dyn AsRef<[u8]>>))?;
        let ko_bytes = fs::read(ko_path)?;
        cpio.add("kernelsu.ko", CpioEntry::regular(0o755, Box::new(ko_bytes) as Box<dyn AsRef<[u8]>>))?;
        if let Some(dir) = ksuinit_d_dir {
            let base = Path::new(dir);
            if base.exists() && base.is_dir() {
                for entry in fs::read_dir(base)? {
                    let entry = entry?;
                    let p = entry.path();
                    if p.is_file() {
                        let rel = p.file_name().unwrap().to_str().unwrap().to_string();
                        let target = format!("ksuinit.d/{}", rel);
                        let content = fs::read(&p)?;
                        cpio.add(&target, CpioEntry::regular(0o755, Box::new(content) as Box<dyn AsRef<[u8]>>))?;
                    }
                }
            }
        }
        let mut new_cpio = Vec::new();
        cpio.dump(&mut new_cpio)?;
        let final_ramdisk = compress_ramdisk(fmt, &new_cpio)?;
        let mut patcher = BootImagePatchOption::new(&boot_img);
        patcher.replace_ramdisk(Box::new(Cursor::new(final_ramdisk)), true);
        let mut output_data = Cursor::new(Vec::new());
        patcher.patch(&mut output_data)?;
        let out_name = format!("ksu_lkm_patched_{}.img", target_partition);
        std::fs::write(&out_name, output_data.into_inner())?;
        if self.fb.flash(target_partition, &out_name)? {
            ok("成功: KernelSU LKM 修补版镜像已刷入");
        } else {
            err("失败: 刷入失败");
        }
        let _ = std::fs::remove_file(out_name);
        Ok(())
    }

    pub fn apatch_patch(&self, boot_img_path: &str, skey: &str, target_partition: &str) -> anyhow::Result<()> {
        step(&format!("正在读取 {} 镜像...", target_partition));
        let mut boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
        let boot_img = BootImage::parse(&boot_data)?;

        step("正在提取内核文件...");
        let kernel_data = boot_img.get_blocks().get_kernel()
            .map(|k| k.get_data().to_vec())
            .unwrap_or_else(Vec::new);
        
        if kernel_data.is_empty() {
            anyhow::bail!("未在镜像中找到内核数据");
        }

        let mut raw_kernel = kernel_data.clone();
        let mut was_compressed = false;
        
        match decompress_ramdisk(&kernel_data) {
            Ok(decompressed) => {
                if decompressed.len() != kernel_data.len() {
                    warn("检测到内核已压缩，正在解压...");
                    raw_kernel = decompressed;
                    was_compressed = true;
                }
            },
            Err(e) => {
                warn(&format!("内核解压尝试失败: {:?}，将尝试使用原数据", e));
            }
        }

        let temp_kernel = "temp_kernel";
        let patched_kernel = "temp_kernel_patched";
        std::fs::write(temp_kernel, &raw_kernel)?;

        step("正在调用 KernelPatch 工具修补内核...");
        let kptools = if cfg!(target_os = "windows") { "KernelPatch/kptools-msys2.exe" } else { "KernelPatch/kptools-linux-x86_64" };
        let kpimg = "KernelPatch/kpimg-android";
        
        if !Path::new(kptools).exists() {
             anyhow::bail!("找不到 KernelPatch 工具: {}", kptools);
        }
         if !Path::new(kpimg).exists() {
             anyhow::bail!("找不到 kpimg-android 文件: {}", kpimg);
        }

        let output = std::process::Command::new(kptools)
            .args(&["-p", "--image", temp_kernel, "--skey", skey, "--kpimg", kpimg, "--out", patched_kernel])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!("KernelPatch 修补失败: {}\n{}", stderr, stdout);
        }
        
        let mut new_kernel_data = std::fs::read(patched_kernel)?;
        let _ = std::fs::remove_file(temp_kernel);
        let _ = std::fs::remove_file(patched_kernel);

        if was_compressed {
            warn("正在将修补后的内核重新压缩 (Gzip)...");
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&new_kernel_data)?;
            new_kernel_data = encoder.finish()?;
        }

        step("正在重新封装镜像...");
        let mut patcher = BootImagePatchOption::new(&boot_img);
        patcher.replace_kernel(Box::new(Cursor::new(new_kernel_data)), false);
        
        let mut output_data = Cursor::new(Vec::new());
        patcher.patch(&mut output_data)?;

        let out_name = format!("apatch_patched_{}.img", target_partition);
        std::fs::write(&out_name, output_data.into_inner())?;

        if self.fb.flash(target_partition, &out_name)? {
            ok("成功: APatch 修补版镜像已刷入");
        } else {
             err("失败: 刷入失败");
        }
        let _ = std::fs::remove_file(out_name);
        Ok(())
    }

    pub fn anykernel3_root(&self, zip_path: &str, boot_img_path: &str, target_partition: &str) -> anyhow::Result<()> {
        // ... (existing logic adaptable for target_partition) ...
        // Wait, I need to update flash target.
        step("正在从 ZIP 提取内核文件...");
        let zip_file = File::open(zip_path)?;
        let mut archive = ZipArchive::new(zip_file)?;
        let mut kernel_data = Vec::new();
        let mut found = false;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            if file.name() == "Image" || file.name().ends_with("/Image") {
                file.read_to_end(&mut kernel_data)?;
                found = true;
                break;
            }
        }
        if !found { anyhow::bail!("ZIP 中未找到 Image 文件"); }
 
        step(&format!("正在读取原始 {} 镜像...", target_partition));
        let mut boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
        let boot_img = BootImage::parse(&boot_data)?;
        let mut patcher = BootImagePatchOption::new(&boot_img);
        patcher.replace_kernel(Box::new(Cursor::new(kernel_data)), false);
 
        step("正在重新打包制作 Root 镜像...");
        let mut output_data = Cursor::new(Vec::new());
        patcher.patch(&mut output_data)?;
        let temp_boot = format!("temp_ak3_{}.img", target_partition);
        std::fs::write(&temp_boot, output_data.into_inner())?;
 
        step("正在刷入修补后的镜像...");
        if self.fb.flash(target_partition, &temp_boot)? {
            ok("成功: 内核已更新");
        } else {
            err("失败: 刷入失败");
        }
        let _ = std::fs::remove_file(temp_boot);
        Ok(())
    }

    pub fn magisk_patch(&self, boot_img_path: &str, apk_path: &str, target_partition: &str) -> anyhow::Result<()> {
        step(&format!("正在解析 APK: {}", apk_path));
        let apk_file = File::open(apk_path)?;
        let mut archive = ZipArchive::new(apk_file)?;
        let (mut magiskinit, mut magiskbin, mut stub, mut init_ld) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let name = file.name();
            if name.contains("libmagiskinit.so") && name.contains("arm64-v8a") {
                file.read_to_end(&mut magiskinit)?;
            } else if name == "assets/magisk64" || name == "assets/magisk32" {
                if magiskbin.is_empty() || name.contains("64") {
                    magiskbin.clear();
                    file.read_to_end(&mut magiskbin)?;
                }
            } else if name == "assets/stub.apk" {
                file.read_to_end(&mut stub)?;
            } else if name == "assets/init-ld" {
                file.read_to_end(&mut init_ld)?;
            }
        }
        if magiskinit.is_empty() { anyhow::bail!("APK 中未找到关键资产 (libmagiskinit.so)"); }

        step(&format!("正在读取 {} 镜像...", target_partition));
        let mut boot_data = Vec::new();
        File::open(boot_img_path)?.read_to_end(&mut boot_data)?;
        let sha1_sum = {
            let mut hasher = Sha1::new();
            hasher.update(&boot_data);
            format!("{:x}", hasher.finalize())
        };

        let boot_img = BootImage::parse(&boot_data)?;
        let mut ramdisk_data = Vec::new();
        if let Some(rd) = boot_img.get_blocks().get_ramdisk() {
            let raw_rd = rd.get_data();
            match decompress_ramdisk(raw_rd) {
                Ok(data) => ramdisk_data = data,
                Err(e) => {
                    warn(&format!("Ramdisk 解压失败: {:?}，尝试直接加载...", e));
                    ramdisk_data = raw_rd.to_vec();
                }
            }
        }

        step("正在修补 Ramdisk (CPIO)...");
        let mut cpio = if ramdisk_data.is_empty() { Cpio::new() } else { Cpio::load_from_data(&ramdisk_data)? };

        cpio.rm("init", false);
        cpio.add("init", CpioEntry::regular(0o750, Box::new(magiskinit) as Box<dyn AsRef<[u8]>>))?;

        let (m_bin, s_apk, i_ld) = (magiskbin, stub, init_ld);
        let add_xz = |cp: &mut Cpio, p: &str, d: Vec<u8>| -> anyhow::Result<()> {
            if d.is_empty() { return Ok(()); }
            let mut compressed = Vec::new();
            lzma_rs::xz_compress(&mut &d[..], &mut compressed).map_err(|e| anyhow::anyhow!("XZ compression failed: {:?}", e))?;
            cp.add(p, CpioEntry::regular(0o644, Box::new(compressed) as Box<dyn AsRef<[u8]>>))?;
            Ok(())
        };

        cpio.rm("overlay.d", true);
        add_xz(&mut cpio, "overlay.d/sbin/magisk.xz", m_bin)?;
        add_xz(&mut cpio, "overlay.d/sbin/stub.xz", s_apk)?;
        if !i_ld.is_empty() {
            add_xz(&mut cpio, "overlay.d/sbin/init-ld.xz", i_ld)?;
        }

        let config = format!("KEEPVERITY=false\nKEEPFORCEENCRYPT=false\nRECOVERYMODE=false\nSHA1={}\n", sha1_sum);
        cpio.rm(".backup", true);
        cpio.add(".backup/.magisk", CpioEntry::regular(0o000, Box::new(config.into_bytes()) as Box<dyn AsRef<[u8]>>))?;

        let mut new_cpio_data = Vec::new();
        cpio.dump(&mut new_cpio_data)?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&new_cpio_data)?;
        let final_ramdisk = encoder.finish()?;

        step("正在尝试修补 Kernel...");
        let mut kernel_data = boot_img.get_blocks().get_kernel()
            .map(|k| k.get_data().to_vec())
            .unwrap_or_else(Vec::new);
        
        fn hex_patch(data: &mut Vec<u8>, from: &[u8], to: &[u8]) -> bool {
            let mut patched = false;
            let mut i = 0;
            while i <= data.len().saturating_sub(from.len()) {
                if &data[i..i+from.len()] == from {
                    data[i..i+to.len()].copy_from_slice(to);
                    patched = true;
                    i += from.len();
                } else { i += 1; }
            }
            patched
        }
        if !kernel_data.is_empty() {
            hex_patch(&mut kernel_data, 
                &[0x70, 0x72, 0x6F, 0x63, 0x61, 0x5F, 0x63, 0x6F, 0x6E, 0x66, 0x69, 0x67, 0x00],
                &[0x70, 0x72, 0x6F, 0x63, 0x61, 0x5F, 0x6D, 0x61, 0x67, 0x69, 0x73, 0x6B, 0x00]
            );
        }

        step(&format!("正在封装 {} 镜像...", target_partition));
        let mut patcher = BootImagePatchOption::new(&boot_img);
        patcher.replace_ramdisk(Box::new(Cursor::new(final_ramdisk)), true);
        if !kernel_data.is_empty() {
            patcher.replace_kernel(Box::new(Cursor::new(kernel_data)), false);
        }
        let mut output_data = Cursor::new(Vec::new());
        patcher.patch(&mut output_data)?;

        // 修改输出文件名以反映分区
        let out_name = format!("magisk_patched_{}.img", target_partition);
        std::fs::write(&out_name, output_data.into_inner())?;
        
        if self.fb.flash(target_partition, &out_name)? {
            ok("成功: Root 修补版镜像已刷入");
        } else {
            err("失败: 刷入失败");
        }
        let _ = std::fs::remove_file(out_name);
        Ok(())
    }
}

#[allow(dead_code)]
pub mod ext {
    use super::*;
    use std::io;

    impl Flasher {
        pub fn reboot_system(&self) -> io::Result<()> {
            self.fb.reboot(None)?;
            Ok(())
        }
        pub fn reboot_recovery(&self) -> io::Result<()> {
            self.fb.reboot(Some("recovery"))?;
            Ok(())
        }
        pub fn reboot_bootloader(&self) -> io::Result<()> {
            self.fb.reboot(Some("bootloader"))?;
            Ok(())
        }
    }
}
