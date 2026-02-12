use crate::error::{FlashError, Result};

const POLICYDB_MAGIC: u32 = 0xf97cff8f_u32;

#[derive(Debug, Clone, PartialEq)]
pub struct Sepolicy {
    pub data: Vec<u8>,
    pub version: i32,
}

impl Sepolicy {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 8 {
            return Err(FlashError::PatchError(
                "sepolicy data too small".to_string()
            ));
        }

        let magic = u32::from_le_bytes([
            data[0], data[1], data[2], data[3]
        ]);

        if magic != POLICYDB_MAGIC {
            return Err(FlashError::PatchError(
                format!("Invalid sepolicy magic: {:x}", magic)
            ));
        }

        let version = i32::from_le_bytes([
            data[4], data[5], data[6], data[7]
        ]);

        Ok(Self {
            data: data.to_vec(),
            version,
        })
    }

    pub fn is_valid(&self) -> bool {
        self.data.len() >= 8 && self.version >= 15
    }

    pub fn add_magisk_rules(&mut self) {
        let magisk_rules = Self::get_magisk_avc_rules();

        self.data.extend_from_slice(&magisk_rules);
    }

    fn get_magisk_avc_rules() -> Vec<u8> {
        vec![
            0x61, 0x6c, 0x6c, 0x6f, 0x77, 0x00,
        ]
    }
}

pub fn extract_sepolicy(ramdisk_data: &[u8]) -> Option<Vec<u8>> {
    // 使用统一的 cpio 解析逻辑
    crate::utils::cpio_extract_file(ramdisk_data, "sepolicy")
}

pub fn get_magisk_selinux_rules() -> &'static str {
    r#"
    ; Magisk SELinux Policy Rules
    ; These rules allow Magisk processes to function properly

    ; Allow magisk to access shell
    allow magisk shell:file { read write open getattr };

    ; Allow magisk to access su socket
    allow magisk su:unix_stream_socket { connectto getattr };

    ; Allow magisk to access tmpfs
    allow magisk tmpfs:file { read write create unlink };

    ; Allow magisk to access system data
    allow magisk system_data_file:file { read write open };

    ; Allow magisk to access kernel proc
    allow magisk proc_kernel:file { read open };

    ; Allow magisk to access selinuxfs
    allow magisk selinuxfs:file { read write open getattr };

    ; Allow init_real to execute
    allow init_real shell:file { execute };
    allow init_real magisk_exec:file { execute };

    ; Allow overlayfs operations
    allow overlayfs tmpfs:file { read write create };
    allow overlayfs system_data_file:file { read write create };

    ; Allowzygote process operations
    allow zygote magisk:unix_stream_socket { connectto };
    allow zygote magisk_exec:file { execute };

    ; Allow system_server operations
    allow system_server magisk:unix_stream_socket { connectto };
    allow system_server magisk_exec:file { execute };

    ; Suppress common AVC denials for magisk
    dontaudit magisk self:capability { sys_module };
    dontaudit magisk kernel:security { compute_avc };
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sepolicy_parse_valid() {
        let mut data = Vec::new();
        data.extend_from_slice(&POLICYDB_MAGIC.to_le_bytes());
        data.extend_from_slice(&26i32.to_le_bytes());
        data.extend_from_slice(&[0u8; 100]);

        let result = Sepolicy::parse(&data);
        assert!(result.is_ok());
        let sepolicy = result.unwrap();
        assert_eq!(sepolicy.version, 26);
        assert!(sepolicy.is_valid());
    }

    #[test]
    fn test_sepolicy_parse_invalid_magic() {
        let mut data = Vec::new();
        data.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
        data.extend_from_slice(&26i32.to_le_bytes());

        let result = Sepolicy::parse(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_sepolicy_parse_too_small() {
        let data = vec![0u8; 4];

        let result = Sepolicy::parse(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_sepolicy_not_found() {
        let empty_data = vec![0u8; 512];
        let result = extract_sepolicy(&empty_data);
        assert!(result.is_none());
    }
}
