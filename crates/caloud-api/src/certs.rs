//! 憑證管理背景任務（ADR 0016，FR-OPS-10、FR-DOM-05）。
//!
//! - 網域池設有 Cloudflare 權杖 → `root`＋`*.root` 一張（DNS-01）
//! - 已驗證、非直通、綁定運作中 App 的網域 → 每個網域一張（HTTP-01）
//! - 每 6 小時掃描；路由表變動或網域池變更時也會觸發
//! - 到期前 30 天續簽；失敗退避 1 小時起、每次加倍、最多 24 小時；一次只處理一張
use crate::cloudflare::Cloudflare;
use crate::AppState;
use caloud_core::db::audit;
use caloud_core::now;
use caloud_proxy::tls::{cert_dir, parse_pem};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCAN_EVERY: Duration = Duration::from_secs(6 * 3600);
const RENEW_BEFORE: i64 = 30 * 86400;
const BACKOFF_MIN: i64 = 3600;
const BACKOFF_MAX: i64 = 86400;
/// 用途：Cloudflare 權杖的 AAD（`caloud_core::secret`）
pub const TOKEN_PURPOSE: &str = "cloudflare_token";

/// 需要的一張憑證
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub name: String,
    pub kind: Kind,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dns01,
    Http01,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Dns01 => "dns01",
            Kind::Http01 => "http01",
        }
    }
}

/// `GET /api/admin/certificates`
#[derive(Debug, Serialize)]
pub struct Certificate {
    pub name: String,
    pub kind: String,
    pub names: Vec<String>,
    pub status: String,
    pub not_after: Option<i64>,
    pub error: Option<String>,
    #[serde(skip)]
    pub attempts: i64,
    pub next_attempt: Option<i64>,
    pub staging: bool,
    pub updated_at: i64,
}

/// 啟動：載入既有憑證檔，背景掃描
pub fn spawn(s: AppState) {
    s.certs.load_dir(&cert_dir(&s.cfg.data_dir, s.cfg.acme_staging));
    let changed = s.routes.changed();
    tokio::spawn(async move {
        loop {
            if let Err(e) = scan(&s).await {
                tracing::warn!(error = %e, "certificate scan failed");
            }
            tokio::select! {
                _ = tokio::time::sleep(SCAN_EVERY) => {}
                _ = changed.notified() => {
                    // 合併短時間內的多次變動
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });
}

/// 網域池權杖設定後等，要求立即重新檢查
pub fn kick(s: &AppState) {
    s.routes.changed().notify_one();
}

/// 目前應該有的憑證
pub async fn wanted(s: &AppState) -> anyhow::Result<Vec<Want>> {
    let mut out = Vec::new();
    let pools: Vec<(String,)> = sqlx::query_as(
        "SELECT root FROM domain_pools WHERE cloudflare_token_enc IS NOT NULL AND cloudflare_zone_id IS NOT NULL ORDER BY root",
    )
    .fetch_all(&s.db)
    .await?;
    for (root,) in pools {
        out.push(Want {
            names: vec![root.clone(), format!("*.{root}")],
            name: root,
            kind: Kind::Dns01,
        });
    }
    let domains: Vec<(String,)> = sqlx::query_as(
        "SELECT d.hostname FROM domains d JOIN apps a ON a.id = d.app_id
         WHERE d.verified = 1 AND d.tls_mode = 'none' AND a.status = 'running' AND d.service IS NOT NULL ORDER BY d.hostname",
    )
    .fetch_all(&s.db)
    .await?;
    for (host,) in domains {
        // 已由網域池 wildcard 涵蓋的不另外申請
        if out.iter().any(|w| covers(&w.names, &host)) {
            continue;
        }
        out.push(Want {
            name: host.clone(),
            kind: Kind::Http01,
            names: vec![host],
        });
    }
    Ok(out)
}

fn covers(names: &[String], host: &str) -> bool {
    names.iter().any(|n| {
        n == host
            || n.strip_prefix("*.")
                .is_some_and(|parent| host.split_once('.').is_some_and(|(_, rest)| rest == parent))
    })
}

#[derive(sqlx::FromRow)]
struct CertRow {
    name: String,
    kind: String,
    names: String,
    status: String,
    not_after: Option<i64>,
    error: Option<String>,
    attempts: i64,
    next_attempt: Option<i64>,
    staging: bool,
    updated_at: i64,
}

pub async fn list(s: &AppState) -> anyhow::Result<Vec<Certificate>> {
    let rows: Vec<CertRow> = sqlx::query_as("SELECT * FROM certificates ORDER BY name").fetch_all(&s.db).await?;
    Ok(rows
        .into_iter()
        .map(|r| Certificate {
            names: serde_json::from_str(&r.names).unwrap_or_default(),
            name: r.name,
            kind: r.kind,
            status: r.status,
            not_after: r.not_after,
            error: r.error,
            attempts: r.attempts,
            next_attempt: r.next_attempt,
            staging: r.staging,
            updated_at: r.updated_at,
        })
        .collect())
}

/// 掃描一次並處理需要簽發的憑證（背景任務與測試共用）
pub async fn scan(s: &AppState) -> anyhow::Result<()> {
    let wants = wanted(s).await?;
    let rows = list(s).await?;
    // 不再需要的憑證：移除紀錄、記憶體與檔案
    for r in rows.iter().filter(|r| !wants.iter().any(|w| w.name == r.name)) {
        sqlx::query("DELETE FROM certificates WHERE name = ?")
            .bind(&r.name)
            .execute(&s.db)
            .await?;
        s.certs.remove(&r.names);
        let _ = std::fs::remove_dir_all(cert_dir(&s.cfg.data_dir, s.cfg.acme_staging).join(&r.name));
        tracing::info!(name = %r.name, "certificate no longer needed; removed");
    }
    let Some(email) = s.cfg.acme_email.clone() else {
        if !wants.is_empty() {
            tracing::warn!("CALOUD_ACME_EMAIL is not set; certificates will not be issued");
        }
        return Ok(());
    };
    let t = now();
    for w in &wants {
        let row = rows.iter().find(|r| r.name == w.name);
        if !needs_issue(s, w, row, t) {
            continue;
        }
        if row.and_then(|r| r.next_attempt).is_some_and(|n| n > t) {
            continue;
        }
        upsert(s, w, "pending", None, None, row.map_or(0, |r| r.attempts), None).await?;
        set_domain_status(s, w, "pending").await;
        match issue(s, w, &email).await {
            Ok(not_after) => {
                upsert(s, w, "issued", Some(not_after), None, 0, None).await?;
                set_domain_status(s, w, "issued").await;
                audit(&s.db, None, "certificate_issue", Some(&w.name), Some(&w.names.join(","))).await;
                tracing::info!(name = %w.name, "certificate issued");
            }
            Err(e) => {
                let attempts = row.map_or(0, |r| r.attempts) + 1;
                let wait = (BACKOFF_MIN << (attempts - 1).min(5)).min(BACKOFF_MAX);
                let msg = format!("{e:#}");
                upsert(s, w, "failed", None, Some(&msg), attempts, Some(now() + wait)).await?;
                set_domain_status(s, w, "failed").await;
                audit(&s.db, None, "certificate_fail", Some(&w.name), Some(&msg)).await;
                tracing::warn!(name = %w.name, error = %msg, retry_in = wait, "certificate issuance failed");
            }
        }
    }
    Ok(())
}

/// 沒有紀錄、名稱改變、不是 issued、換了 staging／production、快到期或記憶體裡沒有 → 要簽
fn needs_issue(s: &AppState, w: &Want, row: Option<&Certificate>, t: i64) -> bool {
    let Some(r) = row else { return true };
    r.names != w.names
        || r.status != "issued"
        || r.staging != s.cfg.acme_staging
        || r.not_after.is_none_or(|n| n - t < RENEW_BEFORE)
        || w.names.iter().any(|n| s.certs.resolve(n.trim_start_matches("*.")).is_none())
}

async fn upsert(
    s: &AppState,
    w: &Want,
    status: &str,
    not_after: Option<i64>,
    error: Option<&str>,
    attempts: i64,
    next_attempt: Option<i64>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO certificates (name, kind, names, status, not_after, error, attempts, next_attempt, staging, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(name) DO UPDATE SET kind = excluded.kind, names = excluded.names, status = excluded.status,
           not_after = COALESCE(excluded.not_after, certificates.not_after), error = excluded.error,
           attempts = excluded.attempts, next_attempt = excluded.next_attempt, staging = excluded.staging,
           updated_at = excluded.updated_at",
    )
    .bind(&w.name)
    .bind(w.kind.as_str())
    .bind(serde_json::to_string(&w.names)?)
    .bind(status)
    .bind(not_after)
    .bind(error)
    .bind(attempts)
    .bind(next_attempt)
    .bind(s.cfg.acme_staging)
    .bind(now())
    .execute(&s.db)
    .await?;
    Ok(())
}

async fn set_domain_status(s: &AppState, w: &Want, status: &str) {
    if w.kind == Kind::Http01 {
        let _ = sqlx::query("UPDATE domains SET tls_status = ? WHERE hostname = ?")
            .bind(status)
            .bind(&w.name)
            .execute(&s.db)
            .await;
    }
}

// ───────────────────────── ACME ─────────────────────────

fn directory(s: &AppState) -> String {
    if let Some(d) = &s.cfg.acme_directory {
        return d.clone();
    }
    if s.cfg.acme_staging {
        LetsEncrypt::Staging.url().into()
    } else {
        LetsEncrypt::Production.url().into()
    }
}

fn builder(s: &AppState) -> anyhow::Result<instant_acme::AccountBuilder> {
    Ok(match &s.cfg.acme_ca_file {
        Some(ca) => Account::builder_with_root(ca)?,
        None => Account::builder()?,
    })
}

fn acme_dir(s: &AppState) -> PathBuf {
    s.cfg
        .data_dir
        .join("acme")
        .join(if s.cfg.acme_staging { "staging" } else { "production" })
}

/// 讀取或建立 ACME 帳號（金鑰檔權限 0600）
async fn account(s: &AppState, email: &str) -> anyhow::Result<Account> {
    let path = acme_dir(s).join("account.json");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let creds: AccountCredentials = serde_json::from_str(&raw)?;
        return Ok(builder(s)?.from_credentials(creds).await?);
    }
    let contact = format!("mailto:{email}");
    let (account, creds) = builder(s)?
        .create(
            &NewAccount {
                contact: &[&contact],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory(s),
            None,
        )
        .await?;
    write_private(&path, &serde_json::to_string(&creds)?)?;
    Ok(account)
}

/// 寫檔，權限 0600
fn write_private(path: &Path, content: &str) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(&tmp)?.write_all(content.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 一個 challenge 的設定資料
struct Pending {
    /// 不含 `*.` 的網域
    domain: String,
    token: String,
    key_auth: String,
    dns_value: String,
}

/// 已設定的 challenge 回應，結束時清除
#[derive(Default)]
struct Cleanup {
    txt: Vec<(String, String)>, // (zone id, record id)
    tokens: Vec<String>,
}

async fn issue(s: &AppState, w: &Want, email: &str) -> anyhow::Result<i64> {
    let account = account(s, email).await?;
    let ids: Vec<Identifier> = w.names.iter().map(|n| Identifier::Dns(n.clone())).collect();
    let mut order = account.new_order(&NewOrder::new(&ids)).await?;
    let ty = match w.kind {
        Kind::Dns01 => ChallengeType::Dns01,
        Kind::Http01 => ChallengeType::Http01,
    };

    let mut pending = Vec::new();
    {
        let mut auths = order.authorizations();
        while let Some(a) = auths.next().await {
            let mut a = a?;
            match a.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                other => anyhow::bail!("授權狀態 {other:?}"),
            }
            let ch = a
                .challenge(ty.clone())
                .ok_or_else(|| anyhow::anyhow!("ACME 伺服器沒有提供 {ty:?}"))?;
            let ka = ch.key_authorization();
            let domain = match &ch.identifier().identifier {
                Identifier::Dns(d) => d.clone(),
                other => anyhow::bail!("不支援的識別 {other:?}"),
            };
            pending.push(Pending {
                domain,
                token: ch.token.clone(),
                key_auth: ka.as_str().to_string(),
                dns_value: ka.dns_value(),
            });
        }
    }

    let mut cleanup = Cleanup::default();
    let r = respond_and_finalize(s, w, &mut order, &pending, &mut cleanup).await;
    // 不論成功與否都清除 challenge 回應
    for t in &cleanup.tokens {
        s.routes.remove_acme_challenge(t);
    }
    if !cleanup.txt.is_empty() {
        if let Ok((token, _)) = pool_token(s, &w.name).await {
            let cf = Cloudflare {
                http: &s.http,
                base: &s.cfg.cloudflare_api,
                token: &token,
            };
            for (zone, id) in &cleanup.txt {
                if let Err(e) = cf.delete_record(zone, id).await {
                    tracing::warn!(error = %e, record = %id, "failed to delete ACME TXT record");
                }
            }
        }
    }
    let (chain, key) = r?;

    let cert = parse_pem(&chain, &key)?;
    let not_after = cert.not_after;
    let dir = cert_dir(&s.cfg.data_dir, s.cfg.acme_staging).join(&w.name);
    write_private(&dir.join("cert.pem"), &chain)?;
    write_private(&dir.join("key.pem"), &key)?;
    s.certs.insert(cert);
    Ok(not_after)
}

/// 設定 challenge 回應 → set_ready → 等待 → finalize，回傳（憑證鏈, 私鑰）
async fn respond_and_finalize(
    s: &AppState,
    w: &Want,
    order: &mut instant_acme::Order,
    pending: &[Pending],
    cleanup: &mut Cleanup,
) -> anyhow::Result<(String, String)> {
    match w.kind {
        Kind::Http01 => {
            for p in pending {
                s.routes.set_acme_challenge(p.token.clone(), p.key_auth.clone());
                cleanup.tokens.push(p.token.clone());
            }
        }
        Kind::Dns01 if !pending.is_empty() => {
            let (token, zone_id) = pool_token(s, &w.name).await?;
            let cf = Cloudflare {
                http: &s.http,
                base: &s.cfg.cloudflare_api,
                token: &token,
            };
            for p in pending {
                let name = format!("_acme-challenge.{}", p.domain);
                let id = cf.create_txt(&zone_id, &name, &p.dns_value).await?;
                cleanup.txt.push((zone_id.clone(), id));
            }
            wait_txt(s, pending).await?;
        }
        Kind::Dns01 => {}
    }

    let ty = match w.kind {
        Kind::Dns01 => ChallengeType::Dns01,
        Kind::Http01 => ChallengeType::Http01,
    };
    {
        let mut auths = order.authorizations();
        while let Some(a) = auths.next().await {
            let mut a = a?;
            if a.status != AuthorizationStatus::Pending {
                continue;
            }
            if let Some(mut ch) = a.challenge(ty.clone()) {
                ch.set_ready().await?;
            }
        }
    }

    let retry = RetryPolicy::new()
        .initial_delay(Duration::from_secs(2))
        .timeout(Duration::from_secs(180));
    let status = order.poll_ready(&retry).await?;
    if status != OrderStatus::Ready {
        anyhow::bail!("ACME 驗證未通過（{status:?}）：{}", challenge_errors(order).await);
    }
    let key = order.finalize().await?;
    let chain = order.poll_certificate(&retry).await?;
    Ok((chain, key))
}

/// 盡量取得各 challenge 的錯誤訊息
async fn challenge_errors(order: &mut instant_acme::Order) -> String {
    let mut out = Vec::new();
    let mut auths = order.authorizations();
    while let Some(Ok(mut a)) = auths.next().await {
        if let Ok(st) = a.refresh().await {
            for c in &st.challenges {
                if let Some(e) = &c.error {
                    out.push(e.to_string());
                }
            }
        }
    }
    if out.is_empty() {
        "沒有詳細錯誤".into()
    } else {
        out.join("；")
    }
}

/// 以 DoH 確認 TXT 已可查到（最多 3 分鐘）
async fn wait_txt(s: &AppState, pending: &[Pending]) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let mut all = true;
        for p in pending {
            let name = format!("_acme-challenge.{}", p.domain);
            if !txt_visible(s, &name, &p.dns_value).await {
                all = false;
                break;
            }
        }
        if all {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("3 分鐘內查不到 _acme-challenge TXT 記錄");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn txt_visible(s: &AppState, name: &str, value: &str) -> bool {
    let Ok(res) = s
        .http
        .get(&s.cfg.doh_url)
        .query(&[("name", name), ("type", "TXT")])
        .header("accept", "application/dns-json")
        .send()
        .await
    else {
        return false;
    };
    let Ok(v) = res.json::<serde_json::Value>().await else {
        return false;
    };
    v["Answer"]
        .as_array()
        .is_some_and(|a| a.iter().any(|r| r["data"].as_str().is_some_and(|t| t.trim_matches('"') == value)))
}

/// 解開網域池的 Cloudflare 權杖，回傳（權杖, zone id）
async fn pool_token(s: &AppState, root: &str) -> anyhow::Result<(String, String)> {
    let key = s
        .master_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("未設定 CALOUD_MASTER_KEY，無法解開 Cloudflare 權杖"))?;
    let (enc, zone): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT cloudflare_token_enc, cloudflare_zone_id FROM domain_pools WHERE root = ?")
            .bind(root)
            .fetch_optional(&s.db)
            .await?
            .ok_or_else(|| anyhow::anyhow!("網域池 {root} 不存在"))?;
    let (Some(enc), Some(zone)) = (enc, zone) else {
        anyhow::bail!("網域池 {root} 沒有設定 Cloudflare 權杖");
    };
    let token = String::from_utf8(key.open(TOKEN_PURPOSE, root, &enc)?)?;
    Ok((token, zone))
}

#[cfg(test)]
mod tests {
    use super::covers;

    #[test]
    fn wildcard_covers_one_level() {
        let names = vec!["caloud.tw".to_string(), "*.caloud.tw".to_string()];
        assert!(covers(&names, "caloud.tw"));
        assert!(covers(&names, "abc.caloud.tw"));
        assert!(!covers(&names, "a.b.caloud.tw"));
        assert!(!covers(&names, "shop.example.com"));
    }
}
