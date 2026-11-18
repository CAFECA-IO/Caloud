use crate::{CertStore, RouteTable, TunnelRegistry, Upstream};
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CONNECTION, HOST, LOCATION, UPGRADE};
use hyper::{Request, Response, StatusCode, Uri, Version};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::net::SocketAddr;
use tokio::net::TcpListener;

type Body = BoxBody<Bytes, hyper::Error>;

/// 共用的轉發狀態（`:80` 與 `:443` 終結後的連線）
#[derive(Clone)]
pub(crate) struct Proxy {
    pub routes: RouteTable,
    pub tunnels: TunnelRegistry,
    pub certs: CertStore,
    client: Client<HttpConnector, Incoming>,
}

impl Proxy {
    pub(crate) fn new(routes: RouteTable, tunnels: TunnelRegistry, certs: CertStore) -> Self {
        Self {
            routes,
            tunnels,
            certs,
            client: Client::builder(TokioExecutor::new()).build_http(),
        }
    }

    /// 以 HTTP/1 或 h2 服務一條連線；`https` 為 Edge 終結 TLS 後的連線
    pub(crate) async fn serve_conn<I>(self, io: I, peer: SocketAddr, https: bool)
    where
        I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let svc = hyper::service::service_fn(move |req| handle(req, peer, https, self.clone()));
        if let Err(e) = auto::Builder::new(TokioExecutor::new())
            .serve_connection_with_upgrades(TokioIo::new(io), svc)
            .await
        {
            tracing::debug!(error = %e, "connection closed");
        }
    }
}

/// HTTP 反向代理（`:80`）。TLS 見 `passthrough::serve_tls`。
///
/// 上游在 Runner 節點上時經 `tunnels` 轉發（M4-07）；WebSocket／HTTP Upgrade 兩種上游都支援（FR-DOM-06）。
/// host 有 Edge 憑證時回 308 轉 HTTPS（FR-DOM-05，ADR 0016）。
pub async fn serve_http(addr: SocketAddr, routes: RouteTable, tunnels: TunnelRegistry, certs: CertStore) -> anyhow::Result<()> {
    serve_listener(TcpListener::bind(addr).await?, routes, tunnels, certs).await
}

/// 同 [`serve_http`]，但使用已綁定的 listener（測試用 port 0）
pub async fn serve_listener(listener: TcpListener, routes: RouteTable, tunnels: TunnelRegistry, certs: CertStore) -> anyhow::Result<()> {
    let proxy = Proxy::new(routes, tunnels, certs);
    tracing::info!(addr = %listener.local_addr()?, "proxy listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        tokio::spawn(proxy.clone().serve_conn(stream, peer, false));
    }
}

async fn handle(mut req: Request<Incoming>, peer: SocketAddr, https: bool, p: Proxy) -> Result<Response<Body>, hyper::Error> {
    // ACME HTTP-01：Caloud 自己的 token 優先；不認得的 token 依 Host 轉給 App（App 自行申請憑證，ADR 0009）
    if let Some(token) = req.uri().path().strip_prefix("/.well-known/acme-challenge/") {
        if let Some(k) = p.routes.acme_challenge(token) {
            return Ok(text(StatusCode::OK, k));
        }
    }

    let host = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| req.uri().authority().map(|a| a.as_str()))
        .unwrap_or_default()
        .to_string();

    // FR-DOM-05：有 Edge 憑證的網域一律走 HTTPS
    if !https && p.certs.has_cert(&host) {
        return Ok(redirect_https(&host, &req));
    }

    let Some(up) = p.routes.lookup_route(&host) else {
        // FR-DOM-07
        return Ok(text(StatusCode::NOT_FOUND, format!("此網域尚未綁定運作中的 App：{host}")));
    };

    // h2 進來的請求一律以 HTTP/1.1 轉給上游（:authority → Host）
    *req.version_mut() = Version::HTTP_11;
    let h = req.headers_mut();
    if !h.contains_key(HOST) {
        if let Ok(v) = HeaderValue::from_str(&host) {
            h.insert(HOST, v);
        }
    }
    if let Ok(v) = HeaderValue::from_str(&peer.ip().to_string()) {
        h.append("x-forwarded-for", v);
    }
    if let Ok(v) = HeaderValue::from_str(&host) {
        h.insert("x-forwarded-host", v);
    }
    h.insert("x-forwarded-proto", HeaderValue::from_static(if https { "https" } else { "http" }));

    if up.node.is_some() || is_upgrade(&req) {
        return Ok(forward_conn(req, &up, &p.tunnels).await);
    }

    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let Ok(uri) = format!("{}{path}", up.target).parse::<Uri>() else {
        return Ok(text(StatusCode::BAD_GATEWAY, "bad upstream".into()));
    };
    *req.uri_mut() = uri;
    match p.client.request(req).await {
        Ok(res) => Ok(res.map(|b| b.boxed())),
        Err(e) => {
            tracing::warn!(error = %e, upstream = %up.target, "upstream error");
            Ok(bad_gateway())
        }
    }
}

/// 308 到 `https://<host 去掉 port><path>`
fn redirect_https<B>(host: &str, req: &Request<B>) -> Response<Body> {
    let bare = host
        .rsplit_once(':')
        .map_or(host, |(h, port)| if port.parse::<u16>().is_ok() { h } else { host });
    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let mut r = text(StatusCode::PERMANENT_REDIRECT, String::new());
    if let Ok(v) = HeaderValue::from_str(&format!("https://{bare}{path}")) {
        r.headers_mut().insert(LOCATION, v);
    }
    r
}

fn is_upgrade<B>(req: &Request<B>) -> bool {
    req.headers().contains_key(UPGRADE)
        && req
            .headers()
            .get_all(CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")))
}

/// `http://host:port[/]` → `host:port`（沒寫 port 時補 80）
fn host_port(target: &str) -> String {
    let t = target.strip_prefix("http://").unwrap_or(target);
    let t = t.split('/').next().unwrap_or(t);
    if t.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        t.to_string()
    } else {
        format!("{t}:80")
    }
}

/// 自建單一 HTTP/1 連線轉發（經通道或直接 TCP），支援 Upgrade。
async fn forward_conn(mut req: Request<Incoming>, up: &Upstream, tunnels: &TunnelRegistry) -> Response<Body> {
    let target = host_port(&up.target);
    let io = match tunnels.dial(up.node.as_deref(), &target).await {
        Ok(io) => io,
        Err(e) => {
            tracing::warn!(error = %e, %target, node = ?up.node, "upstream dial failed");
            return bad_gateway();
        }
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake::<_, Incoming>(TokioIo::new(io)).await {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(error = %e, %target, "upstream handshake failed");
            return bad_gateway();
        }
    };
    tokio::spawn(async move {
        if let Err(e) = conn.with_upgrades().await {
            tracing::debug!(error = %e, "upstream connection closed");
        }
    });

    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    *req.uri_mut() = match path.parse() {
        Ok(u) => u,
        Err(_) => return text(StatusCode::BAD_REQUEST, "bad path".into()),
    };

    let upgrade = is_upgrade(&req);
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    let mut res = match sender.send_request(req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, %target, "upstream error");
            return bad_gateway();
        }
    };

    if res.status() == StatusCode::SWITCHING_PROTOCOLS {
        if let Some(client_upgrade) = client_upgrade {
            let upstream_upgrade = hyper::upgrade::on(&mut res);
            tokio::spawn(async move {
                match tokio::try_join!(client_upgrade, upstream_upgrade) {
                    Ok((c, u)) => {
                        let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(c), &mut TokioIo::new(u)).await;
                    }
                    Err(e) => tracing::debug!(error = %e, "upgrade failed"),
                }
            });
            let mut out = Response::new(Empty::new().map_err(|never| match never {}).boxed());
            *out.status_mut() = res.status();
            *out.headers_mut() = res.headers().clone();
            return out;
        }
    }
    res.map(|b| b.boxed())
}

fn bad_gateway() -> Response<Body> {
    text(StatusCode::BAD_GATEWAY, "App 沒有回應，請稍後再試。".into())
}

fn text(status: StatusCode, body: String) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(body)).map_err(|never| match never {}).boxed());
    *r.status_mut() = status;
    r
}

#[cfg(test)]
mod tests {
    use super::host_port;

    #[test]
    fn host_port_from_target() {
        assert_eq!(host_port("http://127.0.0.1:3000"), "127.0.0.1:3000");
        assert_eq!(host_port("http://web:8080/"), "web:8080");
        assert_eq!(host_port("http://web"), "web:80");
        assert_eq!(host_port("10.0.0.2:5000"), "10.0.0.2:5000");
    }
}
