//! TLS 終結與 ACME 自動簽發（Milestone 2）。
//!
//! 規劃：
//! - 以 `rustls` + `tokio-rustls` 在 `:443` 接受連線，實作 `ResolvesServerCert` 依 SNI 從 [`CertStore`] 取憑證
//! - 以 `instant-acme` 對 Let's Encrypt 下單，HTTP-01 challenge 交給 `RouteTable::set_acme_challenge`
//! - 憑證與私鑰存放 `<data_dir>/certs/<hostname>/{cert.pem,key.pem}`，到期前 30 天自動續簽
//! - 網域需先通過 DNS TXT 驗證（`_caloud-challenge.<hostname>`）才會申請憑證，避免替他人網域簽發
//! - 管理介面本身（`CALOUD_PUBLIC_ORIGIN` 的 host）也經由同一套機制取得憑證

use std::path::PathBuf;

/// 憑證存放位置
pub struct CertStore {
    pub dir: PathBuf,
}

impl CertStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn cert_path(&self, hostname: &str) -> PathBuf {
        self.dir.join(hostname).join("cert.pem")
    }

    pub fn key_path(&self, hostname: &str) -> PathBuf {
        self.dir.join(hostname).join("key.pem")
    }
}
