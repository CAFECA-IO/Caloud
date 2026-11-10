//! Runner 通道（docs/06 §6.1、FR-OPS-09）：第二條 WebSocket 承載 yamux，agent 為 server。
//!
//! 控制面開 stream 送 `CONNECT host:port`；agent 檢查目標後連線並回 `OK`，之後雙向轉送原始位元組。
use crate::client::ws_url;
use bytes::Bytes;
use caloud_core::agent::CONNECT_PATH;
use caloud_core::tunnel::{forbidden_ip, parse_connect, read_line, write_line, ws_io, TUNNEL_PATH};
use futures_util::{SinkExt, StreamExt};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;
use tokio_yamux::{Config, Session, StreamHandle};

/// 通道 WebSocket 位址
pub fn tunnel_url(server: &str) -> anyhow::Result<String> {
    let url = ws_url(server)?;
    Ok(format!("{}{TUNNEL_PATH}", url.trim_end_matches(CONNECT_PATH)))
}

/// 控制面主機的 IP（迴圈位址除外）：不允許經通道回連（docs/07）
async fn control_plane_ips(server: &str) -> Vec<IpAddr> {
    let rest = server.split("://").nth(1).unwrap_or(server);
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = match authority.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => h,
        _ => authority,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match tokio::net::lookup_host((host, 443)).await {
        Ok(addrs) => addrs.map(|a| a.ip()).filter(|ip| !ip.is_loopback()).collect(),
        Err(_) => Vec::new(),
    }
}

/// 連上通道並服務到中斷為止
pub async fn run(server: &str, node_token: &str) -> anyhow::Result<()> {
    let mut req = tunnel_url(server)?.into_client_request()?;
    req.headers_mut().insert(AUTHORIZATION, format!("Bearer {node_token}").parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req).await?;
    let blocked = Arc::new(control_plane_ips(server).await);

    let (sink, stream) = ws.split();
    let stream = stream
        .take_while(|m| std::future::ready(!matches!(m, Ok(Message::Close(_)))))
        .filter_map(|m| {
            std::future::ready(match m {
                Ok(Message::Binary(b)) => Some(Ok::<Bytes, io::Error>(b)),
                Ok(_) => None,
                Err(e) => Some(Err(io::Error::other(e))),
            })
        });
    let sink = sink
        .sink_map_err(io::Error::other)
        .with(|b: Bytes| std::future::ready(Ok::<_, io::Error>(Message::Binary(b))));
    let mut session = Session::new_server(ws_io(stream, sink), Config::default());
    tracing::info!("tunnel connected");
    while let Some(s) = session.next().await {
        let s = s?;
        let blocked = blocked.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_stream(s, &blocked).await {
                tracing::debug!(error = %e, "tunnel stream closed");
            }
        });
    }
    Ok(())
}

async fn serve_stream(mut s: StreamHandle, blocked: &[IpAddr]) -> io::Result<()> {
    let line = read_line(&mut s).await?;
    let mut upstream = match connect_target(&line, blocked).await {
        Ok(u) => u,
        Err(reason) => {
            write_line(&mut s, &format!("ERR {reason}")).await?;
            return Ok(());
        }
    };
    write_line(&mut s, "OK").await?;
    tokio::io::copy_bidirectional(&mut s, &mut upstream).await?;
    Ok(())
}

/// 依 `CONNECT` 行連到目標；拒絕 metadata 與控制面主機
async fn connect_target(line: &str, blocked: &[IpAddr]) -> Result<TcpStream, String> {
    let (host, port) = parse_connect(line)?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| "unknown host".to_string())?
        .collect();
    if addrs.is_empty() {
        return Err("unknown host".into());
    }
    if addrs.iter().any(|a| allowed(a.ip(), blocked).is_err()) {
        return Err("forbidden target".into());
    }
    for a in addrs {
        if let Ok(Ok(t)) = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(a)).await {
            return Ok(t);
        }
    }
    Err("connection refused".into())
}

fn allowed(ip: IpAddr, blocked: &[IpAddr]) -> Result<(), ()> {
    if forbidden_ip(ip) || blocked.contains(&ip) {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-OPS-09
    #[test]
    fn builds_tunnel_url() {
        assert_eq!(
            tunnel_url("https://caloud.cafeca.io/").unwrap(),
            "wss://caloud.cafeca.io/agent/v1/tunnel"
        );
        assert_eq!(tunnel_url("http://127.0.0.1:8080").unwrap(), "ws://127.0.0.1:8080/agent/v1/tunnel");
    }

    // FR-OPS-09
    #[tokio::test]
    async fn screens_connect_targets() {
        let blocked: Vec<IpAddr> = vec!["10.0.0.5".parse().unwrap()];
        assert_eq!(
            connect_target("CONNECT 169.254.169.254:80", &blocked).await.unwrap_err(),
            "forbidden target"
        );
        assert_eq!(
            connect_target("CONNECT 10.0.0.5:443", &blocked).await.unwrap_err(),
            "forbidden target"
        );
        assert!(connect_target("GET / HTTP/1.1", &blocked).await.is_err());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(connect_target(&format!("CONNECT 127.0.0.1:{port}"), &blocked).await.is_ok());
        drop(l);
        assert_eq!(
            connect_target(&format!("CONNECT 127.0.0.1:{port}"), &blocked).await.unwrap_err(),
            "connection refused"
        );
    }
}
