//! Runner agent 的 WebSocket 端點與連線表（docs/06 §6，ADR 0011）。
//!
//! FR-OPS-04：第一則訊息必須是 `register`；之後接收心跳，並把維運者的狀態變更推給 agent。
use crate::service::admin;
use crate::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use caloud_core::agent::{AgentMsg, ServerMsg, HEARTBEAT_SECS, REGISTER_TIMEOUT_SECS};
use futures_util::{SinkExt, StreamExt};
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
                    Ok(AgentMsg::EnvStatus { env_id, status, stage, error, services }) => {
                        if let Err(e) = crate::service::environments::report(&s, &node.id, &env_id, &status, stage, error, services).await {
                            tracing::warn!(error = %e, "environment status failed");
                        }
                    }
                    Ok(AgentMsg::VerifyStage { id, stage, status }) => {
                        if let Err(e) = crate::service::verify::on_stage(&s, &node.id, &id, &stage, &status).await {
                            tracing::warn!(error = %e, "verify stage failed");
                        }
                    }
                    Ok(AgentMsg::VerifyIssue { id, issue }) => {
                        if let Err(e) = crate::service::verify::on_issue(&s, &node.id, &id, issue).await {
                            tracing::warn!(error = %e, "verify issue failed");
                        }
                    }
                    Ok(AgentMsg::VerifyLog { id, stage, line }) => {
                        if let Err(e) = crate::service::verify::on_log(&s, &node.id, &id, &stage, &line).await {
                            tracing::warn!(error = %e, "verify log failed");
                        }
                    }
                    Ok(AgentMsg::BuildUsage { kind, id, secs }) => {
                        if let Err(e) = crate::service::build::record(&s, &node.id, &kind, &id, secs).await {
                            tracing::warn!(error = %e, "build usage failed");
                        }
                    }
                    Ok(AgentMsg::VerifyDone { id, error, kept }) => {
                        if let Err(e) = crate::service::verify::on_done(&s, &node.id, &id, error, kept).await {
                            tracing::warn!(error = %e, "verify done failed");
                        }
                    }
                    Ok(AgentMsg::EnvLog { env_id, stage, line }) => {
                        if let Err(e) = crate::service::environments::add_log(&s, Some(&node.id), &env_id, &stage, &line).await {
                            tracing::warn!(error = %e, "deploy log failed");
                        }
                    }
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

/// Runner 通道（docs/06 §6.1）：Bearer 節點權杖；binary frame 承載 yamux，控制面為 client。
pub async fn tunnel(State(s): State<AppState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let node = match admin::node_by_token(&s, token).await {
        Ok(id) => id,
        Err(crate::ApiError::Unauthorized) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(e) => return e.into_response(),
    };
    ws.max_message_size(TUNNEL_FRAME)
        .max_frame_size(TUNNEL_FRAME)
        .on_upgrade(move |sock| tunnel_session(s, node, sock))
}

/// 通道單一 frame 上限（yamux 預設視窗 256 KiB）
const TUNNEL_FRAME: usize = 1024 * 1024;

async fn tunnel_session(s: AppState, node: String, sock: WebSocket) {
    let (sink, stream) = sock.split();
    let stream = stream
        .take_while(|m| std::future::ready(!matches!(m, Ok(Message::Close(_)))))
        .filter_map(|m| {
            std::future::ready(match m {
                Ok(Message::Binary(b)) => Some(Ok::<Bytes, std::io::Error>(b)),
                Ok(_) => None,
                Err(e) => Some(Err(std::io::Error::other(e))),
            })
        });
    let sink = sink
        .sink_map_err(std::io::Error::other)
        .with(|b: Bytes| std::future::ready(Ok::<_, std::io::Error>(Message::Binary(b))));
    let io = caloud_core::tunnel::ws_io(stream, sink);
    let mut session = tokio_yamux::Session::new_client(io, tokio_yamux::Config::default());
    let gen = s.tunnels.register(&node, session.control());
    tracing::info!(%node, "runner tunnel connected");
    // 控制面不接受 agent 開的 stream；持續 poll 以驅動 session
    while let Some(r) = session.next().await {
        if r.is_err() {
            break;
        }
    }
    s.tunnels.remove(&node, Some(gen));
    tracing::info!(%node, "runner tunnel disconnected");
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
