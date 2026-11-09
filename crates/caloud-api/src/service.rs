//! 服務層：App、網域、使用者的業務操作（ADR 0008）。
//!
//! REST handler 與 MCP 工具都只能經由這裡操作資源；權限檢查、輸入驗證、稽核紀錄都在此完成，
//! 確保兩個入口的行為一致（docs/05-components.md §B）。
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::db::audit;
use caloud_core::model::{App, Domain, Role, User};
use caloud_core::now;
use caloud_orchestrator::ValidatedCompose;
use serde_json::{json, Value};

// ───────────────────────── App ─────────────────────────

/// FR-APP-01：admin+ 看全部，其餘只看自己的
pub async fn list_apps(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<App>> {
    me.require_deploy()?;
    let rows = if me.role().is_admin() {
        sqlx::query_as("SELECT * FROM apps ORDER BY created_at DESC")
            .fetch_all(&s.db)
            .await?
    } else {
        sqlx::query_as("SELECT * FROM apps WHERE owner = ? ORDER BY created_at DESC")
            .bind(&me.0.account)
            .fetch_all(&s.db)
            .await?
    };
    Ok(rows)
}

/// 取得 App 並確認為擁有者或 admin+
pub async fn owned_app(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<App> {
    me.require_deploy()?;
    let app: App = sqlx::query_as("SELECT * FROM apps WHERE id = ?")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    me.can_manage(&app.owner).then_some(app).ok_or(ApiError::Forbidden)
}

fn validate_compose(yaml: &str) -> ApiResult<ValidatedCompose> {
    caloud_orchestrator::validate(yaml).map_err(|e| ApiError::BadRequest(e.to_string()))
}

/// FR-APP-06：App、compose 原文、service 清單與容器狀態
pub async fn get_app(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Value> {
    let app = owned_app(s, me, id).await?;
    let containers = s.runner.status(id).await.unwrap_or_default();
    let services = caloud_orchestrator::validate(&app.compose_yaml)
        .map(|c| c.services)
        .unwrap_or_default();
    Ok(json!({ "app": app, "compose_yaml": app.compose_yaml, "services": services, "containers": containers }))
}

/// FR-APP-01～03：建立 App；`deploy` 為 true 時立即在背景部署
pub async fn create_app(s: &AppState, me: &CurrentUser, name: Option<String>, compose_yaml: &str, deploy: bool) -> ApiResult<App> {
    me.require_deploy()?;
    let compose = validate_compose(compose_yaml)?;
    let id = uuid::Uuid::new_v4().to_string();
    let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| compose.services[0].clone());
    let app: App = sqlx::query_as(
        "INSERT INTO apps (id, name, owner, compose_yaml, status, created_at, updated_at) VALUES (?, ?, ?, ?, 'stopped', ?, ?) RETURNING *",
    )
    .bind(&id)
    .bind(&name)
    .bind(&me.0.account)
    .bind(compose_yaml)
    .bind(now())
    .bind(now())
    .fetch_one(&s.db)
    .await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "app_create",
        Some(&id),
        me.audit_detail(Some(&name)).as_deref(),
    )
    .await;
    if deploy {
        spawn_deploy(s.clone(), app.clone(), compose);
    }
    Ok(app)
}

/// FR-APP-04：更新 compose；`deploy` 為 true 時立即重新部署
pub async fn update_app(
    s: &AppState,
    me: &CurrentUser,
    id: &str,
    name: Option<String>,
    compose_yaml: &str,
    deploy: bool,
) -> ApiResult<App> {
    owned_app(s, me, id).await?;
    let compose = validate_compose(compose_yaml)?;
    let app: App = sqlx::query_as("UPDATE apps SET compose_yaml = ?, name = COALESCE(?, name), updated_at = ? WHERE id = ? RETURNING *")
        .bind(compose_yaml)
        .bind(name.filter(|n| !n.trim().is_empty()))
        .bind(now())
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    audit(&s.db, Some(&me.0.account), "app_update", Some(id), me.audit_detail(None).as_deref()).await;
    if deploy {
        spawn_deploy(s.clone(), app.clone(), compose);
    }
    Ok(app)
}

/// FR-APP-03：重新部署（背景執行）
pub async fn deploy_app(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<()> {
    let app = owned_app(s, me, id).await?;
    let compose = validate_compose(&app.compose_yaml)?;
    audit(&s.db, Some(&me.0.account), "app_deploy", Some(id), me.audit_detail(None).as_deref()).await;
    spawn_deploy(s.clone(), app, compose);
    Ok(())
}

/// FR-APP-05
pub async fn stop_app(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<()> {
    owned_app(s, me, id).await?;
    s.runner.stop(id).await?;
    set_status(s, id, "stopped", None).await?;
    s.routes.reload(&s.db).await?;
    audit(&s.db, Some(&me.0.account), "app_stop", Some(id), me.audit_detail(None).as_deref()).await;
    Ok(())
}

/// FR-APP-05：預設保留 volume
pub async fn delete_app(s: &AppState, me: &CurrentUser, id: &str, purge_volumes: bool) -> ApiResult<()> {
    owned_app(s, me, id).await?;
    s.runner.remove(id, purge_volumes).await?;
    sqlx::query("DELETE FROM apps WHERE id = ?").bind(id).execute(&s.db).await?;
    s.routes.reload(&s.db).await?;
    let detail = purge_volumes.then_some("purge_volumes");
    audit(
        &s.db,
        Some(&me.0.account),
        "app_delete",
        Some(id),
        me.audit_detail(detail).as_deref(),
    )
    .await;
    Ok(())
}

/// FR-APP-06：tail 預設 200、上限 5000
pub async fn app_logs(s: &AppState, me: &CurrentUser, id: &str, service: Option<&str>, tail: Option<u32>) -> ApiResult<String> {
    owned_app(s, me, id).await?;
    Ok(s.runner.logs(id, service, tail.unwrap_or(200).min(5000)).await?)
}

async fn set_status(s: &AppState, id: &str, status: &str, err: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE apps SET status = ?, last_error = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(err)
        .bind(now())
        .bind(id)
        .execute(&s.db)
        .await?;
    Ok(())
}

/// FR-APP-03、FR-DOM-04：背景部署 deploying → running / failed，完成後即時更新路由表
/// TODO(M2-05, FR-APP-08)：每 App 部署互斥鎖
fn spawn_deploy(s: AppState, app: App, compose: ValidatedCompose) {
    tokio::spawn(async move {
        // 狀態寫入失敗只影響顯示，部署本身仍繼續；錯誤寫入 log
        if let Err(e) = set_status(&s, &app.id, "deploying", None).await {
            tracing::error!(error = %e, "set status failed");
        }
        let result = s.runner.deploy(&app.id, &compose).await;
        let (status, err) = match &result {
            Ok(project) => {
                tracing::info!(app = %app.id, %project, "deployed");
                ("running", None)
            }
            Err(e) => {
                tracing::warn!(app = %app.id, error = %e, "deploy failed");
                ("failed", Some(e.to_string()))
            }
        };
        if let Err(e) = set_status(&s, &app.id, status, err.as_deref()).await {
            tracing::error!(error = %e, "set status failed");
        }
        if let Err(e) = s.routes.reload(&s.db).await {
            tracing::error!(error = %e, "route reload failed");
        }
    });
}

// ───────────────────────── 網域 ─────────────────────────

/// FR-DOM-01：admin+ 看全部，其餘只看自己的
pub async fn list_domains(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<Domain>> {
    me.require_deploy()?;
    let rows = if me.role().is_admin() {
        sqlx::query_as("SELECT * FROM domains ORDER BY hostname").fetch_all(&s.db).await?
    } else {
        sqlx::query_as("SELECT * FROM domains WHERE owner = ? ORDER BY hostname")
            .bind(&me.0.account)
            .fetch_all(&s.db)
            .await?
    };
    Ok(rows)
}

/// FR-DOM-01：小寫、去結尾點、驗證 label 格式
pub fn normalize_hostname(h: &str) -> ApiResult<String> {
    let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
    let ok = h.len() <= 253
        && h.contains('.')
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    ok.then_some(h)
        .ok_or_else(|| ApiError::BadRequest("網域格式錯誤，請輸入完整主機名稱，例如 app.example.com。".into()))
}

/// 需要使用者設定的 DNS 紀錄（FR-DOM-02）
pub fn dns_records(d: &Domain) -> Value {
    json!([
        { "type": "A", "name": d.hostname, "value": "Caloud 主機的 IP" },
        { "type": "TXT", "name": format!("_caloud-challenge.{}", d.hostname), "value": d.verify_token },
    ])
}

/// FR-DOM-01、FR-DOM-02
pub async fn create_domain(s: &AppState, me: &CurrentUser, hostname: &str) -> ApiResult<Domain> {
    me.require_deploy()?;
    let hostname = normalize_hostname(hostname)?;
    let token = format!("caloud-{}", uuid::Uuid::new_v4().simple());
    let d: Domain = sqlx::query_as("INSERT INTO domains (hostname, owner, verify_token, created_at) VALUES (?, ?, ?, ?) RETURNING *")
        .bind(&hostname)
        .bind(&me.0.account)
        .bind(&token)
        .bind(now())
        .fetch_one(&s.db)
        .await
        .map_err(|_| ApiError::Conflict("網域已存在，請改用其他主機名稱。".into()))?;
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_create",
        Some(&hostname),
        me.audit_detail(None).as_deref(),
    )
    .await;
    Ok(d)
}

async fn owned_domain(s: &AppState, me: &CurrentUser, hostname: &str) -> ApiResult<Domain> {
    me.require_deploy()?;
    let hostname = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    let d: Domain = sqlx::query_as("SELECT * FROM domains WHERE hostname = ?")
        .bind(&hostname)
        .fetch_optional(&s.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    me.can_manage(&d.owner).then_some(d).ok_or(ApiError::Forbidden)
}

/// FR-DOM-02：透過 DNS-over-HTTPS 查詢 TXT 紀錄驗證擁有權
pub async fn verify_domain(s: &AppState, me: &CurrentUser, hostname: &str) -> ApiResult<Domain> {
    let d = owned_domain(s, me, hostname).await?;
    let name = format!("_caloud-challenge.{}", d.hostname);
    let v: Value = s
        .http
        .get(&s.cfg.doh_url)
        .query(&[("name", name.as_str()), ("type", "TXT")])
        .header("accept", "application/dns-json")
        .send()
        .await
        .map_err(anyhow::Error::from)?
        .json()
        .await
        .map_err(anyhow::Error::from)?;
    let found = v["Answer"].as_array().is_some_and(|a| {
        a.iter()
            .any(|r| r["data"].as_str().is_some_and(|t| t.trim_matches('"') == d.verify_token))
    });
    if !found {
        return Err(ApiError::BadRequest(format!(
            "找不到 TXT 紀錄 {name} = {}。DNS 生效可能需要數分鐘，請稍後再試。",
            d.verify_token
        )));
    }
    let d = sqlx::query_as("UPDATE domains SET verified = 1 WHERE hostname = ? RETURNING *")
        .bind(&d.hostname)
        .fetch_one(&s.db)
        .await?;
    s.routes.reload(&s.db).await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_verify",
        Some(hostname),
        me.audit_detail(None).as_deref(),
    )
    .await;
    // TODO(M2-02)：觸發 ACME 憑證申請
    Ok(d)
}

/// FR-DOM-03：將網域轉向某個 App 的 service:port；`app_id` 為 None 表示解除綁定
pub async fn bind_domain(
    s: &AppState,
    me: &CurrentUser,
    hostname: &str,
    app_id: Option<&str>,
    service: Option<&str>,
    port: Option<u16>,
) -> ApiResult<Domain> {
    let d = owned_domain(s, me, hostname).await?;
    if let Some(app_id) = app_id {
        let app = owned_app(s, me, app_id).await?;
        let svc = service.ok_or_else(|| ApiError::BadRequest("缺少 service，請指定要轉發的 service 名稱。".into()))?;
        let services = validate_compose(&app.compose_yaml)?.services;
        if !services.iter().any(|x| x == svc) {
            return Err(ApiError::BadRequest(format!(
                "App 中沒有 service `{svc}`，可用的 service：{}。",
                services.join("、")
            )));
        }
        if port.is_none_or(|p| p == 0) {
            return Err(ApiError::BadRequest("缺少 port，請指定容器內監聽的 port。".into()));
        }
    }
    let d: Domain = sqlx::query_as("UPDATE domains SET app_id = ?, service = ?, port = ? WHERE hostname = ? RETURNING *")
        .bind(app_id)
        .bind(app_id.and(service))
        .bind(app_id.and(port).map(i64::from))
        .bind(&d.hostname)
        .fetch_one(&s.db)
        .await?;
    s.routes.reload(&s.db).await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_bind",
        Some(&d.hostname),
        me.audit_detail(app_id).as_deref(),
    )
    .await;
    Ok(d)
}

/// FR-DOM-01
pub async fn delete_domain(s: &AppState, me: &CurrentUser, hostname: &str) -> ApiResult<()> {
    let d = owned_domain(s, me, hostname).await?;
    sqlx::query("DELETE FROM domains WHERE hostname = ?")
        .bind(&d.hostname)
        .execute(&s.db)
        .await?;
    s.routes.reload(&s.db).await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_delete",
        Some(&d.hostname),
        me.audit_detail(None).as_deref(),
    )
    .await;
    Ok(())
}

// ───────────────────────── 使用者 ─────────────────────────

/// FR-USER-01
pub async fn list_users(s: &AppState, me: &CurrentUser) -> ApiResult<Vec<User>> {
    me.require_admin()?;
    Ok(sqlx::query_as("SELECT * FROM users ORDER BY created_at").fetch_all(&s.db).await?)
}

/// FR-USER-02：super_admin 不可被修改、也不可被授予（全站唯一）；只有 super_admin 能授予或撤銷 admin；不可修改自己
/// FR-USER-03：停用即撤銷所有 session
pub async fn set_user_role(s: &AppState, me: &CurrentUser, account: &str, role: Role) -> ApiResult<User> {
    me.require_admin()?;
    let account = account.trim().to_ascii_lowercase();
    if account == me.0.account {
        return Err(ApiError::Forbidden);
    }
    let target: User = sqlx::query_as("SELECT * FROM users WHERE account = ?")
        .bind(&account)
        .fetch_optional(&s.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    if target.role == "super_admin" || role == Role::SuperAdmin {
        return Err(ApiError::Forbidden);
    }
    if (target.role == "admin" || role == Role::Admin) && me.role() != Role::SuperAdmin {
        return Err(ApiError::Forbidden);
    }
    let user = sqlx::query_as("UPDATE users SET role = ? WHERE account = ? RETURNING *")
        .bind(role.as_str())
        .bind(&account)
        .fetch_one(&s.db)
        .await?;
    if role == Role::Disabled {
        sqlx::query("DELETE FROM sessions WHERE account = ?")
            .bind(&account)
            .execute(&s.db)
            .await?;
    }
    audit(
        &s.db,
        Some(&me.0.account),
        "user_set_role",
        Some(&account),
        me.audit_detail(Some(role.as_str())).as_deref(),
    )
    .await;
    Ok(user)
}
