//! Runner agent 的 WebSocket 端點與連線表（docs/06 §6，ADR 0011）。
//!
//! FR-OPS-04：第一則訊息必須是 `register`；之後接收心跳，並把維運者的狀態變更推給 agent。
use crate::service::admin;
use crate::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use caloud_core::agent::{AgentMsg, ServerMsg, HEARTBEAT_SECS, REGISTER_TIMEOUT_SECS};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// 單則訊息上限
const MAX_MESSAGE: usize = 64 * 1024;

/// 已連線的 agent：節點 id → 推送通道
#[derive(Clone, Default)]
pub struct AgentHub(Arc<Mutex<HashMap<String, mpsc::UnboundedSender<ServerMsg>>>>);

impl AgentHub {
    fn insert(&self, id: &str, tx: mpsc::UnboundedSender<ServerMsg>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), tx);
    }

    /// 只移除同一條連線（同一節點可能已有較新的連線）
    fn remove(&self, id: &str, tx: &mpsc::UnboundedSender<ServerMsg>) {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(id).is_some_and(|cur| cur.same_channel(tx)) {
            map.remove(id);
        }
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id)
    }

    pub fn send(&self, id: &str, msg: ServerMsg) {
        if let Some(tx) = self.0.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            let _ = tx.send(msg);
        }
    }

    /// 中斷連線（節點被移除）：通知 agent 不要再重連
    pub fn disconnect(&self, id: &str) {
        if let Some(tx) = self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(id) {
            let _ = tx.send(ServerMsg::Error {
                message: "節點已被移除。".into(),
            });
        }
    }
}

pub async fn connect(State(s): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |sock| session(s, sock))
}

async fn send(sock: &mut WebSocket, msg: &ServerMsg) -> bool {
    match serde_json::to_string(msg) {
        Ok(text) => sock.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn reject(mut sock: WebSocket, message: &str) {
    let _ = send(
        &mut sock,
        &ServerMsg::Error {
            message: message.to_string(),
        },
    )
    .await;
    let _ = sock.send(Message::Close(None)).await;
}

/// 等第一則文字訊息
async fn first_text(sock: &mut WebSocket) -> Option<String> {
    while let Some(Ok(m)) = sock.recv().await {
        match m {
            Message::Text(t) => return Some(t.to_string()),
            Message::Close(_) => return None,
            _ => {}
        }
    }
    None
}

async fn session(s: AppState, mut sock: WebSocket) {
    let first = tokio::time::timeout(Duration::from_secs(REGISTER_TIMEOUT_SECS), first_text(&mut sock)).await;
    let Ok(Some(text)) = first else {
        return reject(sock, "請先送出 register。").await;
    };
    let Ok(AgentMsg::Register {
        join_token,
        node_token,
        name,
        specs,
    }) = serde_json::from_str::<AgentMsg>(&text)
    else {
        return reject(sock, "第一則訊息必須是 register。").await;
    };
    let (node, token) = match admin::register_node(&s, join_token.as_deref(), node_token.as_deref(), &name, &specs).await {
        Ok(v) => v,
        Err(crate::ApiError::Unauthorized) => return reject(sock, "權杖無效、已使用或已過期。").await,
        Err(e) => return reject(sock, &e.public_message()).await,
    };
    let ack = ServerMsg::Ack {
        node_id: node.id.clone(),
        node_token: token,
        status: node.status.clone(),
        heartbeat_secs: HEARTBEAT_SECS,
    };
    if !send(&mut sock, &ack).await {
        return;
    }
    tracing::info!(node = %node.id, name = %node.name, "agent connected");

    let (tx, mut rx) = mpsc::unbounded_channel();
    s.agents.insert(&node.id, tx.clone());
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(msg) => {
                    let last = matches!(msg, ServerMsg::Error { .. });
                    if !send(&mut sock, &msg).await || last {
                        break;
                    }
                }
                None => break,
            },
            incoming = sock.recv() => match incoming {
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<AgentMsg>(&t) {
                    Ok(AgentMsg::Heartbeat { used }) => match admin::heartbeat(&s, &node.id, used).await {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(e) => tracing::warn!(error = %e, "heartbeat failed"),
                    },
                    _ => tracing::debug!(node = %node.id, "unexpected agent message"),
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    s.agents.remove(&node.id, &tx);
    let _ = sock.send(Message::Close(None)).await;
    tracing::info!(node = %node.id, "agent disconnected");
}

/// FR-OPS-04：背景任務，定期把太久沒心跳的節點標為離線
pub fn spawn_offline_sweeper(s: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
        loop {
            tick.tick().await;
            match caloud_core::db::mark_offline_nodes(&s.db, caloud_core::now()).await {
                Ok(n) if n > 0 => tracing::warn!(count = n, "nodes marked offline"),
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "offline sweep failed"),
            }
        }
    });
}
