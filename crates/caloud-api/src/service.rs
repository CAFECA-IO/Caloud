//! 服務層：App、網域、使用者的業務操作（ADR 0008）。
//!
//! REST handler 與 MCP 工具都只能經由這裡操作資源；權限檢查、輸入驗證、稽核紀錄都在此完成，
//! 確保兩個入口的行為一致（docs/05-components.md §B）。
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::db::audit;
use caloud_core::model::{App, Domain, Role, User};
use caloud_core::now;
use caloud_orchestrator::{DeploySpec, GitSource, Policy, ValidatedCompose};
use serde_json::{json, Value};

pub mod admin;
pub mod projects;

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

/// 建立或更新 App 的輸入（REST 與 MCP 共用，docs/06-api-spec.md §3.4）
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct AppInput {
    pub name: Option<String>,
    /// `compose`（預設）或 `git`
    pub source: Option<String>,
    pub compose_yaml: Option<String>,
    pub git_url: Option<String>,
    pub git_ref: Option<String>,
    pub git_path: Option<String>,
    pub compose_file: Option<String>,
    pub services: Option<Vec<String>>,
    pub env: Option<String>,
    pub privileged: Option<bool>,
    #[serde(default = "yes")]
    pub deploy: bool,
}

fn yes() -> bool {
    true
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::BadRequest(msg.into())
}

/// Git 網址：`https://`、不含帳密與空白（docs/07-security.md §1、§3.5）
pub(crate) fn check_repo_url(url: &str) -> ApiResult<()> {
    let rest = url.strip_prefix("https://").ok_or_else(|| bad("Git 網址必須以 https:// 開頭。"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || url.chars().any(char::is_whitespace) || url.len() > 512 {
        return Err(bad("Git 網址格式錯誤，且不可包含帳號密碼；目前只支援公開儲存庫。"));
    }
    Ok(())
}

fn git_word(v: &str) -> bool {
    !v.is_empty() && v.len() <= 200 && !v.starts_with('-') && v.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

/// 分支、標籤或 commit
pub(crate) fn check_git_ref(git_ref: &str) -> ApiResult<()> {
    if !git_word(git_ref) || git_ref.contains("..") {
        return Err(bad("ref 只能包含英數字與 . _ / -，例如 main 或 v1.2.0。"));
    }
    Ok(())
}

/// 儲存庫內的相對目錄（"" 為根目錄）
pub(crate) fn check_repo_path(path: &str) -> ApiResult<()> {
    if !path.is_empty() && (!git_word(path) || path.starts_with('/') || path.split('/').any(|c| c == "..")) {
        return Err(bad("目錄必須是儲存庫內的相對路徑，例如 deploy/livekit。"));
    }
    Ok(())
}

/// Git 來源的設定檢查（docs/07-security.md §3.5）
fn check_git(url: &str, git_ref: &str, path: &str, file: &str) -> ApiResult<()> {
    check_repo_url(url)?;
    check_git_ref(git_ref)?;
    check_repo_path(path)?;
    if !git_word(file) || file.contains('/') {
        return Err(bad("compose 檔名格式錯誤，例如 docker-compose.yml。"));
    }
    Ok(())
}

/// FR-APP-14：`.env` 內容檢查
fn check_env(env: &str) -> ApiResult<()> {
    if env.len() > 64 * 1024 {
        return Err(bad("環境變數內容過長（上限 64 KB）。"));
    }
    for (i, line) in env.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let key = l
            .split_once('=')
            .map(|(k, _)| k.trim().trim_start_matches("export ").trim())
            .unwrap_or_default();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || key.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(bad(format!("環境變數第 {} 行格式錯誤，請使用 KEY=value。", i + 1)));
        }
    }
    Ok(())
}

pub fn git_source(app: &App) -> Option<GitSource> {
    (app.source == "git").then(|| GitSource {
        url: app.git_url.clone().unwrap_or_default(),
        git_ref: app.git_ref.clone().unwrap_or_else(|| "main".into()),
        path: app.git_path.clone().unwrap_or_default(),
        compose_file: app.compose_file.clone().unwrap_or_else(|| "docker-compose.yml".into()),
    })
}

fn policy(privileged: bool, git: Option<&GitSource>) -> Policy {
    Policy {
        privileged,
        repo_depth: git.map(|g| g.path.split('/').filter(|c| !c.is_empty() && *c != ".").count()),
        ..Default::default()
    }
}

fn validate_compose(yaml: &str, privileged: bool, git: Option<&GitSource>) -> ApiResult<ValidatedCompose> {
    caloud_orchestrator::validate_with(yaml, policy(privileged, git)).map_err(|e| ApiError::BadRequest(e.to_string()))
}

fn check_services(compose: &ValidatedCompose, services: &Option<Vec<String>>) -> ApiResult<()> {
    for s in services.iter().flatten() {
        if !compose.services.contains(s) {
            return Err(bad(format!(
                "compose 中沒有 service `{s}`，可用的 service：{}。",
                compose.services.join("、")
            )));
        }
    }
    Ok(())
}

/// FR-APP-13：取得 Git 來源並依政策檢查，回傳 compose 原文與檢查結果
async fn fetch_and_validate(s: &AppState, app_id: &str, git: &GitSource, privileged: bool) -> ApiResult<(String, ValidatedCompose)> {
    let yaml = s.runner.fetch_git(app_id, git).await.map_err(|e| bad(e.to_string()))?;
    let compose = validate_compose(&yaml, privileged, Some(git))?;
    Ok((yaml, compose))
}

/// 綁定目標主機：host 網路 service 走主機閘道，其餘走 edge 網路 alias（FR-DOM-10）
fn target_host(app: &App, service: &str) -> String {
    let host_network = caloud_orchestrator::validate_with(&app.compose_yaml, policy(app.privileged, git_source(app).as_ref()))
        .ok()
        .and_then(|c| c.doc["services"][service]["network_mode"].as_str().map(|m| m == "host"))
        .unwrap_or(false);
    if host_network {
        caloud_core::model::HOST_GATEWAY.to_string()
    } else {
        caloud_core::model_upstream_host(&app.id, service)
    }
}

/// FR-APP-02：App、compose 原文、service 清單與容器狀態
pub async fn get_app(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<Value> {
    let app = owned_app(s, me, id).await?;
    let git = git_source(&app);
    let containers = s.runner.status(id, git.as_ref()).await.unwrap_or_default();
    let services = caloud_orchestrator::validate_with(&app.compose_yaml, policy(app.privileged, git.as_ref()))
        .map(|c| c.services)
        .unwrap_or_default();
    Ok(json!({ "app": app, "compose_yaml": app.compose_yaml, "services": services, "containers": containers }))
}

/// FR-APP-01～03、FR-APP-13～16：建立 App；`deploy` 為 true 時立即在背景部署
pub async fn create_app(s: &AppState, me: &CurrentUser, input: AppInput) -> ApiResult<App> {
    me.require_deploy()?;
    let source = input
        .source
        .clone()
        .unwrap_or_else(|| if input.git_url.is_some() { "git".into() } else { "compose".into() });
    let privileged = input.privileged.unwrap_or(false);
    if source == "git" || privileged {
        me.require_admin()?;
    }
    if let Some(env) = &input.env {
        check_env(env)?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let (yaml, compose, git) = match source.as_str() {
        "compose" => {
            let yaml = input.compose_yaml.clone().ok_or_else(|| bad("請提供 compose 內容。"))?;
            let compose = validate_compose(&yaml, privileged, None)?;
            (yaml, compose, None)
        }
        "git" => {
            let git = GitSource {
                url: input
                    .git_url
                    .clone()
                    .ok_or_else(|| bad("請提供 Git 儲存庫網址。"))?
                    .trim()
                    .to_string(),
                git_ref: input
                    .git_ref
                    .clone()
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or_else(|| "main".into()),
                path: input.git_path.clone().unwrap_or_default().trim().trim_matches('/').to_string(),
                compose_file: input
                    .compose_file
                    .clone()
                    .filter(|f| !f.trim().is_empty())
                    .unwrap_or_else(|| "docker-compose.yml".into()),
            };
            check_git(&git.url, &git.git_ref, &git.path, &git.compose_file)?;
            let (yaml, compose) = fetch_and_validate(s, &id, &git, privileged).await?;
            (yaml, compose, Some(git))
        }
        _ => return Err(bad("source 只能是 compose 或 git。")),
    };
    check_services(&compose, &input.services)?;
    let name = input
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| compose.services[0].clone());
    let services = input
        .services
        .filter(|v| !v.is_empty())
        .map(|v| serde_json::to_string(&v).unwrap_or_default());
    let app: App = sqlx::query_as(
        "INSERT INTO apps (id, name, owner, compose_yaml, status, created_at, updated_at, source, git_url, git_ref, git_path, compose_file, services, env, privileged)
         VALUES (?, ?, ?, ?, 'stopped', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING *",
    )
    .bind(&id)
    .bind(&name)
    .bind(&me.0.account)
    .bind(&yaml)
    .bind(now())
    .bind(now())
    .bind(&source)
    .bind(git.as_ref().map(|g| g.url.clone()))
    .bind(git.as_ref().map(|g| g.git_ref.clone()))
    .bind(git.as_ref().map(|g| g.path.clone()))
    .bind(git.as_ref().map(|g| g.compose_file.clone()))
    .bind(services)
    .bind(input.env.filter(|e| !e.trim().is_empty()))
    .bind(privileged)
    .fetch_one(&s.db)
    .await?;
    let detail = format!(
        "{name}{}{}",
        if source == "git" { "; source=git" } else { "" },
        if privileged { "; privileged" } else { "" }
    );
    audit(
        &s.db,
        Some(&me.0.account),
        "app_create",
        Some(&id),
        me.audit_detail(Some(&detail)).as_deref(),
    )
    .await;
    if input.deploy {
        spawn_deploy(s.clone(), app.clone(), Some(compose));
    }
    Ok(app)
}

/// FR-APP-04、FR-APP-13～16：更新 App；省略的欄位維持原值；`deploy` 為 true 時立即重新部署
pub async fn update_app(s: &AppState, me: &CurrentUser, id: &str, input: AppInput) -> ApiResult<App> {
    let old = owned_app(s, me, id).await?;
    let privileged = input.privileged.unwrap_or(old.privileged);
    if old.source == "git" || privileged || old.privileged {
        me.require_admin()?;
    }
    if let Some(env) = &input.env {
        check_env(env)?;
    }
    let mut git = git_source(&old);
    if let Some(g) = git.as_mut() {
        if let Some(r) = input.git_ref.clone().filter(|r| !r.trim().is_empty()) {
            g.git_ref = r;
        }
        if let Some(p) = input.git_path.clone() {
            g.path = p.trim().trim_matches('/').to_string();
        }
        if let Some(f) = input.compose_file.clone().filter(|f| !f.trim().is_empty()) {
            g.compose_file = f;
        }
        check_git(&g.url, &g.git_ref, &g.path, &g.compose_file)?;
    }
    let (yaml, compose) = match &git {
        Some(g) => fetch_and_validate(s, id, g, privileged).await?,
        None => {
            let yaml = input.compose_yaml.clone().unwrap_or_else(|| old.compose_yaml.clone());
            let compose = validate_compose(&yaml, privileged, None)?;
            (yaml, compose)
        }
    };
    let services = match &input.services {
        Some(v) => {
            check_services(&compose, &input.services)?;
            (!v.is_empty()).then(|| serde_json::to_string(v).unwrap_or_default())
        }
        None => old.services.clone(),
    };
    let env = match &input.env {
        Some(e) => (!e.trim().is_empty()).then(|| e.clone()),
        None => old.env.clone(),
    };
    let app: App = sqlx::query_as(
        "UPDATE apps SET compose_yaml = ?, name = COALESCE(?, name), git_ref = ?, git_path = ?, compose_file = ?, services = ?, env = ?, privileged = ?, updated_at = ?
         WHERE id = ? RETURNING *",
    )
    .bind(&yaml)
    .bind(input.name.filter(|n| !n.trim().is_empty()))
    .bind(git.as_ref().map(|g| g.git_ref.clone()))
    .bind(git.as_ref().map(|g| g.path.clone()))
    .bind(git.as_ref().map(|g| g.compose_file.clone()))
    .bind(services)
    .bind(env)
    .bind(privileged)
    .bind(now())
    .bind(id)
    .fetch_one(&s.db)
    .await?;
    // 綁定到此 App 的網域重新解析轉發目標（service 可能改為 host 網路）
    let bound: Vec<(String, String)> = sqlx::query_as("SELECT hostname, service FROM domains WHERE app_id = ? AND service IS NOT NULL")
        .bind(id)
        .fetch_all(&s.db)
        .await?;
    for (host, svc) in bound {
        sqlx::query("UPDATE domains SET target_host = ? WHERE hostname = ?")
            .bind(target_host(&app, &svc))
            .bind(&host)
            .execute(&s.db)
            .await?;
    }
    s.routes.reload(&s.db).await?;
    let detail = input.env.is_some().then_some("env changed");
    audit(
        &s.db,
        Some(&me.0.account),
        "app_update",
        Some(id),
        me.audit_detail(detail).as_deref(),
    )
    .await;
    if input.deploy {
        spawn_deploy(s.clone(), app.clone(), Some(compose));
    }
    Ok(app)
}

/// FR-APP-03：重新部署（背景執行；Git 來源會重新取得指定 ref）
pub async fn deploy_app(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<()> {
    let app = owned_app(s, me, id).await?;
    let compose = match git_source(&app) {
        Some(_) => None,
        None => Some(validate_compose(&app.compose_yaml, app.privileged, None)?),
    };
    audit(&s.db, Some(&me.0.account), "app_deploy", Some(id), me.audit_detail(None).as_deref()).await;
    spawn_deploy(s.clone(), app, compose);
    Ok(())
}

/// FR-APP-05
pub async fn stop_app(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<()> {
    let app = owned_app(s, me, id).await?;
    s.runner.stop(id, git_source(&app).as_ref()).await?;
    set_status(s, id, "stopped", None).await?;
    s.routes.reload(&s.db).await?;
    audit(&s.db, Some(&me.0.account), "app_stop", Some(id), me.audit_detail(None).as_deref()).await;
    Ok(())
}

/// FR-APP-05：預設保留 volume
pub async fn delete_app(s: &AppState, me: &CurrentUser, id: &str, purge_volumes: bool) -> ApiResult<()> {
    let app = owned_app(s, me, id).await?;
    s.runner.remove(id, git_source(&app).as_ref(), purge_volumes).await?;
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
    let app = owned_app(s, me, id).await?;
    Ok(s.runner
        .logs(id, git_source(&app).as_ref(), service, tail.unwrap_or(200).min(5000))
        .await?)
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
/// Git 來源未傳入 `compose` 時，先重新取得並檢查（FR-APP-13）
/// TODO(M2-05, FR-APP-08)：每 App 部署互斥鎖
fn spawn_deploy(s: AppState, app: App, compose: Option<ValidatedCompose>) {
    tokio::spawn(async move {
        // 狀態寫入失敗只影響顯示，部署本身仍繼續；錯誤寫入 log
        if let Err(e) = set_status(&s, &app.id, "deploying", None).await {
            tracing::error!(error = %e, "set status failed");
        }
        let git = git_source(&app);
        let result = async {
            let compose = match (compose, &git) {
                (Some(c), _) => c,
                (None, Some(g)) => {
                    let (yaml, c) = fetch_and_validate(&s, &app.id, g, app.privileged)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.public_message()))?;
                    sqlx::query("UPDATE apps SET compose_yaml = ? WHERE id = ?")
                        .bind(&yaml)
                        .bind(&app.id)
                        .execute(&s.db)
                        .await?;
                    c
                }
                (None, None) => {
                    validate_compose(&app.compose_yaml, app.privileged, None).map_err(|e| anyhow::anyhow!(e.public_message()))?
                }
            };
            let services = app.service_list();
            let reserved = s.cfg.reserved_ports();
            let spec = DeploySpec {
                compose: &compose,
                git: git.as_ref(),
                env: app.env.as_deref(),
                services: services.as_deref(),
                privileged: app.privileged,
                reserved_ports: &reserved,
            };
            s.runner.deploy(&app.id, spec).await
        }
        .await;
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

/// 綁定設定（FR-DOM-03、FR-DOM-09）
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct BindInput {
    pub app_id: Option<String>,
    pub service: Option<String>,
    pub port: Option<u16>,
    /// `none`（預設）或 `passthrough`
    pub tls_mode: Option<String>,
    pub tls_port: Option<u16>,
}

/// FR-DOM-03、FR-DOM-09、FR-DOM-10：將網域轉向某個 App 的 service:port；`app_id` 為 None 表示解除綁定
pub async fn bind_domain(s: &AppState, me: &CurrentUser, hostname: &str, b: BindInput) -> ApiResult<Domain> {
    let d = owned_domain(s, me, hostname).await?;
    let tls_mode = b.tls_mode.clone().unwrap_or_else(|| "none".into());
    if tls_mode != "none" && tls_mode != "passthrough" {
        return Err(bad("tls_mode 只能是 none 或 passthrough。"));
    }
    let mut target = None;
    if let Some(app_id) = b.app_id.as_deref() {
        let app = owned_app(s, me, app_id).await?;
        let svc = b
            .service
            .as_deref()
            .ok_or_else(|| bad("缺少 service，請指定要轉發的 service 名稱。"))?;
        let compose = caloud_orchestrator::validate_with(&app.compose_yaml, policy(app.privileged, git_source(&app).as_ref()))
            .map_err(|e| bad(e.to_string()))?;
        if !compose.services.iter().any(|x| x == svc) {
            return Err(bad(format!(
                "App 中沒有 service `{svc}`，可用的 service：{}。",
                compose.services.join("、")
            )));
        }
        if b.port.is_none_or(|p| p == 0) {
            return Err(bad("缺少 port，請指定容器內監聽的 port。"));
        }
        if tls_mode == "passthrough" && b.tls_port.is_none_or(|p| p == 0) {
            return Err(bad("TLS 直通需要指定 App 終結 TLS 的 port，例如 443。"));
        }
        target = Some(target_host(&app, svc));
    }
    let bound = b.app_id.is_some();
    let d: Domain = sqlx::query_as(
        "UPDATE domains SET app_id = ?, service = ?, port = ?, tls_mode = ?, tls_port = ?, target_host = ? WHERE hostname = ? RETURNING *",
    )
    .bind(&b.app_id)
    .bind(if bound { b.service.clone() } else { None })
    .bind(if bound { b.port.map(i64::from) } else { None })
    .bind(if bound { tls_mode.as_str() } else { "none" })
    .bind(if bound && tls_mode == "passthrough" {
        b.tls_port.map(i64::from)
    } else {
        None
    })
    .bind(&target)
    .bind(&d.hostname)
    .fetch_one(&s.db)
    .await?;
    s.routes.reload(&s.db).await?;
    let detail = b.app_id.as_deref().map(|a| {
        if tls_mode == "passthrough" {
            format!("{a}; tls=passthrough")
        } else {
            a.to_string()
        }
    });
    audit(
        &s.db,
        Some(&me.0.account),
        "domain_bind",
        Some(&d.hostname),
        me.audit_detail(detail.as_deref()).as_deref(),
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
