//! Caloud：單一執行檔，同時提供
//! - 管理介面與 API（`CALOUD_API_LISTEN`，預設 :8080）
//! - 依網域轉發到容器的反向代理（`CALOUD_HTTP_LISTEN` :80、`CALOUD_HTTPS_LISTEN` :443，ADR 0016）
#![forbid(unsafe_code)]
use caloud_api::AppState;
use caloud_auth::CafecaVerifier;
use caloud_core::config::Config;
use caloud_orchestrator::ComposeRunner;
use caloud_proxy::RouteTable;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,caloud=debug".into()))
        .init();

    let cfg = Arc::new(Config::from_env()?);
    tracing::info!(origin = %cfg.public_origin, wallet = %cfg.cafeca_wallet, "starting caloud");

    let db = caloud_core::db::connect(&cfg.data_dir).await?;
    let runner = ComposeRunner::new(
        cfg.data_dir.join("apps"),
        cfg.edge_network.clone(),
        cfg.docker_bin.clone(),
        cfg.git_bin.clone(),
    );
    if let Err(e) = runner.ensure_edge_network().await {
        tracing::error!(error = %e, "docker unavailable; deployments will fail until it is reachable");
    }

    let routes = RouteTable::default();
    // ADR 0017：管理介面網域轉到本機 API
    if let (Some(host), Some(port)) = (cfg.public_host(), cfg.api_port()) {
        tracing::info!(%host, "console served through edge");
        routes.set_fixed(vec![caloud_core::model::Route {
            hostname: host,
            upstream: format!("http://127.0.0.1:{port}"),
            passthrough: None,
            node: None,
            commit: None,
        }]);
    }
    routes.reload(&db).await?;

    let tunnels = caloud_proxy::TunnelRegistry::default();
    let certs = caloud_proxy::CertStore::default();
    let master_key = caloud_core::secret::MasterKey::from_env()?.map(Arc::new);
    if master_key.is_none() {
        tracing::warn!("CALOUD_MASTER_KEY is not set; secrets (Cloudflare tokens) cannot be stored");
    }
    let state = AppState {
        verifier: Arc::new(CafecaVerifier::new(cfg.cafeca_wallet.clone(), cfg.cafeca_rpc.clone())),
        runner,
        routes: routes.clone(),
        db,
        http: reqwest::Client::new(),
        cfg: cfg.clone(),
        agents: Default::default(),
        tunnels: tunnels.clone(),
        certs: certs.clone(),
        master_key,
        github: Default::default(),
        limiter: Default::default(),
    };
    caloud_api::spawn_offline_sweeper(state.clone());
    caloud_api::certs::spawn(state.clone());
    caloud_api::service::verify::spawn_sweeper(state.clone());

    let api = tokio::net::TcpListener::bind(&cfg.api_listen).await?;
    tracing::info!(addr = %cfg.api_listen, "api listening");
    let api = axum::serve(
        api,
        caloud_api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    let proxy = caloud_proxy::serve_http(cfg.http_listen.parse()?, routes.clone(), tunnels.clone(), certs.clone());
    let tls = caloud_proxy::serve_tls(cfg.https_listen.parse()?, routes, tunnels, certs);

    tokio::select! {
        r = async { api.await } => r?,
        r = proxy => r?,
        r = tls => r?,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}
