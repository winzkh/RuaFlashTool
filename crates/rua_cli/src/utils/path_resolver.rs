use std::path::{Path, PathBuf};
use std::env;

pub fn resolve_subdir_dev_release(subdir: &str) -> Option<PathBuf> {
    let exe_path = env::current_exe().ok()?;
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
    let exe_str = exe_path.to_string_lossy();
    let is_dev = exe_str.contains("target\\debug") || exe_str.contains("target\\release");

    let candidate = if is_dev {
        exe_dir.join("..").join("..").join(subdir)
    } else {
        exe_dir.join(subdir)
    };

    if candidate.exists() && candidate.is_dir() {
        Some(candidate.canonicalize().unwrap_or(candidate))
    } else {
        None
    }
}
