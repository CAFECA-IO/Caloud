//! FR-OPS-04：偵測節點規格與目前用量。
use caloud_core::agent::{GpuInfo, NodeSpecs};
use caloud_core::placement::Resources;
use std::path::{Path, PathBuf};
use sysinfo::{Disks, System};

/// Docker 資料目錄；磁碟可用空間以它所在的磁碟計算
pub const DOCKER_ROOT: &str = "/var/lib/docker";

/// 偵測目前主機的規格
/// `scripts/runner-setup.sh` 安裝 runsc 的位置（ADR 0015）
pub const GVISOR_RUNSC: &str = "/usr/local/lib/caloud/gvisor/runsc";

pub fn detect() -> NodeSpecs {
    let mut sys = System::new();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();
    let mounts: Vec<(PathBuf, u64)> = disks
        .list()
        .iter()
        .map(|d| (d.mount_point().to_path_buf(), d.available_space()))
        .collect();
    let (gpus, gpu_driver) = detect_gpus();
    NodeSpecs {
        cpu_cores: std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1),
        arch: std::env::consts::ARCH.to_string(),
        memory_bytes: sys.total_memory(),
        disk_free_bytes: disk_free(&mounts, Path::new(DOCKER_ROOT)),
        os: System::long_os_version().unwrap_or_else(|| std::env::consts::OS.to_string()),
        gvisor: on_path("runsc") || Path::new(GVISOR_RUNSC).exists(),
        buildkit: on_path("buildctl") || on_path("docker-buildx") || Path::new("/usr/libexec/docker/cli-plugins/docker-buildx").exists(),
        // 由自我檢查填入（FR-OPS-12）
        isolation: false,
        self_check: None,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        gpus,
        gpu_driver,
        // 由自我檢查填入（ADR 0022）
        gpu_ready: false,
        gpu_check: None,
    }
}

/// ADR 0022：以 `nvidia-smi` 偵測 NVIDIA GPU；沒有 nvidia-smi 或執行失敗時視為沒有 GPU
pub fn detect_gpus() -> (Vec<GpuInfo>, Option<String>) {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total,driver_version", "--format=csv,noheader,nounits"])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_nvidia_smi(&String::from_utf8_lossy(&o.stdout)),
        _ => (vec![], None),
    }
}

/// `NVIDIA GeForce RTX 4060 Ti, 16380, 580.178.04` 每行一張
pub fn parse_nvidia_smi(out: &str) -> (Vec<GpuInfo>, Option<String>) {
    let mut gpus = Vec::new();
    let mut driver = None;
    for line in out.lines() {
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        if f.len() != 3 || f[0].is_empty() {
            continue;
        }
        let Ok(memory_mb) = f[1].parse::<u64>() else { continue };
        gpus.push(GpuInfo {
            name: f[0].chars().take(100).collect(),
            memory_mb,
        });
        if driver.is_none() && f[2].chars().all(|c| c.is_ascii_digit() || c == '.') && !f[2].is_empty() {
            driver = Some(f[2].to_string());
        }
    }
    (gpus, driver)
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

    // ADR 0022
    #[test]
    fn parses_nvidia_smi() {
        let (g, d) = parse_nvidia_smi("NVIDIA GeForce RTX 4060 Ti, 16380, 580.178.04\nNVIDIA A10, 23028, 580.178.04\n\n");
        assert_eq!(g.len(), 2);
        assert_eq!(
            g[0],
            GpuInfo {
                name: "NVIDIA GeForce RTX 4060 Ti".into(),
                memory_mb: 16380
            }
        );
        assert_eq!(d.as_deref(), Some("580.178.04"));
        assert_eq!(parse_nvidia_smi("No devices were found\n"), (vec![], None));
        assert_eq!(parse_nvidia_smi("x, [N/A], 1\n").0, vec![]);
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
