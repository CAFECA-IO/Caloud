//! 連線控制面（ADR 0011、docs/06 §6）：register → ack → 定期心跳；斷線以指數退避重連。
use crate::specs::{self, Usage};
use crate::state::{self, NodeState};
use caloud_core::agent::{AgentMsg, ServerMsg, CONNECT_PATH};
use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// 重連退避上限
const MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Options {
    /// 控制面網址，例如 `https://caloud.cafeca.io`
    pub server: String,
    /// 一次性加入權杖（尚未註冊時需要）
    pub join_token: Option<String>,
    pub name: String,
    pub state_dir: PathBuf,
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
        specs: specs::detect(),
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
    let (node_id, heartbeat_secs) = match ack {
        ServerMsg::Ack {
            node_id,
            node_token,
            status,
            heartbeat_secs,
        } => {
            if let Some(token) = node_token {
                state::save(
                    &opts.state_dir,
                    &NodeState {
                        server: opts.server.clone(),
                        node_id: node_id.clone(),
                        node_token: token,
                    },
                )?;
            }
            tracing::info!(node = %node_id, %status, "registered");
            (node_id, heartbeat_secs.max(1))
        }
        ServerMsg::Error { message } => return Ok(Outcome::Rejected(message)),
        ServerMsg::Status { .. } => anyhow::bail!("控制面回應順序錯誤"),
    };

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
                    _ => {}
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            }
        }
    }
    Ok(Outcome::Disconnected { registered: true })
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
