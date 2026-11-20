//! 建置資源與建置分鐘（FR-CI-06，[ADR 0021](../../../../docs/adr/0021-checks-quota-release.md)）。
//!
//! 方案（M8）上線前由後台設定：建置與沙箱的 CPU、記憶體，以及每位用戶每月的建置分鐘（0＝不限）。
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::agent::BuildLimits;
use caloud_core::db::audit;
use caloud_core::now;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Settings {
    pub cpus: i64,
    pub memory_mb: i64,
    pub minutes_monthly: i64,
}

pub async fn settings(s: &AppState) -> ApiResult<Settings> {
    Ok(
        sqlx::query_as("SELECT cpus, memory_mb, minutes_monthly FROM build_settings WHERE id = 1")
            .fetch_optional(&s.db)
            .await?
            .unwrap_or(Settings {
                cpus: 4,
                memory_mb: 8192,
                minutes_monthly: 0,
            }),
    )
}

pub(crate) async fn limits(s: &AppState) -> ApiResult<BuildLimits> {
    let c = settings(s).await?;
    Ok(BuildLimits {
        cpus: c.cpus.clamp(1, 16) as u32,
        memory_mb: c.memory_mb.clamp(1024, 32768) as u32,
    })
}

/// UTC 月初（Unix 秒）
pub fn month_start(t: i64) -> i64 {
    let days = t.div_euclid(86400);
    // 公民曆：由天數求年月（Howard Hinnant 的演算法）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    (days - (d - 1)) * 86400
}

/// 帳號本月用量（分鐘，無條件進位）
pub async fn minutes(s: &AppState, account: &str) -> ApiResult<i64> {
    let secs: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(secs), 0) FROM build_usage WHERE account = ? AND at >= ?")
        .bind(account)
        .bind(month_start(now()))
        .fetch_one(&s.db)
        .await?;
    Ok((secs + 59) / 60)
}

/// 本月用量已達上限時拒絕（驗證與部署）
pub(crate) async fn check_quota(s: &AppState, account: &str) -> ApiResult<()> {
    let limit = settings(s).await?.minutes_monthly;
    if limit > 0 && minutes(s, account).await? >= limit {
        return Err(ApiError::Conflict(format!(
            "本月建置分鐘已用完（上限 {limit} 分鐘），請下個月再試或洽管理員。"
        )));
    }
    Ok(())
}

/// agent 回報的用量；`kind` 為 deploy（`id` 為環境）或 verify（`id` 為驗證）。只接受該節點上的對象
pub(crate) async fn record(s: &AppState, node: &str, kind: &str, id: &str, secs: u64) -> ApiResult<()> {
    let secs = secs.min(24 * 3600) as i64;
    let owner: Option<(String, Option<String>)> = match kind {
        "deploy" => {
            sqlx::query_as(
                "SELECT p.owner, p.id FROM environments e JOIN projects p ON p.id = e.project WHERE e.subdomain = ? AND e.node = ?",
            )
            .bind(id)
            .bind(node)
            .fetch_optional(&s.db)
            .await?
        }
        "verify" => {
            let r: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT p.owner, p.id FROM verifications v JOIN projects p ON p.id = v.project WHERE v.id = ? AND v.node = ?",
            )
            .bind(id)
            .bind(node)
            .fetch_optional(&s.db)
            .await?;
            if r.is_some() {
                sqlx::query("UPDATE verifications SET build_secs = build_secs + ? WHERE id = ?")
                    .bind(secs)
                    .bind(id)
                    .execute(&s.db)
                    .await?;
            }
            r
        }
        _ => None,
    };
    let Some((account, project)) = owner else { return Ok(()) };
    sqlx::query("INSERT INTO build_usage (account, project, kind, ref_id, secs, at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(account)
        .bind(project)
        .bind(kind)
        .bind(id)
        .bind(secs)
        .bind(now())
        .execute(&s.db)
        .await?;
    Ok(())
}

/// `GET /api/admin/build-settings`：設定與各用戶本月用量
pub async fn get(s: &AppState, me: &CurrentUser) -> ApiResult<Value> {
    me.require_admin()?;
    let c = settings(s).await?;
    let usage: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT b.account, u.handle, SUM(b.secs) FROM build_usage b LEFT JOIN users u ON u.account = b.account \
         WHERE b.at >= ? GROUP BY b.account ORDER BY SUM(b.secs) DESC LIMIT 100",
    )
    .bind(month_start(now()))
    .fetch_all(&s.db)
    .await?;
    Ok(json!({
        "cpus": c.cpus, "memory_mb": c.memory_mb, "minutes_monthly": c.minutes_monthly,
        "usage": usage.into_iter().map(|(a, h, secs)| json!({ "account": a, "handle": h, "minutes": (secs + 59) / 60 })).collect::<Vec<_>>(),
    }))
}

/// `PUT /api/admin/build-settings`
pub async fn set(s: &AppState, me: &CurrentUser, input: Settings) -> ApiResult<Value> {
    me.require_admin()?;
    if !(1..=16).contains(&input.cpus) || !(1024..=32768).contains(&input.memory_mb) || input.minutes_monthly < 0 {
        return Err(ApiError::BadRequest(
            "cpus 需介於 1–16、memory_mb 需介於 1024–32768、minutes_monthly 不可為負數。".into(),
        ));
    }
    sqlx::query("UPDATE build_settings SET cpus = ?, memory_mb = ?, minutes_monthly = ?, updated_at = ? WHERE id = 1")
        .bind(input.cpus)
        .bind(input.memory_mb)
        .bind(input.minutes_monthly)
        .bind(now())
        .execute(&s.db)
        .await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "build_settings",
        None,
        me.audit_detail(Some(&format!(
            "cpus={} memory_mb={} minutes_monthly={}",
            input.cpus, input.memory_mb, input.minutes_monthly
        )))
        .as_deref(),
    )
    .await;
    get(s, me).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn month_start_utc() {
        // 2026-11-26 03:00:00 UTC → 2026-11-01
        assert_eq!(super::month_start(1_795_662_000), 1_793_491_200);
        // 2024-02-29 → 2024-02-01；月初當天不變
        assert_eq!(super::month_start(1_709_164_800), 1_706_745_600);
        assert_eq!(super::month_start(1_706_745_600), 1_706_745_600);
    }
}
