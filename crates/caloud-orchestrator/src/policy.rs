use serde_yaml::{Mapping, Value};

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("YAML 解析失敗：{0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("compose 檔沒有 services，請至少定義一個 service。")]
    NoServices,
    #[error("service `{service}`：不允許使用 `{key}`，請移除此設定。")]
    Forbidden { service: String, key: String },
    #[error("service `{service}`：不可掛載主機路徑 `{path}`，請改用具名 volume。")]
    HostMount { service: String, path: String },
    #[error("service `{0}`：請指定 image，Caloud 不支援 build。")]
    NoImage(String),
    #[error("頂層 `{0}` 不可引用主機檔案或外部資源，請移除相關設定。")]
    HostFile(String),
}

/// 通過檢查的 compose 檔與其 service 名稱
#[derive(Debug, Clone)]
pub struct ValidatedCompose {
    pub doc: Value,
    pub services: Vec<String>,
}

/// 會讓容器取得主機權限或與其他租戶衝突的鍵
const FORBIDDEN_KEYS: &[&str] = &[
    "privileged",
    "cap_add",
    "devices",
    "device_cgroup_rules",
    "security_opt",
    "userns_mode",
    "cgroup_parent",
    "container_name", // 由 Caloud 依 project 命名，避免撞名
    "ports",          // 一律經由 Caloud 反向代理對外
    "build",
    "extends",
    "env_file", // 會讀取主機檔案
    "sysctls",
    "volumes_from",
];

/// 不可設為 `host` 或指向其他容器的 namespace
const NAMESPACE_KEYS: &[&str] = &["network_mode", "pid", "ipc", "uts", "cgroup"];

/// FR-APP-02：規則清單見 docs/07-security.md §3
pub fn validate(yaml: &str) -> Result<ValidatedCompose, PolicyError> {
    let doc: Value = serde_yaml::from_str(yaml)?;
    let services = doc.get("services").and_then(Value::as_mapping).ok_or(PolicyError::NoServices)?;
    let mut names = Vec::new();

    for (name, svc) in services {
        let name = name.as_str().unwrap_or_default().to_string();
        let svc = svc.as_mapping().cloned().unwrap_or_default();
        check_service(&name, &svc)?;
        names.push(name);
    }

    for top in ["secrets", "configs"] {
        if let Some(m) = doc.get(top).and_then(Value::as_mapping) {
            if m.values().any(|v| v.get("file").is_some()) {
                return Err(PolicyError::HostFile(top.into()));
            }
        }
    }
    if let Some(vols) = doc.get("volumes").and_then(Value::as_mapping) {
        // 具名 volume 不可用 driver_opts 綁到主機路徑
        if vols.values().any(|v| v.get("driver_opts").is_some() || v.get("external").is_some()) {
            return Err(PolicyError::HostFile("volumes".into()));
        }
    }
    if doc
        .get("networks")
        .and_then(Value::as_mapping)
        .is_some_and(|m| m.values().any(|v| v.get("external").is_some()))
    {
        return Err(PolicyError::HostFile("networks".into()));
    }
    if doc.get("include").is_some() {
        return Err(PolicyError::HostFile("include".into()));
    }

    Ok(ValidatedCompose { doc, services: names })
}

fn check_service(name: &str, svc: &Mapping) -> Result<(), PolicyError> {
    let forbid = |key: &str| PolicyError::Forbidden {
        service: name.into(),
        key: key.into(),
    };
    for k in FORBIDDEN_KEYS {
        if svc.contains_key(*k) {
            return Err(forbid(k));
        }
    }
    for k in NAMESPACE_KEYS {
        if svc.get(*k).is_some() {
            return Err(forbid(k));
        }
    }
    if svc.get("image").and_then(Value::as_str).is_none() {
        return Err(PolicyError::NoImage(name.into()));
    }
    if let Some(vols) = svc.get("volumes").and_then(Value::as_sequence) {
        for v in vols {
            let (kind, source) = match v {
                Value::String(s) => {
                    let src = s.split(':').next().unwrap_or_default();
                    let kind = if s.contains(':') && (src.starts_with('/') || src.starts_with('.') || src.starts_with('~')) {
                        "bind"
                    } else {
                        "volume"
                    };
                    (kind.to_string(), src.to_string())
                }
                Value::Mapping(m) => (
                    m.get("type").and_then(Value::as_str).unwrap_or("volume").to_string(),
                    m.get("source").and_then(Value::as_str).unwrap_or_default().to_string(),
                ),
                _ => continue,
            };
            if kind != "volume" && kind != "tmpfs" {
                return Err(PolicyError::HostMount {
                    service: name.into(),
                    path: source,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-APP-02
    #[test]
    fn accepts_simple_app() {
        let y = "services:\n  web:\n    image: nginx:alpine\n    volumes: [data:/usr/share/nginx/html]\n  db:\n    image: postgres:17\nvolumes:\n  data: {}\n";
        let c = validate(y).unwrap();
        assert_eq!(c.services, vec!["web", "db"]);
    }

    // FR-APP-02
    #[test]
    fn rejects_escape_hatches() {
        for y in [
            "services: {a: {image: x, privileged: true}}",
            "services: {a: {image: x, network_mode: host}}",
            "services: {a: {image: x, volumes: ['/var/run/docker.sock:/var/run/docker.sock']}}",
            "services: {a: {image: x, volumes: ['./src:/app']}}",
            "services: {a: {image: x, volumes: [{type: bind, source: /etc, target: /x}]}}",
            "services: {a: {image: x, ports: ['80:80']}}",
            "services: {a: {build: .}}",
        ] {
            assert!(validate(y).is_err(), "should reject: {y}");
        }
    }
}
