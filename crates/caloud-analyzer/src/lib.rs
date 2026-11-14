//! 服務自動解析（設計稿 §6，FR-AN-01～06，[ADR 0014](../../docs/adr/0014-analyzer-crate.md)）。
//!
//! 輸入儲存庫目錄與根目錄，輸出通過平臺政策（docs/07 §3.6）的 compose 與解析報告。
//! 只讀檔，不執行儲存庫內任何程式。
#![forbid(unsafe_code)]

mod compose;
mod deps;
pub mod fs;
mod templates;

use fs::RepoFs;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Options {
    /// 子目錄作為專案根目錄（monorepo，FR-AN-06）；"" 為儲存庫根目錄
    pub root: String,
}

/// 信心度（FR-AN-04）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// 判斷依據：儲存庫相對路徑與行號
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub file: String,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// 機器可讀的類型，例：`source`、`port`、`database`、`ports_removed`
    pub key: String,
    pub message: String,
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}

/// 解析來源（設計稿 §6 的順序）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Override { file: String },
    Compose { file: String },
    Dockerfile { files: Vec<String> },
    Template { template: String },
    Static,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    App,
    Database,
    Cache,
    Support,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub role: Role,
    pub port: Option<u16>,
    /// 對外服務（Edge 路由到這個服務）
    pub public: bool,
}

/// 環境變數由誰提供
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// 平臺自動填入（PORT、DATABASE_URL…）
    Platform,
    /// 平臺產生的秘密值（資料庫密碼）
    Generated,
    /// 需要用戶提供
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub provider: Provider,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub source: Source,
    /// 分析根目錄（儲存庫相對路徑）
    pub root: String,
    /// compose 所在目錄（儲存庫相對路徑）；build context 以此為基準
    pub compose_dir: String,
    /// 通過 `prepare_platform` 的 compose；有 `errors` 時為 None
    pub compose: Option<String>,
    pub services: Vec<ServiceInfo>,
    pub env: Vec<EnvVar>,
    pub findings: Vec<Finding>,
    /// 阻擋項：必須修改儲存庫或覆寫 compose 才能繼續
    pub errors: Vec<String>,
    /// 有低信心項目，第一次驗證前需用戶確認（FR-AN-04）
    pub needs_confirmation: bool,
}

/// 解析過程的累積狀態
pub(crate) struct Ctx<'a> {
    pub fs: &'a RepoFs,
    pub findings: Vec<Finding>,
    pub errors: Vec<String>,
}

impl Ctx<'_> {
    pub fn ev(&self, rel: &str, line: Option<u32>) -> Evidence {
        Evidence {
            file: self.fs.display(rel),
            line,
        }
    }

    pub fn note(&mut self, key: &str, confidence: Confidence, message: impl Into<String>, evidence: Vec<Evidence>) {
        self.findings.push(Finding {
            key: key.into(),
            message: message.into(),
            confidence,
            evidence,
        });
    }
}

/// 平臺 compose 的服務名稱與角色
fn image_role(image: &str) -> Role {
    let name = image
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split([':', '@'])
        .next()
        .unwrap_or_default();
    match name {
        "postgres" | "postgis" | "mysql" | "mariadb" | "mongo" | "timescaledb" => Role::Database,
        "redis" | "valkey" | "memcached" | "keydb" => Role::Cache,
        _ => Role::Support,
    }
}

/// 解析儲存庫（FR-AN-01～06）
pub fn analyze(repo: &Path, opts: &Options) -> Analysis {
    let Some(fs) = RepoFs::new(repo, &opts.root) else {
        return Analysis {
            source: Source::None,
            root: opts.root.clone(),
            compose_dir: String::new(),
            compose: None,
            services: vec![],
            env: vec![],
            findings: vec![],
            errors: vec![format!("根目錄 `{}` 不存在或不是儲存庫內的目錄。", opts.root)],
            needs_confirmation: true,
        };
    };
    let mut cx = Ctx {
        fs: &fs,
        findings: vec![],
        errors: vec![],
    };

    let plan = compose::plan(&mut cx);
    let (source, compose_dir, doc, app_env) = (plan.source, plan.compose_dir, plan.doc, plan.app_env);

    // 平臺政策（07 §3.6）：ports、env_file 轉換後檢查
    let depth = compose_dir.split('/').filter(|s| !s.is_empty()).count();
    let mut services = Vec::new();
    let mut compose_out = None;
    if let Some(doc) = &doc {
        match serde_yaml::to_string(doc)
            .map_err(|e| e.to_string())
            .and_then(|y| caloud_orchestrator::prepare_platform(&y, depth).map_err(|e| e.to_string()))
        {
            Ok(p) => {
                let file = compose_file_label(&source);
                let text = compose_text(&cx, &source);
                for (svc, port) in &p.removed_ports {
                    let ev = Evidence {
                        file: file.clone(),
                        line: compose::line_after(&text, svc, port.split(':').next().unwrap_or(port)),
                    };
                    cx.note(
                        "ports_removed",
                        Confidence::High,
                        format!("移除 `{svc}` 的主機 port `{port}`，對外改由平臺路由。"),
                        vec![ev],
                    );
                }
                for (svc, path) in &p.env_files {
                    cx.note(
                        "env_file",
                        Confidence::High,
                        format!("`{svc}` 的 env_file `{path}` 不讀取；變數請改設為平臺秘密變數。"),
                        vec![Evidence {
                            file: file.clone(),
                            line: compose::line_after(&text, svc, "env_file"),
                        }],
                    );
                }
                services = describe_services(&cx, &p.compose.doc, &p.removed_ports, &compose_dir);
                compose_out = Some(p.yaml);
            }
            Err(e) => {
                cx.errors.push(e);
                services = describe_services(&cx, doc, &[], &compose_dir);
            }
        }
    }

    choose_public(&mut cx, &mut services);
    let mut env = deps::scan_env(&mut cx, &app_env);
    if let Some(g) = compose_out.as_deref().and_then(deps::generated) {
        env.push(g);
    }

    let needs_confirmation = !cx.errors.is_empty() || cx.findings.iter().any(|f| f.confidence == Confidence::Low);
    Analysis {
        source,
        root: fs.root().to_string(),
        compose_dir,
        compose: compose_out,
        services,
        env,
        findings: cx.findings,
        errors: cx.errors,
        needs_confirmation,
    }
}

/// 儲存庫中 compose 原文（找依據行號用）
fn compose_text(cx: &Ctx, source: &Source) -> String {
    let file = match source {
        Source::Override { file } | Source::Compose { file } => file,
        _ => return String::new(),
    };
    let rel = file.strip_prefix(cx.fs.root()).unwrap_or(file).trim_start_matches('/');
    cx.fs.read(rel).unwrap_or_default()
}

fn compose_file_label(source: &Source) -> String {
    match source {
        Source::Override { file } | Source::Compose { file } => file.clone(),
        _ => "（產生的 compose）".into(),
    }
}

/// 從 compose 讀出服務、角色與 port
fn describe_services(cx: &Ctx, doc: &Value, removed_ports: &[(String, String)], compose_dir: &str) -> Vec<ServiceInfo> {
    let Some(svcs) = doc.get("services").and_then(Value::as_mapping) else {
        return vec![];
    };
    let mut out = Vec::new();
    for (name, svc) in svcs {
        let name = name.as_str().unwrap_or_default().to_string();
        let empty = Mapping::new();
        let svc = svc.as_mapping().unwrap_or(&empty);
        let has_build = svc.contains_key("build");
        let one_off = svc.contains_key("profiles");
        let role = if one_off {
            Role::Support
        } else if has_build {
            Role::App
        } else {
            image_role(svc.get("image").and_then(Value::as_str).unwrap_or_default())
        };
        let port = removed_ports
            .iter()
            .filter(|(s, _)| *s == name)
            .find_map(|(_, p)| container_port(p))
            .or_else(|| {
                svc.get("expose")
                    .and_then(Value::as_sequence)
                    .and_then(|s| s.first())
                    .and_then(|v| v.as_u64().map(|n| n.to_string()).or(v.as_str().map(str::to_string)))
                    .and_then(|s| container_port(&s))
            })
            .or_else(|| build_expose(cx, svc, compose_dir))
            .or_else(|| {
                svc.get("environment")
                    .and_then(|e| env_value(e, "PORT"))
                    .and_then(|p| p.parse().ok())
            });
        out.push(ServiceInfo {
            name,
            role,
            port,
            public: false,
        });
    }
    out
}

fn env_value(env: &Value, key: &str) -> Option<String> {
    match env {
        Value::Mapping(m) => m.get(key).map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        }),
        Value::Sequence(s) => s
            .iter()
            .filter_map(Value::as_str)
            .find_map(|l| l.strip_prefix(&format!("{key}=")).map(str::to_string)),
        _ => None,
    }
}

/// `3000`、`8080:3000`、`127.0.0.1:80:3000/tcp`、`{target: 5432}` 的容器 port
pub(crate) fn container_port(spec: &str) -> Option<u16> {
    let s = spec.trim();
    if let Some(t) = s.split("target:").nth(1) {
        return t
            .trim()
            .trim_matches(|c: char| !c.is_ascii_digit())
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok();
    }
    let s = s.split('/').next()?;
    s.rsplit(':').next()?.split('-').next()?.parse().ok()
}

/// 由儲存庫內 Dockerfile 的 EXPOSE 取得 port
fn build_expose(cx: &Ctx, svc: &Mapping, compose_dir: &str) -> Option<u16> {
    let build = svc.get("build")?;
    if let Some(inline) = build.get("dockerfile_inline").and_then(Value::as_str) {
        return templates::expose_port(inline);
    }
    let (context, dockerfile) = match build {
        Value::String(c) => (c.as_str(), "Dockerfile"),
        Value::Mapping(m) => (
            m.get("context").and_then(Value::as_str).unwrap_or("."),
            m.get("dockerfile").and_then(Value::as_str).unwrap_or("Dockerfile"),
        ),
        _ => return None,
    };
    // compose_dir 為儲存庫相對路徑；fs 以根目錄為基準
    let root = cx.fs.root();
    let base = compose_dir.strip_prefix(root).unwrap_or(compose_dir).trim_start_matches('/');
    let rel = normalize_join(&[base, context, dockerfile])?;
    templates::expose_port(&cx.fs.read(&rel)?)
}

/// 合併相對路徑並處理 `..`；超出根目錄回傳 None
pub(crate) fn normalize_join(parts: &[&str]) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for p in parts {
        for c in p.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    out.pop()?;
                }
                c => out.push(c),
            }
        }
    }
    Some(out.join("/"))
}

/// 選對外服務（設計稿 §6「對外服務」）
fn choose_public(cx: &mut Ctx, services: &mut [ServiceInfo]) {
    const NAMES: &[&str] = &["web", "frontend", "app", "www", "site", "server", "api"];
    let apps: Vec<usize> = (0..services.len())
        .filter(|&i| services[i].role == Role::App && services[i].port.is_some())
        .collect();
    let (pick, conf) = match apps.len() {
        1 => (Some(apps[0]), Confidence::High),
        0 => {
            let support: Vec<usize> = (0..services.len())
                .filter(|&i| services[i].role == Role::Support && services[i].port.is_some())
                .collect();
            (support.first().copied(), Confidence::Low)
        }
        _ => {
            let by_name = NAMES.iter().find_map(|n| apps.iter().copied().find(|&i| services[i].name == *n));
            match by_name {
                Some(i) => (Some(i), Confidence::Medium),
                None => (Some(apps[0]), Confidence::Low),
            }
        }
    };
    match pick {
        Some(i) => {
            services[i].public = true;
            let s = &services[i];
            cx.note(
                "public_service",
                conf,
                format!("對外服務：`{}`（port {}）。", s.name, s.port.unwrap_or_default()),
                vec![],
            );
        }
        None if !services.is_empty() => {
            cx.note(
                "public_service",
                Confidence::Low,
                "找不到對外的 HTTP 服務，請指定服務與 port。",
                vec![],
            );
        }
        None => {}
    }
}

#[cfg(test)]
mod tests;
