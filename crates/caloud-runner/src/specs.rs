//! FR-OPS-04：偵測節點規格與目前用量。
use caloud_core::agent::NodeSpecs;
use caloud_core::placement::Resources;
use std::path::{Path, PathBuf};
use sysinfo::{Disks, System};

/// Docker 資料目錄；磁碟可用空間以它所在的磁碟計算
pub const DOCKER_ROOT: &str = "/var/lib/docker";

/// 偵測目前主機的規格
pub fn detect() -> NodeSpecs {
    let mut sys = System::new();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();
    let mounts: Vec<(PathBuf, u64)> = disks
        .list()
        .iter()
        .map(|d| (d.mount_point().to_path_buf(), d.available_space()))
        .collect();
    NodeSpecs {
        cpu_cores: std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1),
        arch: std::env::consts::ARCH.to_string(),
        memory_bytes: sys.total_memory(),
        disk_free_bytes: disk_free(&mounts, Path::new(DOCKER_ROOT)),
        os: System::long_os_version().unwrap_or_else(|| std::env::consts::OS.to_string()),
        gvisor: on_path("runsc"),
        buildkit: on_path("buildctl") || on_path("docker-buildx") || Path::new("/usr/libexec/docker/cli-plugins/docker-buildx").exists(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// 目前用量：CPU（millicores）與記憶體
pub struct Usage {
    sys: System,
}

impl Default for Usage {
    fn default() -> Self {
        Self::new()
    }
}

impl Usage {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Usage { sys }
    }

    pub fn sample(&mut self) -> Resources {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cores = self.sys.cpus().len().max(1) as f64;
        let pct = f64::from(self.sys.global_cpu_usage()).clamp(0.0, 100.0);
        Resources {
            cpu_millis: (pct / 100.0 * cores * 1000.0) as u64,
            memory_bytes: self.sys.used_memory(),
        }
    }
}

/// 以「掛載點為目標路徑最長前綴」的磁碟計算可用空間；找不到則 0
pub fn disk_free(mounts: &[(PathBuf, u64)], target: &Path) -> u64 {
    mounts
        .iter()
        .filter(|(m, _)| target.starts_with(m))
        .max_by_key(|(m, _)| m.components().count())
        .map(|(_, free)| *free)
        .unwrap_or(0)
}

/// 執行檔是否在 PATH 上
pub fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-OPS-04
    #[test]
    fn disk_free_uses_longest_matching_mount() {
        let mounts = vec![
            (PathBuf::from("/"), 10),
            (PathBuf::from("/var"), 20),
            (PathBuf::from("/var/lib/docker"), 30),
            (PathBuf::from("/var/lib/dockerx"), 99),
        ];
        assert_eq!(disk_free(&mounts, Path::new("/var/lib/docker")), 30);
        assert_eq!(disk_free(&mounts[..2], Path::new("/var/lib/docker")), 20);
        assert_eq!(disk_free(&[], Path::new("/var/lib/docker")), 0);
    }

    // FR-OPS-04
    #[test]
    fn detects_this_host() {
        let s = detect();
        assert!(s.cpu_cores >= 1);
        assert!(s.memory_bytes > 0);
        assert!(!s.arch.is_empty());
        assert_eq!(s.agent_version, env!("CARGO_PKG_VERSION"));
        assert!(on_path("sh"));
        assert!(!on_path("definitely-not-a-binary-xyz"));
    }
}
