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
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT d.hostname, d.app_id, d.service, d.port FROM domains d
         JOIN apps a ON a.id = d.app_id
         WHERE d.verified = 1 AND a.status = 'running' AND d.service IS NOT NULL AND d.port IS NOT NULL",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(hostname, app_id, service, port)| Route {
            hostname,
            upstream: format!("http://{}:{}", crate::model_upstream_host(&app_id, &service), port),
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
