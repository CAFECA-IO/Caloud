use crate::RouteTable;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, HOST};
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::net::SocketAddr;
use tokio::net::TcpListener;

type Body = BoxBody<Bytes, hyper::Error>;

/// HTTP 反向代理（M1）。TLS 終結見 `tls` 模組（M2）。
pub async fn serve_http(addr: SocketAddr, routes: RouteTable) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let client: Client<HttpConnector, Incoming> = Client::builder(TokioExecutor::new()).build_http();
    tracing::info!(%addr, "proxy listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let routes = routes.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |req| handle(req, peer, routes.clone(), client.clone()));
            if let Err(e) = auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(stream), svc)
                .await
            {
                tracing::debug!(error = %e, "connection closed");
            }
        });
    }
}

async fn handle(
    mut req: Request<Incoming>,
    peer: SocketAddr,
    routes: RouteTable,
    client: Client<HttpConnector, Incoming>,
) -> Result<Response<Body>, hyper::Error> {
    // ACME HTTP-01
    if let Some(token) = req.uri().path().strip_prefix("/.well-known/acme-challenge/") {
        return Ok(match routes.acme_challenge(token) {
            Some(k) => text(StatusCode::OK, k),
            None => text(StatusCode::NOT_FOUND, "unknown challenge".into()),
        });
    }

    let host = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| req.uri().host())
        .unwrap_or_default()
        .to_string();
    let Some(upstream) = routes.lookup(&host) else {
        // FR-DOM-07
        return Ok(text(StatusCode::NOT_FOUND, format!("此網域尚未綁定運作中的 App：{host}")));
    };

    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let Ok(uri) = format!("{upstream}{path}").parse::<Uri>() else {
        return Ok(text(StatusCode::BAD_GATEWAY, "bad upstream".into()));
    };
    *req.uri_mut() = uri;
    let h = req.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&peer.ip().to_string()) {
        h.append("x-forwarded-for", v);
    }
    if let Ok(v) = HeaderValue::from_str(&host) {
        h.insert("x-forwarded-host", v);
    }
    h.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    // TODO(M2-04, FR-DOM-06): WebSocket / HTTP Upgrade 轉接（hyper::upgrade::on 兩端對接）

    match client.request(req).await {
        Ok(res) => Ok(res.map(|b| b.boxed())),
        Err(e) => {
            tracing::warn!(error = %e, %upstream, "upstream error");
            Ok(text(StatusCode::BAD_GATEWAY, "App 沒有回應，請稍後再試。".into()))
        }
    }
}

fn text(status: StatusCode, body: String) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(body)).map_err(|never| match never {}).boxed());
    *r.status_mut() = status;
    r
}
