//! 依序找出解析來源並組出 compose（設計稿 §6、ADR 0014）。
use crate::deps;
use crate::templates::{self, Detected};
use crate::{Confidence, Ctx, Source};
use serde_yaml::{Mapping, Value};

pub(crate) struct Plan {
    pub source: Source,
    pub compose_dir: String,
    pub doc: Option<Value>,
    /// 由平臺提供值的環境變數名稱
    pub app_env: Vec<String>,
}

const COMPOSE_NAMES: &[&str] = &["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];
const COMPOSE_DIRS: &[&str] = &["docker", "deploy", ".docker"];
const OVERRIDES: &[&str] = &[".caloud/compose.yml", ".caloud/compose.yaml"];

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}/{b}"),
    }
}

fn parse(cx: &mut Ctx, rel: &str) -> Option<Value> {
    let text = cx.fs.read(rel)?;
    match serde_yaml::from_str::<Value>(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            cx.errors.push(format!("{} 無法解析：{e}", cx.fs.display(rel)));
            None
        }
    }
}

pub(crate) fn plan(cx: &mut Ctx) -> Plan {
    let root = cx.fs.root().to_string();

    // 1. 用戶明確指定（FR-AN-03）
    for f in OVERRIDES {
        if cx.fs.is_file(f) {
            let ev = cx.ev(f, None);
            cx.note(
                "source",
                Confidence::High,
                format!("使用儲存庫中的 `{f}`，不做自動解析。"),
                vec![ev],
            );
            let doc = parse(cx, f);
            return Plan {
                source: Source::Override { file: cx.fs.display(f) },
                compose_dir: root,
                doc,
                app_env: vec![],
            };
        }
    }

    // 2. compose 檔
    let candidates = COMPOSE_NAMES.iter().map(|n| (n.to_string(), Confidence::High)).chain(
        COMPOSE_DIRS
            .iter()
            .flat_map(|d| COMPOSE_NAMES.iter().map(move |n| (format!("{d}/{n}"), Confidence::Medium))),
    );
    for (f, conf) in candidates {
        if !cx.fs.is_file(&f) {
            continue;
        }
        let ev = cx.ev(&f, None);
        cx.note("source", conf, format!("沿用 `{f}` 的服務定義。"), vec![ev]);
        let dir = f.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        let compose_dir = join(&root, &dir);
        let Some(mut doc) = parse(cx, &f) else {
            return Plan {
                source: Source::Compose { file: cx.fs.display(&f) },
                compose_dir,
                doc: None,
                app_env: vec![],
            };
        };
        let text = cx.fs.read(&f).unwrap_or_default();
        normalize(cx, &mut doc, &f, &text);
        let mut app_env = vec![];
        if !has_build(&doc) {
            if let Some(det) = templates::detect(cx) {
                // 合併規則（ADR 0014）：compose 只有附加服務，App 以範本產生
                let up = "../".repeat(dir.split('/').filter(|s| !s.is_empty()).count());
                let context = if up.is_empty() {
                    ".".to_string()
                } else {
                    up.trim_end_matches('/').to_string()
                };
                cx.note(
                    "merge",
                    Confidence::Medium,
                    format!("`{f}` 沒有建置 App 的服務，以 {} 範本產生 App 並與其合併。", det.kind),
                    vec![],
                );
                app_env = add_app(cx, &mut doc, det, &context);
            }
        }
        return Plan {
            source: Source::Compose { file: cx.fs.display(&f) },
            compose_dir,
            doc: Some(doc),
            app_env,
        };
    }

    // 3. Dockerfile：每個一個服務
    let dockerfiles: Vec<String> = cx
        .fs
        .list("")
        .into_iter()
        .filter(|(n, d)| !d && (n == "Dockerfile" || n.starts_with("Dockerfile.") || n.ends_with(".Dockerfile")))
        .map(|(n, _)| n)
        .collect();
    if !dockerfiles.is_empty() {
        let mut services = Mapping::new();
        for f in &dockerfiles {
            let name = service_name_for(f);
            let mut build = Mapping::new();
            build.insert("context".into(), ".".into());
            if f != "Dockerfile" {
                build.insert("dockerfile".into(), f.as_str().into());
            }
            let mut svc = Mapping::new();
            svc.insert("build".into(), Value::Mapping(build));
            let content = cx.fs.read(f).unwrap_or_default();
            let ev = cx.ev(f, crate::fs::line_of(&content, "EXPOSE"));
            match templates::expose_port(&content) {
                Some(p) => cx.note("port", Confidence::High, format!("`{name}` 以 EXPOSE 使用 port {p}。"), vec![ev]),
                None => cx.note(
                    "port",
                    Confidence::Low,
                    format!("`{f}` 沒有 EXPOSE，請指定 `{name}` 的 port。"),
                    vec![ev],
                ),
            }
            services.insert(name.into(), Value::Mapping(svc));
        }
        let mut doc = Mapping::new();
        doc.insert("services".into(), Value::Mapping(services));
        let mut doc = Value::Mapping(doc);
        let first = service_name_for(&dockerfiles[0]);
        let ev = cx.ev(&dockerfiles[0], None);
        cx.note(
            "source",
            Confidence::High,
            format!("以儲存庫中的 {} 個 Dockerfile 建置。", dockerfiles.len()),
            vec![ev],
        );
        // 依賴的資料庫仍依語言標記檔判斷（不使用範本的 Dockerfile）
        let needs = templates::detect(cx).map(|d| d.databases).unwrap_or_default();
        let app_env = deps::add_databases(cx, &mut doc, &first, &needs);
        return Plan {
            source: Source::Dockerfile {
                files: dockerfiles.iter().map(|f| cx.fs.display(f)).collect(),
            },
            compose_dir: root,
            doc: Some(doc),
            app_env,
        };
    }

    // 4、5. 語言範本與靜態網站
    if let Some(det) = templates::detect(cx) {
        let source = if det.kind == "static" {
            Source::Static
        } else {
            Source::Template {
                template: det.kind.clone(),
            }
        };
        let mut doc = Value::Mapping(Mapping::from_iter([("services".into(), Value::Mapping(Mapping::new()))]));
        let app_env = add_app(cx, &mut doc, det, ".");
        return Plan {
            source,
            compose_dir: root,
            doc: Some(doc),
            app_env,
        };
    }

    // 找不到：建議子目錄（FR-AN-06）
    suggest_root(cx);
    Plan {
        source: Source::None,
        compose_dir: root,
        doc: None,
        app_env: vec![],
    }
}

fn service_name_for(dockerfile: &str) -> String {
    let n = dockerfile
        .strip_prefix("Dockerfile.")
        .or_else(|| dockerfile.strip_suffix(".Dockerfile"))
        .unwrap_or("web");
    let n: String = n
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if n.is_empty() {
        "web".into()
    } else {
        n
    }
}

fn has_build(doc: &Value) -> bool {
    doc.get("services")
        .and_then(Value::as_mapping)
        .is_some_and(|m| m.values().any(|s| s.get("build").is_some()))
}

/// 加入範本產生的 App 服務與其依賴的資料庫；回傳平臺提供的變數名稱
fn add_app(cx: &mut Ctx, doc: &mut Value, det: Detected, context: &str) -> Vec<String> {
    cx.findings.extend(det.findings.iter().cloned());
    let Some(services) = doc.get_mut("services").and_then(Value::as_mapping_mut) else {
        return vec![];
    };
    let name = ["web", "app", "main"]
        .into_iter()
        .find(|n| !services.contains_key(*n))
        .unwrap_or("caloud-app")
        .to_string();
    let mut build = Mapping::new();
    build.insert("context".into(), context.into());
    build.insert("dockerfile_inline".into(), det.dockerfile.as_str().into());
    let mut svc = Mapping::new();
    svc.insert("build".into(), Value::Mapping(build));
    let mut env = Mapping::new();
    for (k, v) in &det.env {
        env.insert(k.as_str().into(), v.as_str().into());
    }
    if !env.is_empty() {
        svc.insert("environment".into(), Value::Mapping(env));
    }
    services.insert(name.as_str().into(), Value::Mapping(svc));
    let mut provided: Vec<String> = det.env.iter().map(|(k, _)| k.clone()).collect();
    provided.extend(deps::add_databases(cx, doc, &name, &det.databases));
    provided
}

/// 儲存庫 compose 的平臺化前處理：移除 `version`、`container_name`，相對路徑掛載改具名 volume
fn normalize(cx: &mut Ctx, doc: &mut Value, file: &str, text: &str) {
    let Some(top) = doc.as_mapping_mut() else { return };
    top.remove("version");
    top.remove("name");
    let mut new_volumes: Vec<String> = Vec::new();
    if let Some(services) = top.get_mut("services").and_then(Value::as_mapping_mut) {
        for (name, svc) in services.iter_mut() {
            let name = name.as_str().unwrap_or_default().to_string();
            let Some(svc) = svc.as_mapping_mut() else { continue };
            if svc.remove("container_name").is_some() {
                let ev = cx.ev(file, line_after(text, &name, "container_name"));
                cx.note(
                    "container_name_removed",
                    Confidence::High,
                    format!("移除 `{name}` 的 container_name，容器名稱由平臺命名。"),
                    vec![ev],
                );
            }
            let Some(vols) = svc.get_mut("volumes").and_then(Value::as_sequence_mut) else {
                continue;
            };
            for v in vols.iter_mut() {
                let (source, target, rest) = match &*v {
                    Value::String(s) => {
                        let mut parts = s.splitn(3, ':');
                        let src = parts.next().unwrap_or_default().to_string();
                        let Some(tgt) = parts.next() else { continue };
                        (src, tgt.to_string(), parts.next().map(str::to_string))
                    }
                    Value::Mapping(m) if m.get("type").and_then(Value::as_str) == Some("bind") => (
                        m.get("source").and_then(Value::as_str).unwrap_or_default().to_string(),
                        m.get("target").and_then(Value::as_str).unwrap_or_default().to_string(),
                        None,
                    ),
                    _ => continue,
                };
                if !source.starts_with('.') || source.contains('$') || target.is_empty() {
                    continue;
                }
                let vol = volume_name(&name, &source);
                *v = match rest {
                    Some(mode) => Value::String(format!("{vol}:{target}:{mode}")),
                    None => Value::String(format!("{vol}:{target}")),
                };
                let ev = cx.ev(file, None);
                cx.note(
                    "bind_to_volume",
                    Confidence::Medium,
                    format!(
                        "`{name}` 的主機路徑掛載 `{source}` 改為具名 volume `{vol}`；若需要儲存庫內的檔案，請改在 Dockerfile 中 COPY。"
                    ),
                    vec![ev],
                );
                let ev2 = cx.ev(file, line_after(text, &name, &source));
                if let Some(f) = cx.findings.last_mut() {
                    f.evidence = vec![ev2];
                }
                new_volumes.push(vol);
            }
        }
    }
    if !new_volumes.is_empty() {
        let vols = top.entry("volumes".into()).or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Some(m) = vols.as_mapping_mut() {
            for v in new_volumes {
                m.entry(v.into()).or_insert_with(|| Value::Mapping(Mapping::new()));
            }
        }
    }
}

/// 服務 `service:` 區塊之後第一個包含 `needle` 的行號
pub(crate) fn line_after(text: &str, service: &str, needle: &str) -> Option<u32> {
    let key = format!("{service}:");
    let start = text
        .lines()
        .position(|l| l.trim_start() == key || l.trim_start().starts_with(&format!("{key} ")))?;
    text.lines()
        .enumerate()
        .skip(start)
        .find(|(_, l)| l.contains(needle))
        .map(|(i, _)| i as u32 + 1)
}

fn volume_name(service: &str, source: &str) -> String {
    let base = source.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    let clean: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if clean.is_empty() {
        format!("{service}-data")
    } else {
        format!("{service}-{clean}")
    }
}

/// 根目錄沒有可解析的檔案時，找第一層子目錄並建議（FR-AN-06）
fn suggest_root(cx: &mut Ctx) {
    const MARKERS: &[&str] = &[
        ".caloud/compose.yml",
        "compose.yaml",
        "docker-compose.yml",
        "Dockerfile",
        "package.json",
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        "requirements.txt",
    ];
    let dirs: Vec<String> = cx
        .fs
        .list("")
        .into_iter()
        .filter(|(n, d)| *d && !n.starts_with('.') && !crate::fs::SKIP_DIRS.contains(&n.as_str()))
        .map(|(n, _)| n)
        .collect();
    let mut found = Vec::new();
    for d in dirs {
        if let Some(m) = MARKERS.iter().find(|m| cx.fs.is_file(&format!("{d}/{m}"))) {
            found.push((d.clone(), cx.ev(&format!("{d}/{m}"), None)));
        }
    }
    if found.is_empty() {
        cx.errors
            .push("找不到可解析的服務：請加入 compose 檔、Dockerfile，或在 `.caloud/compose.yml` 指定。".into());
        return;
    }
    let names: Vec<String> = found.iter().map(|(d, _)| format!("`{}/`", cx.fs.display(d))).collect();
    cx.note(
        "suggest_root",
        Confidence::Low,
        format!("根目錄沒有可解析的檔案；建議把專案根目錄設為 {}。", names.join("、")),
        found.into_iter().map(|(_, e)| e).collect(),
    );
    cx.errors.push("請指定專案根目錄後重新解析。".into());
}
