//! 連線控制面（ADR 0011、docs/06 §6）：register → ack → 定期心跳；斷線以指數退避重連。
use crate::envs::EnvRunner;
use crate::selfcheck::SelfCheck;
use crate::specs::{self, Usage};
use crate::state::{self, NodeState};
use caloud_core::agent::{AgentMsg, ServerMsg, CONNECT_PATH};
use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// 重連退避上限
const MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct Options {
    /// 控制面網址，例如 `https://caloud.cafeca.io`
    pub server: String,
    /// 一次性加入權杖（尚未註冊時需要）
    pub join_token: Option<String>,
    pub name: String,
    pub state_dir: PathBuf,
    /// 環境部署（FR-OPS-13）
    pub envs: EnvRunner,
    /// 啟動時的自我檢查結果（FR-OPS-12）
    pub check: SelfCheck,
}

/// 由控制面網址算出 WebSocket 位址
pub fn ws_url(server: &str) -> anyhow::Result<String> {
    let s = server.trim_end_matches('/');
    let rest = if let Some(r) = s.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = s.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        anyhow::bail!("控制面網址需以 https:// 開頭（測試可用 http://）");
    };
    Ok(format!("{rest}{CONNECT_PATH}"))
}

/// 一次連線的結果
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 連線中斷，應重連
    Disconnected { registered: bool },
    /// 權杖被拒或節點被移除，不應再重連
    Rejected(String),
}

/// 連線並維持到中斷為止
pub async fn run_once(opts: &Options) -> anyhow::Result<Outcome> {
    let saved = state::load(&opts.state_dir)?.filter(|s| s.server == opts.server);
    let (join_token, node_token) = match &saved {
        Some(s) => (None, Some(s.node_token.clone())),
        None => match &opts.join_token {
            Some(j) => (Some(j.clone()), None),
            None => anyhow::bail!("尚未註冊，請以 --join 提供後台產生的加入權杖"),
        },
    };
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&opts.server)?).await?;
    let register = AgentMsg::Register {
        join_token,
        node_token,
        name: opts.name.clone(),
        specs: {
            let mut s = specs::detect();
            s.gvisor |= opts.check.gvisor;
            s.isolation = opts.check.isolation;
            s.self_check = opts.check.reason.clone();
            s
        },
    };
    ws.send(Message::Text(serde_json::to_string(&register)?.into())).await?;

    let ack = loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => break serde_json::from_str::<ServerMsg>(&t)?,
            Some(Ok(Message::Close(_))) | None => return Ok(Outcome::Disconnected { registered: false }),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
        }
    };
    let (node_id, heartbeat_secs, node_token) = match ack {
        ServerMsg::Ack {
            node_id,
            node_token,
            status,
            heartbeat_secs,
        } => {
            let token = match (node_token, &saved) {
                (Some(token), _) => {
                    state::save(
                        &opts.state_dir,
                        &NodeState {
                            server: opts.server.clone(),
                            node_id: node_id.clone(),
                            node_token: token.clone(),
                        },
                    )?;
                    token
                }
                (None, Some(s)) => s.node_token.clone(),
                (None, None) => anyhow::bail!("控制面未發給節點權杖"),
            };
            tracing::info!(node = %node_id, %status, "registered");
            (node_id, heartbeat_secs.max(1), token)
        }
        ServerMsg::Error { message } => return Ok(Outcome::Rejected(message)),
        _ => anyhow::bail!("控制面回應順序錯誤"),
    };

    // 通道與控制連線同生共死：任一條斷線就整個重連（docs/06 §6.1）
    let server = opts.server.clone();
    let mut tunnel = AbortOnDrop(tokio::spawn(async move { crate::tunnel::run(&server, &node_token).await }));

    // 環境部署的結果經這個通道送回控制面
    let (results_tx, mut results) = tokio::sync::mpsc::unbounded_channel::<AgentMsg>();

    let mut usage = Usage::new();
    let mut tick = tokio::time::interval(Duration::from_secs(heartbeat_secs));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let hb = AgentMsg::Heartbeat { used: usage.sample() };
                if ws.send(Message::Text(serde_json::to_string(&hb)?.into())).await.is_err() {
                    break;
                }
            }
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<ServerMsg>(&t) {
                    Ok(ServerMsg::Status { status }) => tracing::info!(node = %node_id, %status, "status changed"),
                    Ok(ServerMsg::Error { message }) => return Ok(Outcome::Rejected(message)),
                    Ok(ServerMsg::Deploy { env }) => {
                        let (runner, tx, isolated) = (opts.envs.clone(), results_tx.clone(), opts.check.isolation);
                        tokio::spawn(async move {
                            let msg = if isolated {
                                runner.deploy(&env).await
                            } else {
                                AgentMsg::EnvStatus { env_id: env.id.clone(), status: "failed".into(), error: Some("節點未通過自我檢查，不接平臺環境。".into()), services: vec![] }
                            };
                            let _ = tx.send(msg);
                        });
                    }
                    Ok(ServerMsg::Destroy { env_id }) => {
                        let (runner, tx) = (opts.envs.clone(), results_tx.clone());
                        tokio::spawn(async move {
                            let _ = tx.send(runner.destroy(&env_id).await);
                        });
                    }
                    _ => {}
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            Some(msg) = results.recv() => {
                if ws.send(Message::Text(serde_json::to_string(&msg)?.into())).await.is_err() {
                    break;
                }
            }
            r = &mut tunnel.0 => {
                match r {
                    Ok(Err(e)) => tracing::warn!(error = %e, "tunnel failed"),
                    _ => tracing::warn!("tunnel closed"),
                }
                break;
            }
        }
    }
    Ok(Outcome::Disconnected { registered: true })
}

/// 離開 `run_once` 時一併結束通道
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 下一次重連前等待的時間：1、2、4… 秒，上限 60 秒
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(6)).min(MAX_BACKOFF)
}

/// 持續連線；權杖被拒時回傳錯誤（不無限重試）
pub async fn run(opts: Options) -> anyhow::Result<()> {
    let mut attempt = 0u32;
    loop {
        match run_once(&opts).await {
            Ok(Outcome::Rejected(msg)) => anyhow::bail!("控制面拒絕連線：{msg}"),
            Ok(Outcome::Disconnected { registered }) => {
                if registered {
                    attempt = 0;
                }
                tracing::warn!("disconnected, reconnecting");
            }
            Err(e) => tracing::warn!(error = %e, "connect failed"),
        }
        tokio::time::sleep(backoff(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-OPS-04
    #[test]
    fn builds_ws_url_and_backoff() {
        assert_eq!(
            ws_url("https://caloud.cafeca.io/").unwrap(),
            "wss://caloud.cafeca.io/agent/v1/connect"
        );
        assert_eq!(ws_url("http://127.0.0.1:8080").unwrap(), "ws://127.0.0.1:8080/agent/v1/connect");
        assert!(ws_url("caloud.cafeca.io").is_err());
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(20), Duration::from_secs(60));
    }
}
