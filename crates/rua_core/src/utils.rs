use crate::error::{FlashError, Result};
use std::io::{Read, Write, Cursor};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use lzma_rs::xz_decompress;
use zstd::stream::read::Decoder as ZstdDecoder;
use zstd::stream::write::Encoder as ZstdEncoder;
use lz4_flex::frame::FrameDecoder as Lz4Decoder;
use lz4_flex::frame::FrameEncoder as Lz4Encoder;
use cpio::newc::Reader as CpioReader;

#[derive(Clone, Copy, Debug)]
pub enum RamdiskFormat {
    Gzip,
    Xz,
    Zstd,
    Lz4,
    Lz4Legacy,
    Uncompressed,
}

pub fn detect_ramdisk_format(data: &[u8]) -> RamdiskFormat {
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
    } else if m[0] == 0x02 && m[1] == 0x21 && m[2] == 0x4c && m[3] == 0x18 {
        RamdiskFormat::Lz4Legacy
    } else {
        RamdiskFormat::Uncompressed
    }
}

pub fn decompress_ramdisk(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 4 {
        return Ok(data.to_vec());
    }

    let magic = &data[0..4];
    let magic_u32_be = u32::from_be_bytes([magic[0], magic[1], magic[2], magic[3]]);
    let magic_u32_le = u32::from_le_bytes([magic[0], magic[1], magic[2], magic[3]]);
    let mut output = Vec::new();

    match magic {
        [0x1f, 0x8b, ..] => {
            println!("[DEBUG] Detected GZIP format (magic: 0x{:08x})", magic_u32_be);
            let mut decoder = GzDecoder::new(data);
            decoder.read_to_end(&mut output).map_err(FlashError::Io)?;
        }
        [0xfd, 0x37, 0x7a, 0x58] if data.len() > 5 && data[4] == 0x5a && data[5] == 0x00 => {
            println!("[DEBUG] Detected XZ format (magic: 0x{:08x})", magic_u32_be);
            let mut reader = Cursor::new(data);
            xz_decompress(&mut reader, &mut output).map_err(|e| FlashError::PatchError(format!("XZ decompress failed: {:?}", e)))?;
        }
        [0x28, 0xb5, 0x2f, 0xfd] => {
            println!("[DEBUG] Detected Zstd format (magic: 0x{:08x})", magic_u32_be);
            let mut decoder = ZstdDecoder::new(data).map_err(FlashError::Io)?;
            decoder.read_to_end(&mut output).map_err(FlashError::Io)?;
        }
        [0x04, 0x22, 0x4d, 0x18] => {
            println!("[DEBUG] Detected LZ4 frame format (magic: 0x{:08x})", magic_u32_be);
            let mut decoder = Lz4Decoder::new(data);
            decoder.read_to_end(&mut output).map_err(FlashError::Io)?;
        }
        [0x02, 0x21, 0x4c, 0x18] => {
            println!("[DEBUG] Detected LZ4 legacy format (magic: 0x{:08x})", magic_u32_be);
            // LZ4 Legacy 常见于 Android 镜像，通常格式为 Magic(4) + CompressedSize(4) + Data
            // 或者仅仅是连续的 LZ4 块。
            if data.len() > 8 {
                let compressed_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
                println!("[DEBUG] LZ4 Legacy compressed size: {} bytes", compressed_size);
                
                // 尝试跳过头部进行块解压。由于不知道解压后大小，我们预分配一个较大的缓冲区（通常 ramdisk 不会超过 128MB）
                let mut decompressed = vec![0u8; 128 * 1024 * 1024];
                let data_start = if data.len() >= 9 && compressed_size + 8 == data.len() - 1 { 9 } else { 8 };
                
                match lz4_flex::block::decompress_into(&data[data_start..], &mut decompressed) {
                    Ok(size) => {
                        decompressed.truncate(size);
                        output = decompressed;
                        println!("[DEBUG] LZ4 Legacy block decompression success: {} bytes", size);
                    }
                    Err(e) => {
                        println!("[DEBUG] LZ4 Legacy block decompression failed: {:?}, trying as frame", e);
                        // 某些情况下虽然魔数是 legacy，但实际上可能是 frame 或其他变体
                        let mut decoder = Lz4Decoder::new(&data[data_start..]);
                        if let Ok(_) = decoder.read_to_end(&mut output) {
                            println!("[DEBUG] LZ4 Legacy fallback frame decompression success");
                        } else {
                            return Err(FlashError::PatchError(format!("LZ4 Legacy decompression failed: {:?}", e)));
                        }
                    }
                }
            } else {
                return Err(FlashError::PatchError("LZ4 Legacy data too short".into()));
            }
        }
        _ => {
            println!("[DEBUG] Unknown format (magic: 0x{:08x} / 0x{:08x}), trying raw data", magic_u32_le, magic_u32_be);
            return Ok(data.to_vec());
        }
    }
    Ok(output)
}

pub fn compress_ramdisk(fmt: RamdiskFormat, data: &[u8]) -> Result<Vec<u8>> {
    match fmt {
        RamdiskFormat::Gzip => {
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            enc.write_all(data).map_err(FlashError::Io)?;
            Ok(enc.finish().map_err(FlashError::Io)?)
        }
        RamdiskFormat::Xz => {
            let mut out = Vec::new();
            lzma_rs::xz_compress(&mut &data[..], &mut out).map_err(|e| FlashError::PatchError(format!("XZ compression failed: {:?}", e)))?;
            Ok(out)
        }
        RamdiskFormat::Zstd => {
            let mut enc = ZstdEncoder::new(Vec::new(), 0).map_err(FlashError::Io)?;
            enc.write_all(data).map_err(FlashError::Io)?;
            Ok(enc.finish().map_err(FlashError::Io)?)
        }
        RamdiskFormat::Lz4 => {
            let mut enc = Lz4Encoder::new(Vec::new());
            enc.write_all(data).map_err(FlashError::Io)?;
            Ok(enc.finish().map_err(FlashError::Lz4Error)?)
        }
        RamdiskFormat::Lz4Legacy => {
            // Android 镜像中的 LZ4 Legacy 压缩
            // 格式: Magic(4) + Size(4) + Data
            let compressed = lz4_flex::block::compress(data);
            let mut out = Vec::with_capacity(compressed.len() + 8);
            out.extend_from_slice(&[0x02, 0x21, 0x4c, 0x18]); // Magic
            out.extend_from_slice(&(compressed.len() as u32).to_le_bytes()); // Size
            out.extend_from_slice(&compressed);
            Ok(out)
        }
        RamdiskFormat::Uncompressed => Ok(data.to_vec()),
    }
}

pub fn cpio_extract_file(data: &[u8], target_name: &str) -> Option<Vec<u8>> {
    let mut cursor = Cursor::new(data);
    while let Ok(mut reader) = CpioReader::new(cursor) {
        let name = reader.entry().name().to_string();
        if name == target_name {
            let mut content = Vec::new();
            if reader.read_to_end(&mut content).is_ok() {
                return Some(content);
            }
        }
        if name == "TRAILER!!!" {
            break;
        }
        cursor = reader.finish().ok()?;
    }
    None
}

pub fn cpio_extract_file_newc(data: &[u8], target_name: &str) -> Option<(usize, Vec<u8>)> {
    let mut cursor = Cursor::new(data);
    while let Ok(mut reader) = CpioReader::new(cursor) {
        let name = reader.entry().name().to_string();
        let mode = reader.entry().mode();
        if name == target_name {
            let mut content = Vec::new();
            if reader.read_to_end(&mut content).is_ok() {
                return Some((mode as usize, content));
            }
        }
        if name == "TRAILER!!!" {
            break;
        }
        cursor = reader.finish().ok()?;
    }
    None
}

pub fn cpio_load_with_threecpio(data: &[u8]) -> Result<(Vec<(String, u32, Vec<u8>)>, Option<(usize, Vec<u8>)>)> {
    if data.is_empty() {
        return Ok((Vec::new(), None));
    }

    let header_magic = &data[0..6];
    println!("[DEBUG] CPIO header check: {:02x?}", header_magic);
    println!("[DEBUG] Expected newc magic: 070701 = {:02x?}", b"070701");

    let mut cursor = Cursor::new(data);
    let mut entries = Vec::new();
    let mut old_init_info = None;

    loop {
         let reader_result = CpioReader::new(cursor);
         let mut reader = match reader_result {
             Ok(reader) => reader,
             Err(e) => {
                 println!("[DEBUG] CPIO reader error: {:?}, trying to continue", e);
                 break;
             }
         };

        let name = reader.entry().name().to_string();
        let mode = reader.entry().mode();

        println!("[DEBUG] CPIO entry: name={}, mode=0o{:o}", name, mode);

        if name == "TRAILER!!!" {
            println!("[DEBUG] Found CPIO trailer, parsing complete");
            break;
        }

        let mut content = Vec::new();
        match reader.read_to_end(&mut content) {
            Ok(_) => {
                if name == "init" {
                    old_init_info = Some((mode as usize, content.clone()));
                }
                entries.push((name, mode as u32, content));
            }
            Err(e) => {
                println!("[DEBUG] Error reading CPIO entry: {:?}", e);
                break;
            }
        }

        cursor = match reader.finish() {
            Ok(c) => c,
            Err(e) => {
                println!("[DEBUG] Error getting next CPIO entry: {:?}", e);
                break;
            }
        };
    }

    if entries.is_empty() {
        println!("[DEBUG] No CPIO entries found. Data size: {} bytes", data.len());
        println!("[DEBUG] First 32 bytes: {:02x?}", &data[0..std::cmp::min(32, data.len())]);

        if data.len() >= 6 {
            let magic_check = u32::from_be_bytes([
                data[0], data[1], data[2], data[3]
            ]);
            println!("[DEBUG] Magic check: 0x{:08x}", magic_check);

            if magic_check == 0x30373037u32 {
                println!("[DEBUG] Magic looks like newc format but parsing failed");
            }
        }

        return Err(FlashError::PatchError(format!(
            "Failed to parse cpio archive: no entries found (format may be unsupported). Data size: {} bytes, magic: 0x{:08x}",
            data.len(),
            u32::from_be_bytes([data[0], data[1], data[2], data[3]])
        )));
    }

    println!("[DEBUG] Successfully parsed {} CPIO entries", entries.len());
    Ok((entries, old_init_info))
}

pub fn cpio_create_with_threecpio(entries: &[(String, u32, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    {
        let mut cursor = Cursor::new(&mut output);
        
        for (name, mode, content) in entries {
            let builder = cpio::NewcBuilder::new(name)
                .mode(*mode)
                .uid(1000)
                .gid(1000)
                .nlink(1);
            let mut writer = builder.write(&mut cursor, content.len() as u32);
            writer.write_all(content)?;
            writer.finish()?;
        }
        
        let _ = cpio::newc::trailer(&mut cursor);
    }
    Ok(output)
}
