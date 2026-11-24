//! GPU 偵測（FR-ENV-13，ADR 0022）
//!
//! - compose 已宣告 GPU：說明分時共用；
//! - 解析器產生的 compose（Dockerfile、語言範本）：依 CUDA 基底映像與 GPU 依賴加上 GPU 宣告，列為低信心待確認；
//! - 用戶自己的 compose 沒宣告但有訊號：建議自行加上，不修改用戶檔案。
use crate::{build_dockerfile, runtime, Confidence, Ctx, Evidence, Source};
use serde_yaml::{Mapping, Value};

/// GPU 依賴（套件名稱或前綴）
const GPU_DEPS: &[&str] = &["onnxruntime-gpu", "onnxruntime-node-gpu", "@tensorflow/tfjs-node-gpu"];
const GPU_DEP_PREFIXES: &[&str] = &["cupy-cuda"];

/// Dockerfile 的 `FROM` 是否為 CUDA 基底映像
pub(crate) fn cuda_base(line: &str) -> bool {
    let l = line.trim().to_ascii_lowercase();
    let Some(rest) = l.strip_prefix("from ") else { return false };
    let image = rest.split_whitespace().find(|w| !w.starts_with("--")).unwrap_or_default();
    image.contains("nvidia/cuda")
        || image.starts_with("nvcr.io/nvidia/")
        || (image.starts_with("pytorch/pytorch") && image.contains("cuda"))
        || (image.starts_with("tensorflow/tensorflow") && image.contains("gpu"))
}

/// 服務是否已宣告 GPU
pub(crate) fn declared(svc: &Mapping) -> bool {
    svc.get("deploy")
        .and_then(|d| d.get("resources"))
        .and_then(|r| r.get("reservations"))
        .and_then(|r| r.get("devices"))
        .is_some()
}

fn reservation() -> Value {
    let mut dev = Mapping::new();
    dev.insert("capabilities".into(), Value::Sequence(vec!["gpu".into()]));
    let mut r = Mapping::new();
    r.insert("devices".into(), Value::Sequence(vec![Value::Mapping(dev)]));
    let mut res = Mapping::new();
    res.insert("reservations".into(), Value::Mapping(r));
    let mut d = Mapping::new();
    d.insert("resources".into(), Value::Mapping(res));
    Value::Mapping(d)
}

const SHARED: &str = "GPU 由同一節點上的環境分時共用，顯示記憶體不隔離。";

pub(crate) fn apply(cx: &mut Ctx, doc: &mut Value, source: &Source, compose_dir: &str) {
    let Some(svcs) = doc.get_mut("services").and_then(Value::as_mapping_mut) else {
        return;
    };
    let generated = matches!(source, Source::Dockerfile { .. } | Source::Template { .. });
    let dep_ev: Vec<(String, Evidence)> = runtime::repo_deps(cx)
        .map(|(_, deps)| {
            deps.into_iter()
                .filter(|(n, _)| GPU_DEPS.contains(&n.as_str()) || GPU_DEP_PREFIXES.iter().any(|p| n.starts_with(p)))
                .collect()
        })
        .unwrap_or_default();
    let mut first_app = true;
    for (name, svc) in svcs.iter_mut() {
        let name = name.as_str().unwrap_or_default().to_string();
        let Some(svc) = svc.as_mapping_mut() else { continue };
        if declared(svc) {
            cx.note(
                "gpu",
                Confidence::High,
                format!("`{name}` 宣告使用 GPU，會排到 GPU 節點以 gVisor nvproxy 執行。{SHARED}"),
                vec![],
            );
            continue;
        }
        if !svc.contains_key("build") || svc.contains_key("profiles") {
            continue;
        }
        // 訊號：Dockerfile 的 CUDA 基底映像；語言依賴只算在第一個 App 服務
        let mut ev = Vec::new();
        let mut what = Vec::new();
        if let Some((file, text)) = build_dockerfile(cx, svc, compose_dir) {
            if let Some((i, l)) = text.lines().enumerate().find(|(_, l)| cuda_base(l)) {
                ev.push(Evidence {
                    file,
                    line: Some(i as u32 + 1),
                });
                what.push(format!(
                    "CUDA 基底映像 `{}`",
                    l.trim().chars().skip(5).take(80).collect::<String>().trim()
                ));
            }
        }
        if first_app {
            for (dep, e) in &dep_ev {
                ev.push(e.clone());
                what.push(format!("依賴 `{dep}`"));
            }
        }
        first_app = false;
        if ev.is_empty() {
            continue;
        }
        if generated {
            svc.insert("deploy".into(), reservation());
            cx.note(
                "gpu",
                Confidence::Low,
                format!(
                    "偵測到 {}，已為 `{name}` 加上 GPU 宣告。{SHARED}不需要 GPU 時請在 compose 移除 `deploy.resources.reservations.devices`。",
                    what.join("、")
                ),
                ev,
            );
        } else {
            cx.note(
                "gpu",
                Confidence::Medium,
                format!(
                    "偵測到 {}；若 `{name}` 需要 GPU，請在 compose 加上 `deploy.resources.reservations.devices: [{{ capabilities: [gpu] }}]`。",
                    what.join("、")
                ),
                ev,
            );
        }
    }
}
