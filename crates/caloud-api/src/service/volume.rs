//! 加上 volume（FR-ENV-15，[ADR 0024](../../../../docs/adr/0024-project-page.md)）：
//! 持久化目錄沒有掛 volume 時，預覽 compose 修改，確認後寫入確認的設定。
use super::deploy::{self, DeployInput};
use super::projects::owned;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CurrentUser};
use caloud_analyzer::Analysis;
use caloud_core::db::audit;
use caloud_orchestrator::{validate_with, Policy};
use serde::Deserialize;
use serde_json::{json, Value};
use serde_yaml::{Mapping, Value as Yaml};

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::BadRequest(m.into())
}

#[derive(Debug, Deserialize)]
pub struct VolumeInput {
    pub service: String,
    pub target: String,
    /// 確認後立即部署（預設關閉）
    #[serde(default)]
    pub deploy: bool,
}

/// 服務掛載的容器內路徑
fn mounts(svc: &Mapping) -> Vec<String> {
    svc.get("volumes")
        .and_then(Yaml::as_sequence)
        .map(|v| {
            v.iter()
                .filter_map(|m| match m {
                    Yaml::String(s) => s.split(':').nth(1).map(|t| t.trim_end_matches('/').to_string()),
                    Yaml::Mapping(m) => m.get("target").and_then(Yaml::as_str).map(|t| t.trim_end_matches('/').to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 在 compose 中為 `service` 的 `target` 加上具名 volume；回傳（volume 名稱、修改後的 compose）
pub fn patch(compose: &str, service: &str, target: &str) -> Result<(String, String), String> {
    let target = target.trim_end_matches('/');
    if !target.starts_with('/') || target.len() < 2 || target.contains(':') || target.split('/').any(|x| x == "..") {
        return Err("掛載位置需為容器內的絕對路徑。".into());
    }
    let mut doc: Yaml = serde_yaml::from_str(compose).map_err(|e| format!("compose 格式錯誤：{e}"))?;
    let top = doc.as_mapping_mut().ok_or("compose 格式錯誤。")?;
    let existing: Vec<String> = top
        .get("volumes")
        .and_then(Yaml::as_mapping)
        .map(|m| m.keys().filter_map(|k| k.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let svc = top
        .get_mut("services")
        .and_then(Yaml::as_mapping_mut)
        .and_then(|m| m.get_mut(service))
        .and_then(Yaml::as_mapping_mut)
        .ok_or_else(|| format!("compose 中沒有服務 `{service}`。"))?;
    if mounts(svc).iter().any(|m| target == m || target.starts_with(&format!("{m}/"))) {
        return Err(format!("`{target}` 已經有掛載。"));
    }
    let base: String = service
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = format!("{base}-data");
    let name = std::iter::once(base.clone())
        .chain((2..).map(|i| format!("{base}-{i}")))
        .find(|n| !existing.contains(n))
        .unwrap_or(base);
    let vols = svc.entry("volumes".into()).or_insert_with(|| Yaml::Sequence(vec![]));
    vols.as_sequence_mut()
        .ok_or("服務的 volumes 格式錯誤。")?
        .push(Yaml::String(format!("{name}:{target}")));
    let top_vols = top.entry("volumes".into()).or_insert_with(|| Yaml::Mapping(Mapping::new()));
    if top_vols.is_null() {
        *top_vols = Yaml::Mapping(Mapping::new());
    }
    top_vols
        .as_mapping_mut()
        .ok_or("compose 的 volumes 格式錯誤。")?
        .insert(Yaml::String(name.clone()), Yaml::Mapping(Mapping::new()));
    let out = serde_yaml::to_string(&doc).map_err(|e| e.to_string())?;
    Ok((name, out))
}

/// 確認的解析、它的解析結果與有效 compose
async fn current(s: &AppState, me: &CurrentUser, id: &str) -> ApiResult<(caloud_core::model::AnalysisRow, Analysis, String)> {
    let p = owned(s, me, id).await?;
    let c = deploy::confirmed(s, &p).await?;
    let a: Analysis = serde_json::from_str(&c.result).map_err(|e| ApiError::Internal(e.into()))?;
    let compose = c
        .compose
        .clone()
        .or_else(|| a.compose.clone())
        .ok_or_else(|| ApiError::Conflict("確認的解析沒有 compose。".into()))?;
    Ok((c, a, compose))
}

/// `GET /api/projects/{id}/volume-fix`
pub async fn preview(s: &AppState, me: &CurrentUser, id: &str, service: &str, target: &str) -> ApiResult<Value> {
    let (_, _, before) = current(s, me, id).await?;
    let (volume, after) = patch(&before, service, target).map_err(bad)?;
    Ok(json!({ "volume": volume, "before": before, "after": after }))
}

/// `POST /api/projects/{id}/volume-fix`
pub async fn apply(s: &AppState, me: &CurrentUser, id: &str, input: VolumeInput) -> ApiResult<Value> {
    let (c, a, before) = current(s, me, id).await?;
    let (volume, after) = patch(&before, &input.service, &input.target).map_err(bad)?;
    let depth = a.compose_dir.split('/').filter(|x| !x.is_empty()).count();
    validate_with(&after, Policy::platform(depth)).map_err(|e| bad(e.to_string()))?;
    // 先固定舊驗證的設定指紋，再改確認的 compose
    deploy::freeze_settings(s, &c).await?;
    sqlx::query("UPDATE analyses SET compose = ? WHERE id = ?")
        .bind(&after)
        .bind(&c.id)
        .execute(&s.db)
        .await?;
    audit(
        &s.db,
        Some(&me.0.account),
        "project_volume_fix",
        Some(id),
        me.audit_detail(Some(&format!("{}: {volume}:{}", input.service, input.target)))
            .as_deref(),
    )
    .await;
    let mut out = json!({ "volume": volume, "deployment": null, "verification": null });
    if input.deploy {
        let r = deploy::deploy(s, me, id, DeployInput::default()).await?;
        if r.get("verification").is_some() {
            out["verification"] = r["verification"].clone();
        } else {
            out["deployment"] = r;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_named_volume() {
        let c = "services:\n  web:\n    build: .\n    volumes:\n    - models:/models\nvolumes:\n  models: {}\n  web-data: {}\n";
        let (name, out) = patch(c, "web", "/app/.data/").unwrap();
        assert_eq!(name, "web-data-2", "名稱衝突時加序號");
        let doc: Yaml = serde_yaml::from_str(&out).unwrap();
        let vols: Vec<&str> = doc["services"]["web"]["volumes"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(Yaml::as_str)
            .collect();
        assert_eq!(vols, ["models:/models", "web-data-2:/app/.data"]);
        assert!(doc["volumes"]["web-data-2"].is_mapping());

        let (name, out) = patch("services:\n  api:\n    image: x\n", "api", "/srv/uploads").unwrap();
        assert_eq!(name, "api-data");
        assert!(
            out.contains("api-data:/srv/uploads") && out.contains("volumes:\n  api-data: {}"),
            "{out}"
        );
    }

    #[test]
    fn rejects_bad_targets() {
        let c = "services:\n  web:\n    volumes:\n    - d:/app\n";
        assert!(patch(c, "web", "/app/.data").unwrap_err().contains("已經有掛載"));
        assert!(patch(c, "nope", "/x").unwrap_err().contains("沒有服務"));
        for t in ["relative", "/", "/a:/b", "/a/../etc"] {
            assert!(patch(c, "web", t).is_err(), "{t}");
        }
    }
}
