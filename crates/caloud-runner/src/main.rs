//! `caloud-agent`：Runner 節點上的 agent（ADR 0012）。
//!
//! 用法：`caloud-agent --server https://caloud.cafeca.io --join caloud_join_…`
//! 註冊後節點權杖存在 `~/.caloud-agent/node.json`，之後不需要再帶 `--join`。
//! 啟動時先自我檢查（FR-OPS-12）；`caloud-agent --self-check` 只檢查並印出結果。
use caloud_runner::client::{self, Options};
use caloud_runner::envs::{EnvRunner, DEFAULT_DNS};
use caloud_runner::{selfcheck, specs, state};
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "用法：caloud-agent --server <https://控制面> [--join <加入權杖>] [--name <節點名稱>] [--state-dir <目錄>] \
         [--docker <docker 路徑>] [--iptables <iptables 路徑>] [--dns 1.1.1.1,8.8.8.8]\n      caloud-agent --self-check [--state-dir <目錄>]"
    );
    std::process::exit(2)
}

fn default_name() -> String {
    let raw = sysinfo::System::host_name().unwrap_or_else(|| "runner".into()).to_ascii_lowercase();
    let name: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        "runner".into()
    } else {
        name.chars().take(63).collect()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let mut server = None;
    let mut join = std::env::var("CALOUD_AGENT_JOIN").ok();
    let mut name = None;
    let mut dir = None;
    let mut docker = "docker".to_string();
    let mut iptables = "iptables".to_string();
    let mut dns: Vec<String> = DEFAULT_DNS.iter().map(|s| s.to_string()).collect();
    let mut self_check_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--server" => server = Some(val()),
            "--join" => join = Some(val()),
            "--name" => name = Some(val()),
            "--state-dir" => dir = Some(PathBuf::from(val())),
            "--docker" => docker = val(),
            "--iptables" => iptables = val(),
            "--dns" => dns = val().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "--self-check" => self_check_only = true,
            _ => usage(),
        }
    }
    let state_dir = dir.unwrap_or_else(state::default_dir);
    let envs = EnvRunner::new(docker, state_dir.clone(), dns);
    let sp = specs::detect();
    let mut check = selfcheck::check(&envs, &iptables, sp.disk_free_bytes).await;
    match &check.reason {
        None => tracing::info!("self-check passed: gVisor and environment isolation verified"),
        Some(r) => tracing::warn!(reason = %r, "self-check failed; this node will not accept platform environments"),
    }
    // ADR 0022：GPU 只在隔離通過時檢查
    if check.isolation && sp.gpu_driver.is_some() {
        let runsc = if std::path::Path::new(specs::GVISOR_RUNSC).exists() {
            specs::GVISOR_RUNSC
        } else {
            "runsc"
        };
        (check.gpu_ready, check.gpu_check) = selfcheck::gpu(&envs, runsc, sp.gpu_driver.as_deref()).await;
        match &check.gpu_check {
            None => tracing::info!(gpus = sp.gpus.len(), "GPU self-check passed (nvproxy)"),
            Some(r) => tracing::warn!(reason = %r, "GPU self-check failed; GPU environments will not be scheduled here"),
        }
    }
    if self_check_only {
        println!(
            "gvisor={} isolation={} gpu_ready={}",
            check.gvisor, check.isolation, check.gpu_ready
        );
        if let Some(r) = &check.gpu_check {
            println!("gpu_check={r}");
        }
        if let Some(r) = &check.reason {
            println!("reason={r}");
        }
        std::process::exit(if check.isolation { 0 } else { 1 });
    }
    let Some(server) = server.or_else(|| std::env::var("CALOUD_AGENT_SERVER").ok()) else {
        usage()
    };
    client::run(Options {
        server,
        join_token: join,
        name: name.unwrap_or_else(default_name),
        state_dir,
        envs,
        check,
    })
    .await
}
