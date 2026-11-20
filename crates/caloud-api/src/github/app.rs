//! GitHub App 憑證（FR-GH-05）、App JWT 與 installation token（FR-GH-01，ADR 0017）。
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use caloud_core::db::audit;
use caloud_core::now;
use caloud_core::secret::hint;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::PrivateKeyDer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 主金鑰 AAD 的用途前綴
const PURPOSE: &str = "github_app";
const UA: &str = "Caloud";

/// `GET /api/admin/github-app`
#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_secret_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct Row {
    app_id: i64,
    slug: String,
    client_id: String,
    client_secret_enc: String,
    client_secret_hint: String,
    private_key_enc: String,
    private_key_hint: String,
    webhook_secret_enc: String,
    webhook_secret_hint: String,
    updated_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppInput {
    pub app_id: i64,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub private_key: Option<String>,
    pub webhook_secret: Option<String>,
}

/// 解開後的憑證（只在記憶體）
pub struct Creds {
    pub app_id: i64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: String,
    pub private_key: String,
    pub webhook_secret: String,
}

async fn row(s: &AppState) -> ApiResult<Option<Row>> {
    Ok(sqlx::query_as("SELECT * FROM github_app WHERE id = 1")
        .fetch_optional(&s.db)
        .await?)
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::BadRequest(m.into())
}

/// FR-GH-05
pub async fn info(s: &AppState, me: &CurrentUser) -> ApiResult<AppInfo> {
    me.require_admin()?;
    Ok(match row(s).await? {
        None => AppInfo {
            configured: false,
            app_id: None,
            slug: None,
            client_id: None,
            client_secret_hint: None,
            private_key_hint: None,
            webhook_secret_hint: None,
            updated_at: None,
        },
        Some(r) => AppInfo {
            configured: true,
            app_id: Some(r.app_id),
            slug: Some(r.slug),
            client_id: Some(r.client_id),
            client_secret_hint: Some(r.client_secret_hint),
            private_key_hint: Some(r.private_key_hint),
            webhook_secret_hint: Some(r.webhook_secret_hint),
            updated_at: Some(r.updated_at),
        },
    })
}

/// FR-GH-05：設定或更新；以私鑰簽 JWT 呼叫 `GET /app` 確認相符並取得 slug
pub async fn set(s: &AppState, me: &CurrentUser, input: AppInput) -> ApiResult<AppInfo> {
    me.require_admin()?;
    let key = s
        .master_key
        .as_ref()
        .ok_or_else(|| bad("伺服器未設定主金鑰 CALOUD_MASTER_KEY，無法保存 GitHub App 憑證。"))?;
    let client_id = input.client_id.trim().to_string();
    if input.app_id <= 0 || client_id.is_empty() || client_id.len() > 64 {
        return Err(bad("請提供 App ID 與 Client ID。"));
    }
    let old = row(s).await?;
    let open = |enc: &str, field: &str| -> ApiResult<String> {
        let b = key.open(PURPOSE, field, enc).map_err(ApiError::Internal)?;
        String::from_utf8(b).map_err(|e| ApiError::Internal(e.into()))
    };
    let pick = |new: &Option<String>, field: &str, old_enc: Option<&str>| -> ApiResult<String> {
        match new.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) => Ok(v.to_string()),
            None => match old_enc {
                Some(enc) => open(enc, field),
                None => Err(bad("首次設定需要 client secret、私鑰與 webhook secret。")),
            },
        }
    };
    let client_secret = pick(
        &input.client_secret,
        "client_secret",
        old.as_ref().map(|r| r.client_secret_enc.as_str()),
    )?;
    let private_key = pick(&input.private_key, "private_key", old.as_ref().map(|r| r.private_key_enc.as_str()))?;
    let webhook_secret = pick(
        &input.webhook_secret,
        "webhook_secret",
        old.as_ref().map(|r| r.webhook_secret_enc.as_str()),
    )?;
    if webhook_secret.len() < 16 {
        return Err(bad("webhook secret 至少 16 個字元。"));
    }
    let jwt = app_jwt(input.app_id, &private_key).map_err(|e| bad(format!("私鑰無效：{e}")))?;
    let app: Value = gh_get(s, "/app", &format!("Bearer {jwt}"))
        .await
        .map_err(|e| bad(format!("無法以此私鑰向 GitHub 驗證 App：{e}")))?;
    if app["id"].as_i64() != Some(input.app_id) {
        return Err(bad("私鑰與 App ID 不相符。"));
    }
    let slug = app["slug"].as_str().unwrap_or_default().to_string();
    if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(bad("GitHub 回傳的 App 資料不完整。"));
    }
    let seal = |field: &str, v: &str| key.seal(PURPOSE, field, v.as_bytes()).map_err(ApiError::Internal);
    sqlx::query(
        "INSERT INTO github_app (id, app_id, slug, client_id, client_secret_enc, client_secret_hint, private_key_enc, private_key_hint,
           webhook_secret_enc, webhook_secret_hint, updated_at) VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET app_id = excluded.app_id, slug = excluded.slug, client_id = excluded.client_id,
           client_secret_enc = excluded.client_secret_enc, client_secret_hint = excluded.client_secret_hint,
           private_key_enc = excluded.private_key_enc, private_key_hint = excluded.private_key_hint,
           webhook_secret_enc = excluded.webhook_secret_enc, webhook_secret_hint = excluded.webhook_secret_hint,
           updated_at = excluded.updated_at",
    )
    .bind(input.app_id)
    .bind(&slug)
    .bind(&client_id)
    .bind(seal("client_secret", &client_secret)?)
    .bind(hint(&client_secret))
    .bind(seal("private_key", &private_key)?)
    .bind(hint(
        private_key
            .trim_end()
            .trim_end_matches("-----END RSA PRIVATE KEY-----")
            .trim_end_matches("-----END PRIVATE KEY-----")
            .trim_end(),
    ))
    .bind(seal("webhook_secret", &webhook_secret)?)
    .bind(hint(&webhook_secret))
    .bind(now())
    .execute(&s.db)
    .await?;
    s.github.clear_tokens();
    audit(
        &s.db,
        Some(&me.0.account),
        "github_app_update",
        Some(&input.app_id.to_string()),
        me.audit_detail(None).as_deref(),
    )
    .await;
    info(s, me).await
}

/// 解開憑證；未設定回 None
pub async fn creds(s: &AppState) -> ApiResult<Option<Creds>> {
    let Some(r) = row(s).await? else { return Ok(None) };
    let key = s
        .master_key
        .as_ref()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("CALOUD_MASTER_KEY is not set")))?;
    let open = |enc: &str, field: &str| -> ApiResult<String> {
        let b = key.open(PURPOSE, field, enc).map_err(ApiError::Internal)?;
        String::from_utf8(b).map_err(|e| ApiError::Internal(e.into()))
    };
    Ok(Some(Creds {
        app_id: r.app_id,
        slug: r.slug,
        client_id: r.client_id,
        client_secret: open(&r.client_secret_enc, "client_secret")?,
        private_key: open(&r.private_key_enc, "private_key")?,
        webhook_secret: open(&r.webhook_secret_enc, "webhook_secret")?,
    }))
}

/// RS256 App JWT（iat 往前 60 秒、exp 9 分鐘），私鑰為 PKCS#1 或 PKCS#8 PEM
pub fn app_jwt(app_id: i64, private_key_pem: &str) -> anyhow::Result<String> {
    let key = match PrivateKeyDer::from_pem_slice(private_key_pem.as_bytes())? {
        PrivateKeyDer::Pkcs1(k) => RsaKeyPair::from_der(k.secret_pkcs1_der()),
        PrivateKeyDer::Pkcs8(k) => RsaKeyPair::from_pkcs8(k.secret_pkcs8_der()),
        _ => anyhow::bail!("需要 RSA 私鑰"),
    }
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let t = now();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(json!({ "iat": t - 60, "exp": t + 540, "iss": app_id.to_string() }).to_string());
    let msg = format!("{header}.{claims}");
    let mut sig = vec![0u8; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), msg.as_bytes(), &mut sig)
        .map_err(|_| anyhow::anyhow!("簽章失敗"))?;
    Ok(format!("{msg}.{}", URL_SAFE_NO_PAD.encode(sig)))
}

/// GitHub API 錯誤只帶狀態與訊息
async fn gh_send(req: reqwest::RequestBuilder) -> anyhow::Result<Value> {
    let res = req
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header("user-agent", UA)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("無法連線 GitHub：{}", e.without_url()))?;
    let st = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !st.is_success() {
        anyhow::bail!("GitHub 回應 {st}：{}", v["message"].as_str().unwrap_or("未知錯誤"));
    }
    Ok(v)
}

pub async fn gh_get(s: &AppState, path: &str, auth: &str) -> anyhow::Result<Value> {
    gh_send(s.http.get(format!("{}{path}", s.cfg.github_api)).header("authorization", auth)).await
}

pub async fn gh_post(s: &AppState, path: &str, auth: &str, body: Value) -> anyhow::Result<Value> {
    gh_send(
        s.http
            .post(format!("{}{path}", s.cfg.github_api))
            .header("authorization", auth)
            .json(&body),
    )
    .await
}

pub async fn gh_patch(s: &AppState, path: &str, auth: &str, body: Value) -> anyhow::Result<Value> {
    gh_send(
        s.http
            .patch(format!("{}{path}", s.cfg.github_api))
            .header("authorization", auth)
            .json(&body),
    )
    .await
}

/// installation token：記憶體快取到過期前 5 分鐘（ADR 0017）
pub async fn installation_token(s: &AppState, installation_id: i64) -> anyhow::Result<String> {
    if let Some(t) = s.github.cached_token(installation_id) {
        return Ok(t);
    }
    let c = creds(s)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .ok_or_else(|| anyhow::anyhow!("GitHub App 尚未設定"))?;
    let jwt = app_jwt(c.app_id, &c.private_key)?;
    let v = gh_post(
        s,
        &format!("/app/installations/{installation_id}/access_tokens"),
        &format!("Bearer {jwt}"),
        json!({}),
    )
    .await?;
    let token = v["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("GitHub 沒有回傳 installation token"))?
        .to_string();
    let exp = v["expires_at"]
        .as_str()
        .and_then(|e| time::OffsetDateTime::parse(e, &time::format_description::well_known::Rfc3339).ok())
        .map(|t| t.unix_timestamp())
        .unwrap_or(now() + 3600);
    s.github.store_token(installation_id, token.clone(), exp - 300);
    Ok(token)
}

/// git 的 `http.extraHeader` 值
pub fn git_auth_header(token: &str) -> String {
    format!("Authorization: Basic {}", STANDARD.encode(format!("x-access-token:{token}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-GH-01：RS256 JWT 可以用公鑰驗證，claims 正確
    #[test]
    fn signs_app_jwt() {
        let pem = include_str!("../../tests/fixtures/github-app-test.pem");
        let jwt = app_jwt(5826601, pem).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "5826601");
        assert!(claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap() <= 600);
        let pubkey = include_bytes!("../../tests/fixtures/github-app-test.pub.der");
        let sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, &pubkey[..])
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
        assert!(app_jwt(1, "not a key").is_err());
        assert_eq!(git_auth_header("t"), "Authorization: Basic eC1hY2Nlc3MtdG9rZW46dA==");
    }
}
