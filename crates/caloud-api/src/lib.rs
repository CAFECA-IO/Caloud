//! 管理介面 HTTP API（axum）與內嵌的 Web UI。
#![forbid(unsafe_code)]
mod apps;
mod assets;
mod auth;
mod domains;
mod error;
mod mcp;
pub mod service;
mod tokens;
mod users;

use axum::routing::{get, post, put};
use axum::Router;
use caloud_auth::CafecaVerifier;
use caloud_core::{config::Config, db::Db};
use caloud_orchestrator::ComposeRunner;
use caloud_proxy::RouteTable;
use std::sync::Arc;

pub use auth::CurrentUser;
pub use error::ApiError;
pub use mcp::PROTOCOL_VERSIONS as MCP_PROTOCOL_VERSIONS;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub verifier: Arc<CafecaVerifier>,
    pub runner: ComposeRunner,
    pub routes: RouteTable,
    pub http: reqwest::Client,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(assets::index))
        .route("/styleguide", get(assets::styleguide))
        .route("/assets/{*path}", get(assets::serve))
        // 登入
        .route("/api/auth/config", get(auth::config))
        .route("/api/auth/nonce", post(auth::nonce))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        // 使用者管理（admin）
        .route("/api/users", get(users::list))
        .route("/api/users/{account}", put(users::set_role))
        // 網域
        .route("/api/domains", get(domains::list).post(domains::create))
        .route("/api/domains/{hostname}", axum::routing::delete(domains::remove))
        .route("/api/domains/{hostname}/verify", post(domains::verify))
        .route("/api/domains/{hostname}/bind", put(domains::bind))
        // App（docker compose）
        .route("/api/apps", get(apps::list).post(apps::create))
        .route("/api/apps/{id}", get(apps::get).put(apps::update).delete(apps::remove))
        .route("/api/apps/{id}/deploy", post(apps::deploy))
        .route("/api/apps/{id}/stop", post(apps::stop))
        .route("/api/apps/{id}/logs", get(apps::logs))
        // API 權杖與 MCP（ADR 0008）
        .route("/api/tokens", get(tokens::list).post(tokens::create))
        .route("/api/tokens/{id}", axum::routing::delete(tokens::remove))
        .route("/mcp", post(mcp::post).get(mcp::method_not_allowed).delete(mcp::method_not_allowed))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
