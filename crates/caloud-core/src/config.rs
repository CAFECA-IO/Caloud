use serde::Deserialize;
use std::path::PathBuf;

/// 由環境變數載入（前綴 `CALOUD_`），見 README「設定」。
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// 管理介面的 origin，必須與瀏覽器網址列完全相同（CAFECA 簽章綁定此網域）
    pub public_origin: String,
    /// 管理介面 / API 監聽位址
    pub api_listen: String,
    /// 反向代理 HTTP 監聽位址（ACME HTTP-01 與 301 轉 HTTPS）
    pub http_listen: String,
    /// 反向代理 HTTPS 監聽位址
    pub https_listen: String,
    /// CAFECA 錢包網址，用來讀取 /.well-known/cafeca-configuration
    pub cafeca_wallet: String,
    /// 覆寫 RPC（建議自架節點）
    pub cafeca_rpc: Option<String>,
    /// 資料目錄：SQLite、compose 檔、憑證
    pub data_dir: PathBuf,
    /// ACME 聯絡信箱
    pub acme_email: Option<String>,
    /// 是否使用 Let's Encrypt staging
    pub acme_staging: bool,
    /// 所有被代理容器要加入的 docker 網路
    pub edge_network: String,
    /// 第一位以外的新使用者預設角色（pending 需 super admin 核准）
    pub default_role: String,
    /// docker CLI 路徑
    pub docker_bin: String,
    /// 網域 TXT 驗證用的 DNS-over-HTTPS JSON 端點
    pub doh_url: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let get = |k: &str, d: &str| std::env::var(format!("CALOUD_{k}")).unwrap_or_else(|_| d.to_string());
        let opt = |k: &str| std::env::var(format!("CALOUD_{k}")).ok().filter(|s| !s.is_empty());
        Ok(Self {
            public_origin: get("PUBLIC_ORIGIN", "http://localhost:8080"),
            api_listen: get("API_LISTEN", "0.0.0.0:8080"),
            http_listen: get("HTTP_LISTEN", "0.0.0.0:80"),
            https_listen: get("HTTPS_LISTEN", "0.0.0.0:443"),
            cafeca_wallet: get("CAFECA_WALLET", "https://cafeca.io"),
            cafeca_rpc: opt("CAFECA_RPC"),
            data_dir: get("DATA_DIR", "./data").into(),
            acme_email: opt("ACME_EMAIL"),
            acme_staging: get("ACME_STAGING", "false") == "true",
            edge_network: get("EDGE_NETWORK", "caloud-edge"),
            default_role: get("DEFAULT_ROLE", "pending"),
            docker_bin: get("DOCKER_BIN", "docker"),
            doh_url: get("DOH_URL", "https://cloudflare-dns.com/dns-query"),
        })
    }
}
