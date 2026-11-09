//! Caloud：單一執行檔，同時提供
//! - 管理介面與 API（`CALOUD_API_LISTEN`，預設 :8080）
//! - 依網域轉發到容器的反向代理（`CALOUD_HTTP_LISTEN` :80，M2 加上 `CALOUD_HTTPS_LISTEN` :443）
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
    let runner = ComposeRunner::new(cfg.data_dir.join("apps"), cfg.edge_network.clone(), cfg.docker_bin.clone());
    if let Err(e) = runner.ensure_edge_network().await {
        tracing::error!(error = %e, "docker unavailable; deployments will fail until it is reachable");
    }

    let routes = RouteTable::default();
    routes.reload(&db).await?;

    let state = AppState {
        verifier: Arc::new(CafecaVerifier::new(cfg.cafeca_wallet.clone(), cfg.cafeca_rpc.clone())),
        runner,
        routes: routes.clone(),
        db,
        http: reqwest::Client::new(),
        cfg: cfg.clone(),
    };

    let api = tokio::net::TcpListener::bind(&cfg.api_listen).await?;
    tracing::info!(addr = %cfg.api_listen, "api listening");
    let api = axum::serve(api, caloud_api::router(state));
    let proxy = caloud_proxy::serve_http(cfg.http_listen.parse()?, routes);

    tokio::select! {
        r = async { api.await } => r?,
        r = proxy => r?,
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
    Ok(())
}
