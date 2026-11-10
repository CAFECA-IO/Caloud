//! Runner 通道（FR-OPS-09，docs/06 §6.1）：節點 id → yamux Control。
//!
//! 控制面的 `/agent/v1/tunnel` 端點建立 yamux session 後登記到這裡；Edge 經 `dial` 開 stream 轉發。
use caloud_core::tunnel::request_connect;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_yamux::Control;

/// 上游連線（TCP 或通道 stream）
pub trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> IoStream for T {}

#[derive(Clone, Default)]
pub struct TunnelRegistry {
    map: Arc<Mutex<HashMap<String, (u64, Control)>>>,
    gen: Arc<AtomicU64>,
}

impl TunnelRegistry {
    /// 登記節點的通道，回傳代號（移除時用來確認是同一條）
    pub fn register(&self, node: &str, control: Control) -> u64 {
        let gen = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        let old = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node.to_string(), (gen, control));
        if let Some((_, mut c)) = old {
            tokio::spawn(async move { c.close().await });
        }
        gen
    }

    /// 移除通道；`gen` 為 Some 時只在仍是同一條時移除
    pub fn remove(&self, node: &str, gen: Option<u64>) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(node).is_some_and(|(g, _)| gen.is_none_or(|x| x == *g)) {
            if let Some((_, mut c)) = map.remove(node) {
                tokio::spawn(async move { c.close().await });
            }
        }
    }

    pub fn contains(&self, node: &str) -> bool {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).contains_key(node)
    }

    /// 經節點通道連到 `target`（host:port）
    pub async fn open(&self, node: &str, target: &str) -> io::Result<tokio_yamux::StreamHandle> {
        let control = self.map.lock().unwrap_or_else(|e| e.into_inner()).get(node).map(|(_, c)| c.clone());
        let mut control = control.ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "node tunnel not connected"))?;
        let mut stream = control.open_stream().await.map_err(io::Error::other)?;
        tokio::time::timeout(Duration::from_secs(10), request_connect(&mut stream, target))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "tunnel connect timed out"))??;
        Ok(stream)
    }

    /// 連到上游：有節點走通道，否則直接 TCP
    pub async fn dial(&self, node: Option<&str>, target: &str) -> io::Result<Box<dyn IoStream>> {
        match node {
            Some(n) => Ok(Box::new(self.open(n, target).await?)),
            None => {
                let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(target))
                    .await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
                Ok(Box::new(s))
            }
        }
    }
}
