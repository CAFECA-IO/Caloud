use arc_swap::ArcSwap;
use caloud_core::model::Route;
use std::collections::HashMap;
use std::sync::Arc;

/// 轉發目標
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// HTTP：`http://host:port`；TLS 直通：`host:port`
    pub target: String,
    /// 經此 Runner 節點的通道（FR-OPS-09）
    pub node: Option<String>,
    /// 專案環境的執行中 commit：回應帶 `X-Caloud-Commit`（ADR 0023）
    pub commit: Option<String>,
}

/// hostname → upstream，無鎖讀取
#[derive(Default, Clone)]
pub struct RouteTable {
    inner: Arc<ArcSwap<HashMap<String, Upstream>>>,
    /// TLS 直通：hostname → host:port（FR-DOM-09）
    passthrough: Arc<ArcSwap<HashMap<String, Upstream>>>,
    /// ACME HTTP-01：token → key authorization
    acme: Arc<ArcSwap<HashMap<String, String>>>,
    changed: Arc<tokio::sync::Notify>,
    /// 系統路由（例：管理介面網域，ADR 0017）：每次 `replace` 都一併載入
    fixed: Arc<ArcSwap<Vec<Route>>>,
}

impl RouteTable {
    /// HTTP 轉發目標的 base URL
    pub fn lookup(&self, host: &str) -> Option<String> {
        self.lookup_route(host).map(|u| u.target)
    }

    pub fn lookup_route(&self, host: &str) -> Option<Upstream> {
        self.inner.load().get(&normalize(host)).cloned()
    }

    /// FR-DOM-09：SNI 對應的直通目標
    pub fn lookup_passthrough(&self, sni: &str) -> Option<String> {
        self.lookup_passthrough_route(sni).map(|u| u.target)
    }

    pub fn lookup_passthrough_route(&self, sni: &str) -> Option<Upstream> {
        self.passthrough.load().get(&normalize(sni)).cloned()
    }

    /// 設定系統路由；之後的 `replace`／`reload` 都會包含
    pub fn set_fixed(&self, routes: Vec<Route>) {
        self.fixed.store(Arc::new(routes));
    }

    pub fn replace(&self, mut routes: Vec<Route>) {
        routes.extend(self.fixed.load().iter().cloned());
        let pass: HashMap<_, _> = routes
            .iter()
            .filter_map(|r| {
                r.passthrough.clone().map(|p| {
                    (
                        r.hostname.clone(),
                        Upstream {
                            target: p,
                            node: r.node.clone(),
                            commit: None,
                        },
                    )
                })
            })
            .collect();
        let map: HashMap<_, _> = routes
            .into_iter()
            .map(|r| {
                (
                    r.hostname,
                    Upstream {
                        target: r.upstream,
                        node: r.node,
                        commit: r.commit,
                    },
                )
            })
            .collect();
        tracing::info!(count = map.len(), passthrough = pass.len(), "route table reloaded");
        self.inner.store(Arc::new(map));
        self.passthrough.store(Arc::new(pass));
        self.changed.notify_one();
    }

    /// 從資料庫重新載入（網域、App 狀態變動後呼叫）
    pub async fn reload(&self, db: &caloud_core::db::Db) -> anyhow::Result<()> {
        self.replace(caloud_core::db::load_routes(db).await?);
        Ok(())
    }

    pub fn acme_challenge(&self, token: &str) -> Option<String> {
        self.acme.load().get(token).cloned()
    }

    pub fn set_acme_challenge(&self, token: String, key_auth: String) {
        let mut m = (**self.acme.load()).clone();
        m.insert(token, key_auth);
        self.acme.store(Arc::new(m));
    }

    pub fn remove_acme_challenge(&self, token: &str) {
        let mut m = (**self.acme.load()).clone();
        m.remove(token);
        self.acme.store(Arc::new(m));
    }

    /// 路由表變動通知（憑證管理據此重新檢查，ADR 0016）
    pub fn changed(&self) -> Arc<tokio::sync::Notify> {
        self.changed.clone()
    }
}

fn normalize(host: &str) -> String {
    host.split(':').next().unwrap_or(host).trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-DOM-04
    #[test]
    fn lookup_normalizes_host() {
        let t = RouteTable::default();
        t.replace(vec![Route {
            hostname: "app.example.com".into(),
            upstream: "http://x:80".into(),
            passthrough: Some("relay:443".into()),
            node: Some("n1".into()),
            commit: None,
        }]);
        assert_eq!(t.lookup("App.Example.com:443").as_deref(), Some("http://x:80"));
        assert_eq!(t.lookup("app.example.com.").as_deref(), Some("http://x:80"));
        assert!(t.lookup("other.example.com").is_none());
        assert_eq!(t.lookup_passthrough("APP.example.com").as_deref(), Some("relay:443"));
        assert_eq!(t.lookup_route("app.example.com").unwrap().node.as_deref(), Some("n1"));
    }

    // ADR 0017：系統路由（管理介面網域）在每次重新載入後都保留
    #[test]
    fn fixed_routes_survive_reload() {
        let t = RouteTable::default();
        t.set_fixed(vec![Route {
            hostname: "caloud.cafeca.io".into(),
            upstream: "http://127.0.0.1:8080".into(),
            passthrough: None,
            node: None,
            commit: None,
        }]);
        t.replace(vec![]);
        assert_eq!(t.lookup("caloud.cafeca.io").as_deref(), Some("http://127.0.0.1:8080"));
        t.replace(vec![Route {
            hostname: "a.example.com".into(),
            upstream: "http://a:80".into(),
            passthrough: None,
            node: None,
            commit: None,
        }]);
        assert!(t.lookup("caloud.cafeca.io").is_some() && t.lookup("a.example.com").is_some());
    }
}
