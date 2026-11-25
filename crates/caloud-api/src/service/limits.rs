//! 專案資源上限（FR-ENV-16，[ADR 0025](../../../../docs/adr/0025-project-limits.md)）：
//! Admin 設定專案的記憶體與 CPU 合計上限，服務在 compose 宣告用量，不超過上限即採用。
use super::environments::PER_SERVICE;
use super::projects::owned;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_core::agent::ServiceLimit;
use caloud_core::db::audit;
use caloud_core::model::Project;
use caloud_core::now;
use caloud_core::placement::Resources;
use caloud_orchestrator::sandbox::declared_limits;
use caloud_orchestrator::ValidatedCompose;
use serde::Deserialize;
use serde_json::{json, Value};

/// 未宣告的服務、未設定專案上限時每個服務的上限
pub const DEFAULT_MEMORY_MB: u64 = 1024;
pub const DEFAULT_CPU_MILLIS: u64 = 1000;
const MIN_MEMORY_MB: u64 = 64;
const MIN_CPU_MILLIS: u64 = 100;

/// 一個服務的上限與是否由 compose 宣告
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub limit: ServiceLimit,
    pub memory_declared: bool,
    pub cpu_declared: bool,
}

fn gb(mb: u64) -> String {
    let g = mb as f64 / 1024.0;
    if g.fract() == 0.0 {
        format!("{g:.0} GB")
    } else {
        format!("{g:.1} GB")
    }
}

fn cores(m: u64) -> String {
    let c = m as f64 / 1000.0;
    if c.fract() == 0.0 {
        format!("{c:.0} 核")
    } else {
        format!("{c:.2} 核")
    }
}

/// 依專案上限檢查 compose 宣告，回傳每個服務的上限；超過時回傳說明
pub fn resolve(compose: &ValidatedCompose, memory_cap: Option<i64>, cpu_cap: Option<f64>) -> Result<Vec<Resolved>, String> {
    let out: Vec<Resolved> = declared_limits(compose)
        .into_iter()
        .map(|d| Resolved {
            limit: ServiceLimit {
                service: d.service,
                memory_mb: d.memory_mb.unwrap_or(DEFAULT_MEMORY_MB),
                cpu_millis: d.cpus.map(|c| (c * 1000.0).round() as u64).unwrap_or(DEFAULT_CPU_MILLIS),
            },
            memory_declared: d.memory_mb.is_some(),
            cpu_declared: d.cpus.is_some(),
        })
        .collect();
    for r in &out {
        let l = &r.limit;
        if l.memory_mb < MIN_MEMORY_MB {
            return Err(format!("`{}` 的記憶體至少要 {MIN_MEMORY_MB} MB。", l.service));
        }
        if l.cpu_millis < MIN_CPU_MILLIS {
            return Err(format!("`{}` 的 CPU 至少要 0.1 核。", l.service));
        }
    }
    let total_mem: u64 = out.iter().map(|r| r.limit.memory_mb).sum();
    let total_cpu: u64 = out.iter().map(|r| r.limit.cpu_millis).sum();
    let big = |f: &dyn Fn(&Resolved) -> bool| {
        out.iter()
            .filter(|r| f(r))
            .map(|r| format!("`{}`", r.limit.service))
            .collect::<Vec<_>>()
            .join("、")
    };
    match memory_cap {
        None => {
            let over = big(&|r| r.limit.memory_mb > DEFAULT_MEMORY_MB);
            if !over.is_empty() {
                return Err(format!(
                    "{over} 的記憶體超過 1 GB；這個專案沒有放寬資源上限，每個服務最多 1 GB，需要更多請聯絡 Admin。"
                ));
            }
        }
        Some(cap) if total_mem > cap.max(0) as u64 => {
            return Err(format!(
                "服務合計記憶體 {}，超過專案上限 {}（沒有宣告的服務以 1 GB 計）。",
                gb(total_mem),
                gb(cap as u64)
            ));
        }
        _ => {}
    }
    match cpu_cap {
        None => {
            let over = big(&|r| r.limit.cpu_millis > DEFAULT_CPU_MILLIS);
            if !over.is_empty() {
                return Err(format!(
                    "{over} 的 CPU 超過 1 核；這個專案沒有放寬資源上限，每個服務最多 1 核，需要更多請聯絡 Admin。"
                ));
            }
        }
        Some(cap) if total_cpu > (cap * 1000.0).round() as u64 => {
            return Err(format!(
                "服務合計 CPU {}，超過專案上限 {}（沒有宣告的服務以 1 核計）。",
                cores(total_cpu),
                cores((cap * 1000.0).round() as u64)
            ));
        }
        _ => {}
    }
    Ok(out)
}

/// 排程預留量：宣告的服務以宣告值，其餘維持每個服務 512 MB／0.25 核
pub fn need(limits: &[Resolved]) -> Resources {
    limits.iter().fold(
        Resources {
            cpu_millis: 0,
            memory_bytes: 0,
        },
        |acc, r| Resources {
            cpu_millis: acc.cpu_millis
                + if r.cpu_declared {
                    r.limit.cpu_millis
                } else {
                    PER_SERVICE.cpu_millis
                },
            memory_bytes: acc.memory_bytes
                + if r.memory_declared {
                    r.limit.memory_mb * 1024 * 1024
                } else {
                    PER_SERVICE.memory_bytes
                },
        },
    )
}

/// 專案頁的 `limits`
pub fn view(p: &Project, compose: Option<&ValidatedCompose>) -> Value {
    let (services, error) = match compose {
        None => (vec![], None),
        Some(c) => {
            let error = resolve(c, p.memory_limit_mb, p.cpu_limit).err();
            // 超過上限時仍列出宣告值，方便對照
            let list = resolve(c, Some(i64::MAX), Some(f64::MAX)).unwrap_or_default();
            (list, error)
        }
    };
    json!({
        "memory_mb": p.memory_limit_mb,
        "cpus": p.cpu_limit,
        "services": services.iter().map(|r| json!({
            "service": r.limit.service,
            "memory_mb": r.limit.memory_mb,
            "cpus": r.limit.cpu_millis as f64 / 1000.0,
            "declared": r.memory_declared || r.cpu_declared,
        })).collect::<Vec<_>>(),
        "total_memory_mb": services.iter().map(|r| r.limit.memory_mb).sum::<u64>(),
        "total_cpus": services.iter().map(|r| r.limit.cpu_millis).sum::<u64>() as f64 / 1000.0,
        "error": error,
    })
}

#[derive(Debug, Deserialize)]
pub struct LimitsInput {
    pub memory_mb: Option<i64>,
    pub cpus: Option<f64>,
}

/// `PUT /api/projects/{id}/limits`（admin+）
pub async fn set(s: &AppState, me: &CurrentUser, id: &str, input: LimitsInput) -> ApiResult<Value> {
    me.require_admin()?;
    owned(s, me, id).await?;
    if let Some(m) = input.memory_mb {
        if !(1024..=131072).contains(&m) {
            return Err(ApiError::BadRequest("記憶體上限需介於 1024 與 131072 MB。".into()));
        }
    }
    if let Some(c) = input.cpus {
        if !(1.0..=64.0).contains(&c) {
            return Err(ApiError::BadRequest("CPU 上限需介於 1 與 64 核。".into()));
        }
    }
    sqlx::query("UPDATE projects SET memory_limit_mb = ?, cpu_limit = ?, updated_at = ? WHERE id = ?")
        .bind(input.memory_mb)
        .bind(input.cpus)
        .bind(now())
        .bind(id)
        .execute(&s.db)
        .await?;
    let detail = format!(
        "memory={} cpus={}",
        input.memory_mb.map(|m| format!("{m}MB")).unwrap_or_else(|| "default".into()),
        input.cpus.map(|c| c.to_string()).unwrap_or_else(|| "default".into())
    );
    audit(
        &s.db,
        Some(&me.0.account),
        "project_limits",
        Some(id),
        me.audit_detail(Some(&detail)).as_deref(),
    )
    .await;
    let p: Project = sqlx::query_as("SELECT * FROM projects WHERE id = ?")
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    let compose = super::deploy::confirmed_compose(s, &p).await?;
    Ok(view(&p, compose.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use caloud_orchestrator::{validate_with, Policy};

    fn c(y: &str) -> ValidatedCompose {
        validate_with(y, Policy::platform(0)).unwrap()
    }

    #[test]
    fn caps_and_defaults() {
        let plain = c("services:\n  web:\n    image: x\n  infer:\n    image: y\n");
        let r = resolve(&plain, None, None).unwrap();
        assert_eq!(r[0].limit.memory_mb, 1024);
        assert_eq!(need(&r).memory_bytes, 2 * 512 * 1024 * 1024, "未宣告維持 512 MB 預留");

        let big = c("services:\n  web:\n    image: x\n  infer:\n    image: y\n    mem_limit: 15g\n    cpus: 4\n");
        assert!(resolve(&big, None, None).unwrap_err().contains("聯絡 Admin"));
        assert!(resolve(&big, Some(16384), None).unwrap_err().contains("CPU"));
        let r = resolve(&big, Some(16384), Some(8.0)).unwrap();
        assert_eq!(
            r[1].limit,
            ServiceLimit {
                service: "infer".into(),
                memory_mb: 15360,
                cpu_millis: 4000
            }
        );
        assert_eq!(need(&r).memory_bytes, (15360 + 512) * 1024 * 1024);
        assert_eq!(need(&r).cpu_millis, 4000 + 250);
        let e = resolve(&big, Some(16000), Some(8.0)).unwrap_err();
        assert!(e.contains("16 GB") && e.contains("15.6 GB"), "{e}");
        assert!(resolve(&c("services:\n  a:\n    image: x\n    mem_limit: 10m\n"), None, None)
            .unwrap_err()
            .contains("64 MB"));
    }
}
