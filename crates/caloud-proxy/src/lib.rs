//! 內建反向代理：依 Host header 把流量轉到對應容器。
//!
//! - 路由表存在 [`RouteTable`]（`ArcSwap`），API 改動網域或 App 狀態後呼叫 `reload` 即時生效，不中斷連線
//! - `:80` 依 Host 轉發；Caloud 自己的 ACME HTTP-01 token 優先，其餘交給 App
//! - `:443` 依 SNI 做 TLS 直通（[`passthrough`]，ADR 0009）；Caloud 自己終結 TLS 為 M2（[`tls`]）
#![forbid(unsafe_code)]
pub mod passthrough;
pub mod routes;
pub mod server;
pub mod tls;

pub use passthrough::serve_tls_passthrough;
pub use routes::RouteTable;
pub use server::serve_http;
