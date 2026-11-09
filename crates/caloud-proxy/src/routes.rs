use arc_swap::ArcSwap;
use caloud_core::model::Route;
use std::collections::HashMap;
use std::sync::Arc;

/// hostname → upstream base URL，無鎖讀取
#[derive(Default, Clone)]
pub struct RouteTable {
    inner: Arc<ArcSwap<HashMap<String, String>>>,
    /// ACME HTTP-01：token → key authorization
    acme: Arc<ArcSwap<HashMap<String, String>>>,
}

impl RouteTable {
    pub fn lookup(&self, host: &str) -> Option<String> {
        let host = host.split(':').next().unwrap_or(host).trim_end_matches('.').to_ascii_lowercase();
        self.inner.load().get(&host).cloned()
    }

    pub fn replace(&self, routes: Vec<Route>) {
        let map: HashMap<_, _> = routes.into_iter().map(|r| (r.hostname, r.upstream)).collect();
        tracing::info!(count = map.len(), "route table reloaded");
        self.inner.store(Arc::new(map));
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

    pub fn snapshot(&self) -> Vec<(String, String)> {
        self.inner.load().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
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
        }]);
        assert_eq!(t.lookup("App.Example.com:443").as_deref(), Some("http://x:80"));
        assert_eq!(t.lookup("app.example.com.").as_deref(), Some("http://x:80"));
        assert!(t.lookup("other.example.com").is_none());
    }
}
