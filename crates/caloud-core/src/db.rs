use crate::model::{Role, Route, User};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;

pub type Db = SqlitePool;

pub async fn connect(data_dir: &Path) -> anyhow::Result<Db> {
    tokio::fs::create_dir_all(data_dir).await?;
    let url = format!("sqlite://{}", data_dir.join("caloud.db").display());
    let opts = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(opts).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// FR-AUTH-03／04：登入成功後建立或更新使用者（ADR 0004）。
///
/// **第一位登入者即為 super admin**：角色在單一 INSERT 陳述式內以子查詢決定，
/// SQLite 一次只允許一個寫入者，因此兩人同時登入也只會有一位拿到 super_admin。
pub async fn upsert_login(db: &Db, account: &str, handle: Option<&str>, default_role: Role) -> anyhow::Result<User> {
    let now = crate::now();
    let user: User = sqlx::query_as(
        "INSERT INTO users (account, handle, role, created_at, last_login)
         VALUES (?1, ?2, CASE WHEN (SELECT COUNT(*) FROM users) = 0 THEN 'super_admin' ELSE ?3 END, ?4, ?4)
         ON CONFLICT(account) DO UPDATE SET last_login = excluded.last_login,
                                            handle = COALESCE(excluded.handle, users.handle)
         RETURNING *",
    )
    .bind(account)
    .bind(handle)
    .bind(default_role.as_str())
    .bind(now)
    .fetch_one(db)
    .await?;
    if user.role == Role::SuperAdmin.as_str() && user.created_at == now && user.last_login == now {
        tracing::warn!(%account, "first login: granted super_admin");
    }
    Ok(user)
}

/// 目前所有可用路由（已驗證網域 + 綁定到 running 的 App）
pub async fn load_routes(db: &Db) -> anyhow::Result<Vec<Route>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        hostname: String,
        app_id: String,
        service: String,
        port: i64,
        tls_mode: String,
        tls_port: Option<i64>,
        target_host: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT d.hostname, d.app_id, d.service, d.port, d.tls_mode, d.tls_port, d.target_host FROM domains d
         JOIN apps a ON a.id = d.app_id
         WHERE d.verified = 1 AND a.status = 'running' AND d.service IS NOT NULL AND d.port IS NOT NULL",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            // FR-DOM-10：host 網路 service 以主機轉發；其餘使用 edge 網路 alias
            let host = r.target_host.unwrap_or_else(|| crate::model_upstream_host(&r.app_id, &r.service));
            Route {
                upstream: format!("http://{host}:{}", r.port),
                passthrough: (r.tls_mode == "passthrough")
                    .then(|| r.tls_port.map(|p| format!("{host}:{p}")))
                    .flatten(),
                hostname: r.hostname,
                // 單機 App 直接在控制面主機上
                node: None,
            }
        })
        .chain(load_env_routes(db).await?)
        .collect())
}

/// FR-OPS-13：running 的平臺環境經所在節點的通道轉發到對外服務的固定 IP（ADR 0015）
async fn load_env_routes(db: &Db) -> anyhow::Result<Vec<Route>> {
    let rows: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT hostname, public_ip, public_port, node FROM environments WHERE status = 'running' AND public_ip IS NOT NULL",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(hostname, ip, port, node)| Route {
            hostname,
            upstream: format!("http://{ip}:{port}"),
            passthrough: None,
            node: Some(node),
        })
        .collect())
}

/// FR-AUD-01
pub async fn audit(db: &Db, account: Option<&str>, action: &str, target: Option<&str>, detail: Option<&str>) {
    let r = sqlx::query("INSERT INTO audit_log (account, action, target, detail, at) VALUES (?, ?, ?, ?, ?)")
        .bind(account)
        .bind(action)
        .bind(target)
        .bind(detail)
        .bind(crate::now())
        .execute(db)
        .await;
    if let Err(e) = r {
        tracing::error!(error = %e, "audit log write failed");
    }
}

/// FR-OPS-04：把超過 `OFFLINE_AFTER_SECS` 沒有心跳的 active 節點標為離線，回傳筆數。
/// 排空中的節點維持 `draining`，以免重連後又開始接工作。
pub async fn mark_offline_nodes(db: &Db, now: i64) -> sqlx::Result<u64> {
    let r = sqlx::query("UPDATE nodes SET status = 'offline' WHERE status = 'active' AND last_seen < ?")
        .bind(now - crate::agent::OFFLINE_AFTER_SECS)
        .execute(db)
        .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-AUTH-03
    #[tokio::test]
    async fn first_login_becomes_super_admin() {
        let dir = std::env::temp_dir().join(format!("caloud-test-{}", uuid::Uuid::new_v4()));
        let db = connect(&dir).await.unwrap();
        let a = upsert_login(&db, "0xaaa", None, Role::Pending).await.unwrap();
        let b = upsert_login(&db, "0xbbb", Some("bob"), Role::Pending).await.unwrap();
        let a2 = upsert_login(&db, "0xaaa", Some("alice"), Role::Pending).await.unwrap();
        assert_eq!(a.role, "super_admin");
        assert_eq!(b.role, "pending");
        assert_eq!(a2.role, "super_admin");
        assert_eq!(a2.handle.as_deref(), Some("alice"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
