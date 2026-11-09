//! Caloud 共用元件：設定、資料庫、資料模型。
#![forbid(unsafe_code)]
pub mod config;
pub mod db;
pub mod model;

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// compose project name：`caloud-<app id 前 12 碼>`
pub fn project_name(app_id: &str) -> String {
    format!("caloud-{}", &app_id.replace('-', "")[..12])
}

/// 被代理容器在 edge 網路上的 DNS alias：`<project>-<service>`
pub fn model_upstream_host(app_id: &str, service: &str) -> String {
    format!("{}-{}", project_name(app_id), service)
}
