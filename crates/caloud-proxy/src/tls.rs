//! Edge TLS 終結（ADR 0016，FR-DOM-05、FR-OPS-10）。
//!
//! - [`CertStore`]：名稱 → 憑證，ArcSwap 無鎖讀取；簽發後即時替換，不中斷連線
//! - 依 SNI 選擇憑證：先找完全相符，再找上一層 wildcard（只配對一層）
//! - 憑證檔：`<dir>/<名稱>/{cert.pem,key.pem}`；`dir` 由呼叫端依 staging／production 分開
use arc_swap::ArcSwap;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 已載入的憑證
#[derive(Clone)]
pub struct LoadedCert {
    pub key: Arc<CertifiedKey>,
    /// 憑證的 SAN（小寫，wildcard 為 `*.x`）
    pub names: Vec<String>,
    /// 到期時間（unix 秒）
    pub not_after: i64,
}

/// 依 SNI 選擇憑證；clone 共用同一份
#[derive(Clone, Default)]
pub struct CertStore {
    inner: Arc<ArcSwap<HashMap<String, LoadedCert>>>,
}

impl std::fmt::Debug for CertStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertStore").field("names", &self.inner.load().len()).finish()
    }
}

impl CertStore {
    /// 先完全相符，再找上一層 wildcard（`a.caloud.tw` → `*.caloud.tw`；`caloud.tw` 不配 `*.caloud.tw`）
    pub fn resolve(&self, host: &str) -> Option<LoadedCert> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let map = self.inner.load();
        if let Some(c) = map.get(&host) {
            return Some(c.clone());
        }
        let (_, parent) = host.split_once('.')?;
        if !parent.contains('.') {
            return None;
        }
        map.get(&format!("*.{parent}")).cloned()
    }

    /// `:80` 轉 HTTPS 判斷（host 可帶 port）
    pub fn has_cert(&self, host: &str) -> bool {
        let host = host
            .rsplit_once(':')
            .map_or(host, |(h, p)| if p.parse::<u16>().is_ok() { h } else { host });
        self.resolve(host).is_some()
    }

    /// 以憑證的每個 SAN 加入（取代同名舊憑證）
    pub fn insert(&self, cert: LoadedCert) {
        let mut m = (**self.inner.load()).clone();
        for n in &cert.names {
            m.insert(n.clone(), cert.clone());
        }
        self.inner.store(Arc::new(m));
    }

    /// 移除這些名稱
    pub fn remove(&self, names: &[String]) {
        let mut m = (**self.inner.load()).clone();
        for n in names {
            m.remove(&n.to_ascii_lowercase());
        }
        self.inner.store(Arc::new(m));
    }

    /// 載入 `dir/<名稱>/{cert.pem,key.pem}`；壞掉或過期的略過並記錄
    pub fn load_dir(&self, dir: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        let mut n = 0;
        for e in entries.flatten() {
            let p = e.path();
            match load_pair(&p.join("cert.pem"), &p.join("key.pem")) {
                Ok(c) if c.not_after > caloud_core::now() => {
                    self.insert(c);
                    n += 1;
                }
                Ok(_) => tracing::warn!(path = %p.display(), "certificate expired; skipped"),
                Err(e) => tracing::warn!(path = %p.display(), error = %e, "certificate not loaded"),
            }
        }
        tracing::info!(dir = %dir.display(), count = n, "certificates loaded");
        n
    }
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        CertStore::resolve(self, hello.server_name()?).map(|c| c.key)
    }
}

fn load_pair(cert: &Path, key: &Path) -> anyhow::Result<LoadedCert> {
    parse_pem(&std::fs::read_to_string(cert)?, &std::fs::read_to_string(key)?)
}

/// 由 PEM（憑證鏈＋私鑰）建立 [`LoadedCert`]
pub fn parse_pem(chain_pem: &str, key_pem: &str) -> anyhow::Result<LoadedCert> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(chain_pem.as_bytes()).collect::<Result<_, _>>()?;
    let leaf = chain.first().ok_or_else(|| anyhow::anyhow!("PEM 中沒有憑證"))?;
    let (names, not_after) = leaf_info(leaf)?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())?;
    let signer = rustls::crypto::ring::sign::any_supported_type(&key)?;
    Ok(LoadedCert {
        key: Arc::new(CertifiedKey::new(chain, signer)),
        names,
        not_after,
    })
}

/// 葉憑證的 SAN 與到期時間
fn leaf_info(der: &CertificateDer<'_>) -> anyhow::Result<(Vec<String>, i64)> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(|e| anyhow::anyhow!("無法解析憑證：{e}"))?;
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for n in &san.value.general_names {
            if let x509_parser::extensions::GeneralName::DNSName(d) = n {
                names.push(d.to_ascii_lowercase());
            }
        }
    }
    if names.is_empty() {
        anyhow::bail!("憑證沒有 DNS SAN");
    }
    Ok((names, cert.validity().not_after.timestamp()))
}

/// 憑證檔目錄：`<data_dir>/certs/<staging|production>`
pub fn cert_dir(data_dir: &Path, staging: bool) -> PathBuf {
    data_dir.join("certs").join(if staging { "staging" } else { "production" })
}

/// 以 [`CertStore`] 選憑證的 rustls 設定（ALPN：h2、http/1.1）
pub fn server_config(store: CertStore) -> Arc<ServerConfig> {
    let mut cfg = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring 支援預設 TLS 版本")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(store));
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(cfg)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 自簽憑證（測試用）
    pub(crate) fn self_signed(names: &[&str]) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    // FR-OPS-10：完全相符優先；wildcard 只配一層、不配根網域
    #[test]
    fn resolves_exact_then_one_level_wildcard() {
        let store = CertStore::default();
        let (c, k) = self_signed(&["caloud.tw", "*.caloud.tw"]);
        store.insert(parse_pem(&c, &k).unwrap());
        let (c2, k2) = self_signed(&["shop.example.com"]);
        let shop = parse_pem(&c2, &k2).unwrap();
        assert_eq!(shop.names, vec!["shop.example.com"]);
        assert!(shop.not_after > caloud_core::now());
        store.insert(shop);

        assert!(store.resolve("caloud.tw").is_some());
        assert!(store.resolve("Q7V2M4KX.caloud.tw.").is_some());
        assert!(store.resolve("a.b.caloud.tw").is_none());
        assert!(store.resolve("shop.example.com").is_some());
        assert!(store.resolve("example.com").is_none());
        assert!(store.resolve("x.example.com").is_none());
        assert!(store.has_cert("abc.caloud.tw:80"));
        assert!(!store.has_cert("relay.cafeca.io"));

        store.remove(&["*.caloud.tw".into()]);
        assert!(store.resolve("abc.caloud.tw").is_none());
        assert!(store.resolve("caloud.tw").is_some());
    }

    #[test]
    fn loads_cert_dirs() {
        let dir = std::env::temp_dir().join(format!("caloud-certs-{}", uuid_like()));
        let (c, k) = self_signed(&["a.example.com"]);
        std::fs::create_dir_all(dir.join("a.example.com")).unwrap();
        std::fs::write(dir.join("a.example.com/cert.pem"), c).unwrap();
        std::fs::write(dir.join("a.example.com/key.pem"), k).unwrap();
        std::fs::create_dir_all(dir.join("broken")).unwrap();
        std::fs::write(dir.join("broken/cert.pem"), "x").unwrap();
        let store = CertStore::default();
        assert_eq!(store.load_dir(&dir), 1);
        assert!(store.resolve("a.example.com").is_some());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(cert_dir(Path::new("/data"), true), Path::new("/data/certs/staging"));
    }

    fn uuid_like() -> String {
        format!("{}-{}", std::process::id(), caloud_core::now())
    }
}
