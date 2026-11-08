//! Where everything lives. `HIVE_HOME` relocates all of it at once, which is
//! also how the tests keep out of the real state directory.

use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("HIVE_HOME") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".hive")
}

pub fn config_path() -> PathBuf {
    home().join("config.toml")
}

pub fn state_path() -> PathBuf {
    home().join("state.json")
}

pub fn socket_path() -> PathBuf {
    let path = home().join("hive.sock");
    // sockaddr_un caps paths near 104 bytes. A deep HIVE_HOME gets a short
    // per-home path in the temp dir instead.
    if path.as_os_str().len() < 100 {
        return path;
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from("/tmp").join(format!(
        "hive-{uid}-{:08x}.sock",
        fnv1a(home().to_string_lossy().as_bytes()) as u32
    ))
}

pub fn pid_path() -> PathBuf {
    home().join("hive.pid")
}

pub fn log_path() -> PathBuf {
    home().join("daemon.log")
}

/// Worktrees for one repository, grouped so two repos with the same folder
/// name never collide.
pub fn worktrees_for(repo: &Path) -> PathBuf {
    let base = repo
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    home()
        .join("worktrees")
        .join(format!("{base}-{:08x}", fnv1a(repo.to_string_lossy().as_bytes()) as u32))
}

pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(home())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
