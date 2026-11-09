//! 內建反向代理：依 Host header 把流量轉到對應容器。
//!
//! - 路由表存在 [`RouteTable`]（`ArcSwap`），API 改動網域或 App 狀態後呼叫 `reload` 即時生效，不中斷連線
//! - `:80` 處理 ACME HTTP-01 challenge，其餘請求在有憑證時 301 到 HTTPS（M2）
//! - `:443` 以 SNI 選憑證（M2，見 [`tls`]）
#![forbid(unsafe_code)]
pub mod routes;
pub mod server;
pub mod tls;

pub use routes::RouteTable;
pub use server::serve_http;
