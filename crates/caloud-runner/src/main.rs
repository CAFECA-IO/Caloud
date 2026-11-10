//! `caloud-agent`：Runner 節點上的 agent（ADR 0012）。
//!
//! 用法：`caloud-agent --server https://caloud.cafeca.io --join caloud_join_…`
//! 註冊後節點權杖存在 `~/.caloud-agent/node.json`，之後不需要再帶 `--join`。
use caloud_runner::client::{self, Options};
use caloud_runner::state;
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("用法：caloud-agent --server <https://控制面> [--join <加入權杖>] [--name <節點名稱>] [--state-dir <目錄>]");
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
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--server" => server = Some(val()),
            "--join" => join = Some(val()),
            "--name" => name = Some(val()),
            "--state-dir" => dir = Some(PathBuf::from(val())),
            _ => usage(),
        }
    }
    let Some(server) = server.or_else(|| std::env::var("CALOUD_AGENT_SERVER").ok()) else {
        usage()
    };
    client::run(Options {
        server,
        join_token: join,
        name: name.unwrap_or_else(default_name),
        state_dir: dir.unwrap_or_else(state::default_dir),
    })
    .await
}
