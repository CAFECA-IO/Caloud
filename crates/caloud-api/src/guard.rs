//! 主控台對外的基本防護（M2-07，NFR-SEC-01，[ADR 0017](../../../docs/adr/0017-github-app-public-console.md)）：
//!
//! - `/api/**` 寫入請求的 `Origin` 必須等於 public origin（webhook 除外）
//! - 依來源 IP 的速率限制（固定 60 秒視窗，記憶體）
//! - 安全標頭；HTML 另加 CSP
//!
//! 來源 IP：只有從本機（Edge）進來的請求才信任 `X-Forwarded-For` 的最後一個值；
//! 本機直接連線且沒有 `X-Forwarded-For`（SSH 通道、測試）不受速率限制。
use crate::AppState;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use caloud_core::now;
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

const WINDOW: i64 = 60;

/// 速率類別與每分鐘上限
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    Auth,
    Webhook,
    Write,
}

impl Class {
    fn limit(self) -> u32 {
        match self {
            Class::Auth => 20,
            Class::Webhook => 300,
            Class::Write => 120,
        }
    }
}

#[derive(Default)]
pub struct Limiter {
    buckets: Mutex<HashMap<(IpAddr, Class), (i64, u32)>>,
}

impl Limiter {
    /// 允許時回 None；超過時回需要等待的秒數
    pub fn hit(&self, ip: IpAddr, class: Class, t: i64) -> Option<i64> {
        let window = t - t.rem_euclid(WINDOW);
        let mut m = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 10_000 {
            m.retain(|_, (w, _)| *w == window);
        }
        let e = m.entry((ip, class)).or_insert((window, 0));
        if e.0 != window {
            *e = (window, 0);
        }
        e.1 += 1;
        (e.1 > class.limit()).then(|| window + WINDOW - t)
    }
}

pub fn classify(method: &Method, path: &str) -> Option<Class> {
    if path == "/api/auth/nonce" || path == "/api/auth/login" {
        return Some(Class::Auth);
    }
    if path == "/api/github/webhook" {
        return Some(Class::Webhook);
    }
    let write = matches!(*method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE);
    (write && path.starts_with("/api/")).then_some(Class::Write)
}

/// 實際來源 IP；None 表示本機直接連線（不限速）
pub fn client_ip(peer: Option<SocketAddr>, xff: Option<&str>) -> Option<IpAddr> {
    let local = peer.is_none_or(|p| p.ip().is_loopback());
    if !local {
        return peer.map(|p| p.ip());
    }
    xff?.rsplit(',').next()?.trim().parse().ok()
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

pub async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let write = matches!(method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE);

    // Origin（CSRF）：webhook 由 HMAC 保護
    if write && path.starts_with("/api/") && path != "/api/github/webhook" {
        if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            if origin != s.cfg.public_origin {
                return json_error(StatusCode::FORBIDDEN, "不允許的 Origin。");
            }
        }
    }

    // 速率限制
    if let Some(class) = classify(&method, &path) {
        let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
        let xff = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok());
        if let Some(ip) = client_ip(peer, xff) {
            if let Some(wait) = s.limiter.hit(ip, class, now()) {
                let mut r = json_error(StatusCode::TOO_MANY_REQUESTS, "請求太頻繁，請稍後再試。");
                if let Ok(v) = HeaderValue::from_str(&wait.max(1).to_string()) {
                    r.headers_mut().insert(header::RETRY_AFTER, v);
                }
                return r;
            }
        }
    }

    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // ADR 0023：建置識別
    h.insert("x-caloud-build", HeaderValue::from_static(caloud_core::build::COMMIT));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if s.cfg.public_origin.starts_with("https://") {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    let html = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/html"));
    if html {
        let wallet = s.cfg.cafeca_wallet.trim_end_matches('/');
        let csp = format!(
            "default-src 'self'; script-src 'self' 'unsafe-inline' {wallet}; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: https:; connect-src 'self' https:; frame-src {wallet}; frame-ancestors 'none'; \
             base-uri 'none'; form-action 'self'"
        );
        if let Ok(v) = HeaderValue::from_str(&csp) {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    // NFR-SEC-01：固定視窗、各類別分開計數
    #[test]
    fn limits_per_ip_and_class() {
        let l = Limiter::default();
        let ip: IpAddr = "203.0.113.5".parse().unwrap();
        let t = 1_000_040; // 視窗起點 1_000_020
        for _ in 0..20 {
            assert_eq!(l.hit(ip, Class::Auth, t), None);
        }
        assert_eq!(l.hit(ip, Class::Auth, t), Some(40));
        assert_eq!(l.hit(ip, Class::Write, t), None);
        assert_eq!(l.hit("203.0.113.6".parse().unwrap(), Class::Auth, t), None);
        assert_eq!(l.hit(ip, Class::Auth, t + 60), None, "下一個視窗重新計數");
    }

    #[test]
    fn classifies_and_finds_client_ip() {
        assert_eq!(classify(&Method::POST, "/api/auth/login"), Some(Class::Auth));
        assert_eq!(classify(&Method::GET, "/api/auth/nonce"), Some(Class::Auth));
        assert_eq!(classify(&Method::POST, "/api/github/webhook"), Some(Class::Webhook));
        assert_eq!(classify(&Method::DELETE, "/api/apps/x"), Some(Class::Write));
        assert_eq!(classify(&Method::GET, "/api/apps"), None);
        assert_eq!(classify(&Method::POST, "/mcp"), None);

        let edge: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let remote: SocketAddr = "198.51.100.7:5000".parse().unwrap();
        // 經 Edge：取最後一個（Edge 附加的）值；用戶自帶的偽造值在前面
        assert_eq!(
            client_ip(Some(edge), Some("1.1.1.1, 203.0.113.9")),
            Some("203.0.113.9".parse().unwrap())
        );
        assert_eq!(client_ip(Some(edge), None), None, "本機直接連線不限速");
        // 不是本機：不信任 X-Forwarded-For
        assert_eq!(client_ip(Some(remote), Some("10.0.0.1")), Some(remote.ip()));
    }
}
