use serde::Deserialize;
use std::path::PathBuf;

/// 由環境變數載入（前綴 `CALOUD_`），見 README「設定」。
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// 管理介面的 origin，必須與瀏覽器網址列完全相同（CAFECA 簽章綁定此網域）
    pub public_origin: String,
    /// 管理介面 / API 監聽位址
    pub api_listen: String,
    /// 反向代理 HTTP 監聽位址（ACME HTTP-01 與 308 轉 HTTPS）
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
    /// 覆寫 ACME directory（測試用 Pebble 等，ADR 0016）
    pub acme_directory: Option<String>,
    /// 覆寫 ACME 伺服器的 CA（PEM 檔；測試用）
    pub acme_ca_file: Option<PathBuf>,
    /// 所有被代理容器要加入的 docker 網路
    pub edge_network: String,
    /// 第一位以外的新使用者預設角色（pending 需 super admin 核准）
    pub default_role: String,
    /// docker CLI 路徑
    pub docker_bin: String,
    /// 網域 TXT 驗證用的 DNS-over-HTTPS JSON 端點
    pub doh_url: String,
    /// git CLI 路徑（Git 來源 App，ADR 0009）
    pub git_bin: String,
    /// Cloudflare API 端點（ADR 0016；測試時指向 mock）
    pub cloudflare_api: String,
    /// GitHub API 與網站（ADR 0017；測試時指向 mock）
    pub github_api: String,
    pub github_web: String,
    /// 部署後健康檢查的等待秒數（ADR 0018）；0 表示不檢查（測試用）
    pub deploy_health_secs: u64,
}

impl Config {
    /// 管理介面的 host（不含 scheme 與 port），作為 Edge 的系統路由（ADR 0017）
    pub fn public_host(&self) -> Option<String> {
        let rest = self.public_origin.split_once("://")?.1;
        let host = rest.split(['/', ':']).next()?.to_ascii_lowercase();
        (!host.is_empty() && host != "localhost" && host.parse::<std::net::IpAddr>().is_err()).then_some(host)
    }

    /// API 監聽的 port
    pub fn api_port(&self) -> Option<u16> {
        self.api_listen.rsplit(':').next()?.parse().ok()
    }

    /// Caloud 自己佔用的主機連接埠（HTTP、HTTPS、API），特權 App 的 `ports` 會移除這些（FR-APP-16）
    pub fn reserved_ports(&self) -> Vec<u16> {
        [&self.http_listen, &self.https_listen, &self.api_listen]
            .iter()
            .filter_map(|a| a.rsplit(':').next().and_then(|p| p.parse().ok()))
            .collect()
    }

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
            acme_directory: opt("ACME_DIRECTORY"),
            acme_ca_file: opt("ACME_CA_FILE").map(PathBuf::from),
            edge_network: get("EDGE_NETWORK", "caloud-edge"),
            default_role: get("DEFAULT_ROLE", "pending"),
            docker_bin: get("DOCKER_BIN", "docker"),
            doh_url: get("DOH_URL", "https://cloudflare-dns.com/dns-query"),
            git_bin: get("GIT_BIN", "git"),
            cloudflare_api: get("CLOUDFLARE_API", "https://api.cloudflare.com/client/v4"),
            github_api: get("GITHUB_API", "https://api.github.com"),
            github_web: get("GITHUB_WEB", "https://github.com"),
            deploy_health_secs: get("DEPLOY_HEALTH_SECS", "60").parse().unwrap_or(60),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    // ADR 0017：只有網域形式的 public origin 會成為 Edge 的系統路由
    #[test]
    fn public_host_and_api_port() {
        let mut c = Config::from_env().unwrap();
        for (origin, want) in [
            ("https://Caloud.CAFECA.io", Some("caloud.cafeca.io")),
            ("https://caloud.cafeca.io:8443/x", Some("caloud.cafeca.io")),
            ("http://localhost:8080", None),
            ("http://127.0.0.1:3000", None),
        ] {
            c.public_origin = origin.into();
            assert_eq!(c.public_host().as_deref(), want, "{origin}");
        }
        c.api_listen = "0.0.0.0:8080".into();
        assert_eq!(c.api_port(), Some(8080));
    }
}
