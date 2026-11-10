//! 管理介面 HTTP API（axum）與內嵌的 Web UI。
#![forbid(unsafe_code)]
mod admin;
mod agent;
mod apps;
mod assets;
mod auth;
mod domains;
mod error;
mod mcp;
pub mod service;
mod tokens;
mod users;

use axum::routing::{delete, get, post, put};
use axum::Router;
use caloud_auth::CafecaVerifier;
use caloud_core::{config::Config, db::Db};
use caloud_orchestrator::ComposeRunner;
use caloud_proxy::RouteTable;
use std::sync::Arc;

pub use agent::{spawn_offline_sweeper, AgentHub};
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
    /// 已連線的 Runner agent（ADR 0011）
    pub agents: AgentHub,
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
        .route("/api/domains/{hostname}", delete(domains::remove))
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
        .route("/api/tokens/{id}", delete(tokens::remove))
        .route("/mcp", post(mcp::post).get(mcp::method_not_allowed).delete(mcp::method_not_allowed))
        // 後台（ADR 0010）
        .route("/api/admin/domain-pools", get(admin::list_pools).post(admin::create_pool))
        .route("/api/admin/domain-pools/{root}", put(admin::update_pool).delete(admin::delete_pool))
        .route("/api/admin/reserved-names", get(admin::list_reserved).post(admin::add_reserved))
        .route("/api/admin/reserved-names/{name}", delete(admin::remove_reserved))
        .route("/api/admin/blocked-hosts", get(admin::list_blocked).post(admin::block_host))
        .route("/api/admin/blocked-hosts/{hostname}", delete(admin::unblock_host))
        .route("/api/admin/subdomain-ids", post(admin::subdomain_id))
        .route("/api/admin/nodes", get(admin::list_nodes))
        .route("/api/admin/nodes/join-tokens", post(admin::join_token))
        .route("/api/admin/nodes/{id}", delete(admin::delete_node))
        .route("/api/admin/nodes/{id}/drain", post(admin::drain_node))
        .route("/api/admin/nodes/{id}/activate", post(admin::activate_node))
        // Runner agent（ADR 0011）
        .route(caloud_core::agent::CONNECT_PATH, get(agent::connect))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}
