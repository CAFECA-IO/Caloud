//! 秘密變數（FR-ENV-05、FR-APP-11，[ADR 0019](../../../../docs/adr/0019-project-vars.md)）。
//!
//! - 依環境分開，以主金鑰加密保存（AAD `project_var:<專案>:<環境>:<名稱>`），只寫不讀；
//! - 平臺產生的變數（`CALOUD_DB_PASSWORD`）第一次部署時產生，不可修改；
//! - 每次變更 `projects.vars_rev` 加 1，部署時記錄到環境，用來提示「重新部署後生效」。
use super::projects::owned;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_analyzer::{Analysis, Evidence, Provider};
use caloud_core::agent::EnvValue;
use caloud_core::db::audit;
use caloud_core::now;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 目前只有 production（staging 在 M7-02）
pub const ENVS: &[&str] = &["production"];
/// 值的長度上限
pub const MAX_VALUE: usize = 32 * 1024;
/// 每個環境的變數上限
pub const MAX_VARS: i64 = 200;
/// 平臺產生的資料庫密碼（解析器在 compose 中以 `${CALOUD_DB_PASSWORD}` 引用）
pub const DB_PASSWORD: &str = "CALOUD_DB_PASSWORD";
const PURPOSE: &str = "project_var";

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::BadRequest(m.into())
}

#[derive(Debug, Clone, Serialize)]
pub struct VarItem {
    pub name: String,
    pub set: bool,
    pub hint: Option<String>,
    /// `user` | `generated`
    pub source: &'static str,
    /// 確認的解析需要用戶提供
    pub needed: bool,
    pub build_time: bool,
    pub evidence: Vec<Evidence>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct VarList {
    pub env: String,
    pub rev: i64,
    pub items: Vec<VarItem>,
}

#[derive(Debug, Deserialize)]
pub struct VarsInput {
    #[serde(default = "default_env")]
    pub env: String,
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    #[serde(default)]
    pub unset: Vec<String>,
}

fn default_env() -> String {
    "production".into()
}

#[derive(sqlx::FromRow)]
struct Row {
    name: String,
    value_enc: String,
    hint: Option<String>,
    generated: bool,
    updated_at: i64,
}

fn check_env(env: &str) -> ApiResult<()> {
    ENVS.contains(&env).then_some(()).ok_or_else(|| bad("env 需為 production。"))
}

pub fn valid_name(n: &str) -> bool {
    let mut c = n.chars();
    n.len() <= 128 && c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// 值 ≥ 12 字元才給末 4 碼提示
fn hint_for(v: &str) -> Option<String> {
    (v.chars().count() >= 12).then(|| caloud_core::secret::hint(v))
}

fn aad_id(project: &str, env: &str, name: &str) -> String {
    format!("{project}:{env}:{name}")
}

/// 確認的解析
async fn confirmed(s: &AppState, project: &str) -> ApiResult<Option<Analysis>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT a.result FROM analyses a JOIN projects p ON p.confirmed_analysis = a.id WHERE p.id = ?")
            .bind(project)
            .fetch_optional(&s.db)
            .await?;
    Ok(row.and_then(|(r,)| serde_json::from_str(&r).ok()))
}

async fn rows(s: &AppState, project: &str, env: &str) -> ApiResult<Vec<Row>> {
    Ok(
        sqlx::query_as("SELECT name, value_enc, hint, generated, updated_at FROM project_vars WHERE project = ? AND env = ? ORDER BY name")
            .bind(project)
            .bind(env)
            .fetch_all(&s.db)
            .await?,
    )
}

pub(crate) async fn rev(s: &AppState, project: &str) -> ApiResult<i64> {
    Ok(sqlx::query_scalar("SELECT vars_rev FROM projects WHERE id = ?")
        .bind(project)
        .fetch_one(&s.db)
        .await?)
}

/// 解析需要的用戶變數＋已設定＋平臺產生
async fn items(s: &AppState, project: &str, env: &str) -> ApiResult<Vec<VarItem>> {
    let mut out: BTreeMap<String, VarItem> = BTreeMap::new();
    if let Some(a) = confirmed(s, project).await? {
        for e in a.env.into_iter().filter(|e| e.provider == Provider::User) {
            out.insert(
                e.name.clone(),
                VarItem {
                    name: e.name,
                    set: false,
                    hint: None,
                    source: "user",
                    needed: true,
                    build_time: e.build_time,
                    evidence: e.evidence,
                    updated_at: None,
                },
            );
        }
    }
    for r in rows(s, project, env).await? {
        let it = out.entry(r.name.clone()).or_insert_with(|| VarItem {
            name: r.name.clone(),
            set: false,
            hint: None,
            source: "user",
            needed: false,
            build_time: false,
            evidence: vec![],
            updated_at: None,
        });
        it.set = true;
        it.hint = r.hint;
        it.updated_at = Some(r.updated_at);
        if r.generated {
            it.source = "generated";
        }
    }
    Ok(out.into_values().collect())
}

/// `GET /api/projects/{id}/vars`
pub async fn list(s: &AppState, me: &CurrentUser, id: &str, env: &str) -> ApiResult<VarList> {
    owned(s, me, id).await?;
    check_env(env)?;
    Ok(VarList {
        env: env.to_string(),
        rev: rev(s, id).await?,
        items: items(s, id, env).await?,
    })
}

/// `PUT /api/projects/{id}/vars`
pub async fn update(s: &AppState, me: &CurrentUser, id: &str, input: VarsInput) -> ApiResult<VarList> {
    owned(s, me, id).await?;
    check_env(&input.env)?;
    let env = input.env.as_str();
    if input.set.is_empty() && input.unset.is_empty() {
        return list(s, me, id, env).await;
    }
    for name in input.set.keys().chain(input.unset.iter()) {
        if !valid_name(name) {
            return Err(bad(format!("變數名稱 `{name}` 只能用英數與底線，且不可以數字開頭。")));
        }
        if name.starts_with("CALOUD_") {
            return Err(bad(format!("`{name}`：CALOUD_ 開頭的名稱保留給平臺。")));
        }
    }
    for (name, v) in &input.set {
        if v.len() > MAX_VALUE {
            return Err(bad(format!("`{name}` 的值超過 32 KB。")));
        }
    }
    let key = s
        .master_key
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("伺服器未設定主金鑰 CALOUD_MASTER_KEY，無法保存秘密變數。".into()))?;
    let existing = rows(s, id, env).await?;
    if existing
        .iter()
        .any(|r| r.generated && (input.set.contains_key(&r.name) || input.unset.contains(&r.name)))
    {
        return Err(bad("平臺產生的變數不可修改。"));
    }
    let new_names = input.set.keys().filter(|n| !existing.iter().any(|r| &r.name == *n)).count() as i64;
    if existing.len() as i64 + new_names > MAX_VARS {
        return Err(bad(format!("每個環境最多 {MAX_VARS} 個變數。")));
    }
    let t = now();
    let mut tx = s.db.begin().await?;
    for (name, value) in &input.set {
        let enc = key
            .seal(PURPOSE, &aad_id(id, env, name), value.as_bytes())
            .map_err(ApiError::Internal)?;
        sqlx::query(
            "INSERT INTO project_vars (project, env, name, value_enc, hint, generated, updated_by, updated_at) VALUES (?, ?, ?, ?, ?, 0, ?, ?)
             ON CONFLICT(project, env, name) DO UPDATE SET value_enc = excluded.value_enc, hint = excluded.hint,
             updated_by = excluded.updated_by, updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(env)
        .bind(name)
        .bind(enc)
        .bind(hint_for(value))
        .bind(&me.0.account)
        .bind(t)
        .execute(&mut *tx)
        .await?;
    }
    for name in &input.unset {
        sqlx::query("DELETE FROM project_vars WHERE project = ? AND env = ? AND name = ? AND generated = 0")
            .bind(id)
            .bind(env)
            .bind(name)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE projects SET vars_rev = vars_rev + 1 WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // 稽核只記名稱（ADR 0019）
    let mut detail = Vec::new();
    if !input.set.is_empty() {
        detail.push(format!("set {}", input.set.keys().cloned().collect::<Vec<_>>().join(",")));
    }
    if !input.unset.is_empty() {
        detail.push(format!("unset {}", input.unset.join(",")));
    }
    audit(
        &s.db,
        Some(&me.0.account),
        "project_vars",
        Some(id),
        me.audit_detail(Some(&format!("{env}: {}", detail.join("; ")))).as_deref(),
    )
    .await;
    list(s, me, id, env).await
}

/// 部署用：解密這個環境的變數；compose 用到資料庫密碼且尚未產生時產生一個
pub(crate) async fn for_deploy(s: &AppState, project: &str, env: &str, compose: &str) -> ApiResult<Vec<EnvValue>> {
    let mut list = rows(s, project, env).await?;
    let needs_db = compose.contains(&format!("${{{DB_PASSWORD}}}")) || compose.contains(&format!("${DB_PASSWORD}"));
    if list.is_empty() && !needs_db {
        return Ok(vec![]);
    }
    let key = s
        .master_key
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("伺服器未設定主金鑰 CALOUD_MASTER_KEY，無法使用秘密變數。".into()))?;
    if needs_db && !list.iter().any(|r| r.name == DB_PASSWORD) {
        let pw: String = caloud_auth::new_token()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(32)
            .collect();
        let enc = key
            .seal(PURPOSE, &aad_id(project, env, DB_PASSWORD), pw.as_bytes())
            .map_err(ApiError::Internal)?;
        sqlx::query(
            "INSERT OR IGNORE INTO project_vars (project, env, name, value_enc, hint, generated, updated_by, updated_at) VALUES (?, ?, ?, ?, NULL, 1, NULL, ?)",
        )
        .bind(project)
        .bind(env)
        .bind(DB_PASSWORD)
        .bind(enc)
        .bind(now())
        .execute(&s.db)
        .await?;
        list = rows(s, project, env).await?;
    }
    list.into_iter()
        .map(|r| {
            let plain = key
                .open(PURPOSE, &aad_id(project, env, &r.name), &r.value_enc)
                .map_err(|e| ApiError::Conflict(format!("無法解密變數 `{}`：{e}；請重新設定。", r.name)))?;
            Ok(EnvValue {
                value: String::from_utf8(plain).map_err(|_| ApiError::Conflict(format!("變數 `{}` 不是有效的文字。", r.name)))?,
                user: !r.generated,
                name: r.name,
            })
        })
        .collect()
}

/// 驗證用（ADR 0020）：production 的用戶變數；資料庫密碼每次重新產生，不寫入資料庫
/// `user`：是否帶入用戶變數（PR 驗證不帶，ADR 0021）
pub(crate) async fn for_verify(s: &AppState, project: &str, compose: &str, user: bool) -> ApiResult<Vec<EnvValue>> {
    let list: Vec<Row> = if user {
        rows(s, project, "production").await?.into_iter().filter(|r| !r.generated).collect()
    } else {
        vec![]
    };
    let mut out = Vec::new();
    if !list.is_empty() {
        let key = s
            .master_key
            .as_ref()
            .ok_or_else(|| ApiError::Conflict("伺服器未設定主金鑰 CALOUD_MASTER_KEY，無法使用秘密變數。".into()))?;
        for r in list {
            let plain = key
                .open(PURPOSE, &aad_id(project, "production", &r.name), &r.value_enc)
                .map_err(|e| ApiError::Conflict(format!("無法解密變數 `{}`：{e}；請重新設定。", r.name)))?;
            out.push(EnvValue {
                value: String::from_utf8(plain).map_err(|_| ApiError::Conflict(format!("變數 `{}` 不是有效的文字。", r.name)))?,
                user: true,
                name: r.name,
            });
        }
    }
    if compose.contains(&format!("${{{DB_PASSWORD}}}")) || compose.contains(&format!("${DB_PASSWORD}")) {
        out.push(EnvValue {
            name: DB_PASSWORD.into(),
            value: caloud_auth::new_token()
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(32)
                .collect(),
            user: false,
        });
    }
    Ok(out)
}

/// 部署卡片：需要但未設定的用戶變數
pub(crate) async fn missing(s: &AppState, project: &str, env: &str) -> ApiResult<Vec<String>> {
    Ok(items(s, project, env)
        .await?
        .into_iter()
        .filter(|i| i.needed && !i.set)
        .map(|i| i.name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_hints() {
        assert!(valid_name("SESSION_SECRET"));
        assert!(valid_name("_x1"));
        assert!(!valid_name("1A"));
        assert!(!valid_name("A-B"));
        assert!(!valid_name(""));
        assert!(!valid_name("A B"));
        assert_eq!(hint_for("short"), None);
        assert_eq!(hint_for("abcdefghijkLMNO").as_deref(), Some("…LMNO"));
    }
}
