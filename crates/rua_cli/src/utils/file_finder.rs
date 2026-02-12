use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LkmPackage {
    pub ko_path: PathBuf,
    pub kmi: String,
}

#[derive(Debug, Clone)]
pub struct KsuLkmVersion {
    pub version_name: String,
    pub ksuinit_path: PathBuf,
    pub ksuinit_d_path: PathBuf,
    pub ko_files: Vec<LkmPackage>,
}

#[derive(Debug, Clone)]
pub struct KsuLkmBranch {
    pub name: String,
    pub versions: Vec<KsuLkmVersion>,
}

pub struct FileFinder;

impl FileFinder {
    pub fn find_ksu_lkm_branches(base_dir: &Path) -> Vec<KsuLkmBranch> {
        let mut branches = Vec::new();
        let ksuinit_base = base_dir.join("KSUINIT");
        let lkm_base = base_dir.join("LKM");
        
        if !ksuinit_base.exists() || !ksuinit_base.is_dir() {
            return branches;
        }
        
        if !lkm_base.exists() || !lkm_base.is_dir() {
            return branches;
        }
        
        // 遍历 KSUINIT 下的分支
        if let Ok(ksuinit_entries) = fs::read_dir(&ksuinit_base) {
            for ksuinit_entry in ksuinit_entries.flatten() {
                if ksuinit_entry.path().is_dir() {
                    let branch_name = ksuinit_entry.file_name().to_string_lossy().to_string();
                    let lkm_branch_dir = lkm_base.join(&branch_name);
                    
                    if !lkm_branch_dir.exists() || !lkm_branch_dir.is_dir() {
                        continue;
                    }
                    
                    let mut versions = Vec::new();
                    
                    // 遍历该分支下的版本
                    if let Ok(version_entries) = fs::read_dir(ksuinit_entry.path()) {
                        for version_entry in version_entries.flatten() {
                            if version_entry.path().is_dir() {
                                let version_name = version_entry.file_name().to_string_lossy().to_string();
                                let ksuinit_version_dir = version_entry.path();
                                let lkm_version_dir = lkm_branch_dir.join(&version_name);
                                
                                if !lkm_version_dir.exists() || !lkm_version_dir.is_dir() {
                                    continue;
                                }
                                
                                let ksuinit_path = ksuinit_version_dir.join("ksuinit");
                                let ksuinit_d_path = ksuinit_version_dir.join("ksuinit.d");
                                
                                if !ksuinit_path.exists() {
                                    continue;
                                }
                                
                                let mut ko_files = Vec::new();
                                if let Ok(ko_entries) = fs::read_dir(&lkm_version_dir) {
                                    for ko_entry in ko_entries.flatten() {
                                        let ko_path = ko_entry.path();
                                        if ko_path.is_file() && ko_path.extension().is_some_and(|ext| ext == "ko") {
                                            if let Some(kmi) = Self::extract_kernelsu_kmi(&ko_path) {
                                                ko_files.push(LkmPackage {
                                                    ko_path: ko_path.clone(),
                                                    kmi,
                                                });
                                            }
                                        }
                                    }
                                }
                                
                                if !ko_files.is_empty() {
                                    versions.push(KsuLkmVersion {
                                        version_name,
                                        ksuinit_path,
                                        ksuinit_d_path,
                                        ko_files,
                                    });
                                }
                            }
                        }
                    }
                    
                    if !versions.is_empty() {
                        branches.push(KsuLkmBranch {
                            name: branch_name,
                            versions,
                        });
                    }
                }
            }
        }
        
        branches
    }
    
    fn extract_kernelsu_kmi(path: &Path) -> Option<String> {
        let filename = path.file_name()?.to_string_lossy().to_string();
        
        if !filename.ends_with("_kernelsu.ko") {
            return None;
        }
        
        let without_suffix = &filename[..filename.len() - "_kernelsu.ko".len()];
        
        // 文件名格式通常是 android12-5.10_kernelsu.ko
        if let Some(start) = without_suffix.find("android") {
            let kmi_part = &without_suffix[start..];
            let cleaned: String = kmi_part
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
                .collect();
            if !cleaned.is_empty() && cleaned.starts_with("android") {
                return Some(cleaned);
            }
        }
        
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_extract_kmi() {
        assert_eq!(
            FileFinder::extract_kernelsu_kmi(Path::new("android14-6.1_kernelsu.ko")).as_deref(),
            Some("android14-6.1")
        );
        assert_eq!(
            FileFinder::extract_kernelsu_kmi(Path::new("android13-5.10_kernelsu.ko")).as_deref(),
            Some("android13-5.10")
        );
    }
}
