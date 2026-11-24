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
    #[error("service `{service}`：{msg}")]
    Privileged { service: String, msg: String },
    #[error("service `{0}`：GPU 只支援 `deploy.resources.reservations.devices: [{{ capabilities: [gpu] }}]`（driver 只能是 nvidia），且只限從儲存庫部署的專案。")]
    Gpu(String),
    #[error("頂層 `{0}` 不可引用主機檔案或外部資源，請移除相關設定。")]
    HostFile(String),
    #[error("不可使用 YAML tag `{0}`，請改寫為一般值。")]
    Tag(String),
    #[error("service `{service}`：`{key}` 不可使用 `${{VAR}}` 變數替換。")]
    Interpolated { service: String, key: String },
}

/// 通過檢查的 compose 檔與其 service 名稱
#[derive(Debug, Clone)]
pub struct ValidatedCompose {
    pub doc: Value,
    pub services: Vec<String>,
    /// 宣告 GPU 的服務（ADR 0022）
    pub gpu_services: Vec<String>,
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

/// 特權 App 額外允許的鍵（FR-APP-16，docs/07-security.md §3.5）
const PRIVILEGED_ALLOWED: &[&str] = &["build", "network_mode", "ports", "env_file", "tmpfs"];

/// 檢查選項
#[derive(Debug, Clone, Copy, Default)]
pub struct Policy {
    /// 特權 App（FR-APP-16）
    pub privileged: bool,
    /// Git 來源時，compose 目錄在儲存庫中的深度（`deploy/livekit` = 2）；None 表示上傳的 compose
    pub repo_depth: Option<usize>,
    /// 平臺 App（M5-06，docs/07-security.md §3.6）：允許儲存庫內 build，其餘同一般 App
    pub platform: bool,
}

impl Policy {
    /// 平臺 App 政策；`ports`、`env_file` 須先經 [`prepare_platform`] 轉換
    pub fn platform(repo_depth: usize) -> Self {
        Self {
            privileged: false,
            repo_depth: Some(repo_depth),
            platform: true,
        }
    }
}

/// [`prepare_platform`] 的結果
#[derive(Debug, Clone)]
pub struct PlatformCompose {
    pub compose: ValidatedCompose,
    /// 轉換後的 compose
    pub yaml: String,
    /// 被移除的 `ports`：(service, 原設定)；對外改由 Edge 路由
    pub removed_ports: Vec<(String, String)>,
    /// 被移除的 `env_file`：(service, 儲存庫內路徑)；變數名稱列入盤點，值由平臺秘密變數注入
    pub env_files: Vec<(String, String)>,
}

/// M5-06：把儲存庫裡的 compose 轉成平臺 App（docs/07-security.md §3.6）。
///
/// 移除 `ports` 與 `env_file` 並記錄下來，再以 [`Policy::platform`] 檢查其餘規則。
pub fn prepare_platform(yaml: &str, repo_depth: usize) -> Result<PlatformCompose, PolicyError> {
    let mut doc: Value = serde_yaml::from_str(yaml)?;
    doc.apply_merge()?;
    if let Some(tag) = find_tag(&doc) {
        return Err(PolicyError::Tag(tag));
    }
    let services = doc
        .get_mut("services")
        .and_then(Value::as_mapping_mut)
        .ok_or(PolicyError::NoServices)?;
    let mut removed_ports = Vec::new();
    let mut env_files = Vec::new();
    for (name, svc) in services.iter_mut() {
        let name = name.as_str().unwrap_or_default().to_string();
        let Some(svc) = svc.as_mapping_mut() else { continue };
        if let Some(ports) = svc.remove("ports") {
            for p in ports.as_sequence().cloned().unwrap_or_default() {
                let text = match &p {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    other => serde_yaml::to_string(other)?.trim().replace('\n', ", "),
                };
                removed_ports.push((name.clone(), text));
            }
        }
        if let Some(env_file) = svc.remove("env_file") {
            let paths: Vec<String> = match &env_file {
                Value::String(p) => vec![p.clone()],
                Value::Sequence(list) => list
                    .iter()
                    .filter_map(|v| v.as_str().or_else(|| v.get("path").and_then(Value::as_str)))
                    .map(str::to_string)
                    .collect(),
                _ => vec![],
            };
            for p in paths {
                no_var(&name, "env_file", &p)?;
                within_repo(&p, repo_depth).ok_or_else(|| PolicyError::Privileged {
                    service: name.clone(),
                    msg: format!("env_file `{p}` 必須是儲存庫內的相對路徑。"),
                })?;
                env_files.push((name.clone(), p));
            }
        }
    }
    let out = serde_yaml::to_string(&doc)?;
    let compose = validate_with(&out, Policy::platform(repo_depth))?;
    Ok(PlatformCompose {
        compose,
        yaml: out,
        removed_ports,
        env_files,
    })
}

/// FR-APP-02：一般 App 的規則，清單見 docs/07-security.md §3
pub fn validate(yaml: &str) -> Result<ValidatedCompose, PolicyError> {
    validate_with(yaml, Policy::default())
}

/// FR-APP-02、FR-APP-16：依選項檢查
pub fn validate_with(yaml: &str, policy: Policy) -> Result<ValidatedCompose, PolicyError> {
    let mut doc: Value = serde_yaml::from_str(yaml)?;
    // 先展開 `<<: *anchor`，避免禁用鍵藏在 anchor 中（docs/07 §3.3）
    doc.apply_merge()?;
    if let Some(tag) = find_tag(&doc) {
        return Err(PolicyError::Tag(tag));
    }
    let services = doc.get("services").and_then(Value::as_mapping).ok_or(PolicyError::NoServices)?;
    let mut names = Vec::new();
    let mut gpu_services = Vec::new();

    for (name, svc) in services {
        let name = name.as_str().unwrap_or_default().to_string();
        let svc = svc.as_mapping().cloned().unwrap_or_default();
        if check_service(&name, &svc, policy)? {
            gpu_services.push(name.clone());
        }
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

    Ok(ValidatedCompose {
        doc,
        services: names,
        gpu_services,
    })
}

/// 找出文件中的第一個 YAML tag（`!reset`、`!override` 會改變型別而繞過檢查）
fn find_tag(v: &Value) -> Option<String> {
    match v {
        Value::Tagged(t) => Some(t.tag.to_string()),
        Value::Sequence(s) => s.iter().find_map(find_tag),
        Value::Mapping(m) => m.iter().find_map(|(k, v)| find_tag(k).or_else(|| find_tag(v))),
        _ => None,
    }
}

/// 檢查欄位不可使用 `${VAR}`：值會在 compose 執行時被 App env 或主機環境變數改寫
fn no_var(service: &str, key: &str, s: &str) -> Result<(), PolicyError> {
    if s.contains('$') {
        return Err(PolicyError::Interpolated {
            service: service.into(),
            key: key.into(),
        });
    }
    Ok(())
}

/// 相對路徑是否留在儲存庫內：從 compose 目錄（深度 `depth`）出發，`..` 不可超出根目錄
fn within_repo(path: &str, depth: usize) -> Option<usize> {
    if path.starts_with('/') || path.starts_with('~') || path.contains('\\') || path.contains("://") {
        return None;
    }
    let mut d = depth as isize;
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                d -= 1;
                if d < 0 {
                    return None;
                }
            }
            _ => d += 1,
        }
    }
    Some(d as usize)
}

/// build 只能用儲存庫內的路徑（特權 App 與平臺 App 共用）
fn check_build(name: &str, build: &Value, policy: Policy) -> Result<(), PolicyError> {
    let err = |msg: String| PolicyError::Privileged { service: name.into(), msg };
    let depth = policy
        .repo_depth
        .ok_or_else(|| err("只有 Git 來源的 App 可以使用 build。".into()))?;
    let (context, dockerfile) = match build {
        Value::String(c) => (c.as_str(), None),
        Value::Mapping(m) => {
            for k in ["additional_contexts", "secrets", "ssh"] {
                if m.contains_key(k) {
                    return Err(err(format!("build 不可使用 `{k}`。")));
                }
            }
            (
                m.get("context").and_then(Value::as_str).unwrap_or("."),
                m.get("dockerfile").and_then(Value::as_str),
            )
        }
        _ => return Err(err("build 格式錯誤。".into())),
    };
    no_var(name, "build.context", context)?;
    no_var(name, "build.dockerfile", dockerfile.unwrap_or_default())?;
    let ctx_depth = within_repo(context, depth).ok_or_else(|| err(format!("build context `{context}` 必須是儲存庫內的相對路徑。")))?;
    if let Some(df) = dockerfile {
        within_repo(df, ctx_depth).ok_or_else(|| err(format!("dockerfile `{df}` 必須是儲存庫內的相對路徑。")))?;
    }
    Ok(())
}

fn check_privileged(name: &str, svc: &Mapping, policy: Policy) -> Result<(), PolicyError> {
    let err = |msg: String| PolicyError::Privileged { service: name.into(), msg };
    if let Some(mode) = svc.get("network_mode") {
        if mode.as_str() != Some("host") {
            return Err(err("network_mode 只能是 host。".into()));
        }
    }
    if let Some(build) = svc.get("build") {
        check_build(name, build, policy)?;
    }
    if let Some(env_file) = svc.get("env_file") {
        let depth = policy
            .repo_depth
            .ok_or_else(|| err("只有 Git 來源的 App 可以使用 env_file。".into()))?;
        let paths: Vec<&str> = match env_file {
            Value::String(p) => vec![p.as_str()],
            Value::Sequence(list) => list
                .iter()
                .filter_map(|v| v.as_str().or_else(|| v.get("path").and_then(Value::as_str)))
                .collect(),
            _ => vec![],
        };
        for p in paths {
            no_var(name, "env_file", p)?;
            within_repo(p, depth).ok_or_else(|| err(format!("env_file `{p}` 必須是儲存庫內的相對路徑。")))?;
        }
    }
    Ok(())
}

/// 回傳這個服務是否宣告 GPU
fn check_service(name: &str, svc: &Mapping, policy: Policy) -> Result<bool, PolicyError> {
    let forbid = |key: &str| PolicyError::Forbidden {
        service: name.into(),
        key: key.into(),
    };
    let allowed = |k: &str| (policy.privileged && PRIVILEGED_ALLOWED.contains(&k)) || (policy.platform && k == "build");
    for k in FORBIDDEN_KEYS {
        if svc.contains_key(*k) && !allowed(k) {
            return Err(forbid(k));
        }
    }
    for k in NAMESPACE_KEYS {
        if svc.get(*k).is_some() && !allowed(k) {
            return Err(forbid(k));
        }
    }
    if policy.privileged {
        check_privileged(name, svc, policy)?;
    } else if let Some(build) = svc.get("build").filter(|_| policy.platform) {
        check_build(name, build, policy)?;
    }
    // GPU 只給平臺 App，且只接受 capabilities: [gpu]（ADR 0022，docs/07 §3.6）
    let gpu = match svc
        .get("deploy")
        .and_then(|d| d.get("resources"))
        .and_then(|r| r.get("reservations"))
        .and_then(|r| r.get("devices"))
    {
        None => false,
        Some(d) if policy.platform && !policy.privileged && gpu_devices_ok(d) => true,
        Some(_) => return Err(PolicyError::Gpu(name.into())),
    };
    if svc.get("image").and_then(Value::as_str).is_none() && !svc.contains_key("build") {
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
                _ => {
                    return Err(PolicyError::HostMount {
                        service: name.into(),
                        path: format!("{v:?}"),
                    })
                }
            };
            no_var(name, "volumes", &kind)?;
            no_var(name, "volumes", &source)?;
            if kind != "volume" && kind != "tmpfs" {
                return Err(PolicyError::HostMount {
                    service: name.into(),
                    path: source,
                });
            }
        }
    }
    Ok(gpu)
}

/// GPU 宣告：每一項都只能是 `capabilities: [gpu]`（可另帶 `driver: nvidia`、`count`、`device_ids`、`options`，
/// override 一律清掉宣告、改以 CDI 裝置 `caloud.dev/gpu=all` 掛入，ADR 0022）
fn gpu_devices_ok(d: &Value) -> bool {
    let Some(list) = d.as_sequence().filter(|l| !l.is_empty()) else {
        return false;
    };
    list.iter().all(|item| {
        let Some(m) = item.as_mapping() else { return false };
        let caps_ok = m
            .get("capabilities")
            .and_then(Value::as_sequence)
            .is_some_and(|c| !c.is_empty() && c.iter().all(|v| v.as_str() == Some("gpu")));
        let driver_ok = m.get("driver").is_none_or(|v| v.as_str() == Some("nvidia"));
        let keys_ok = m
            .keys()
            .all(|k| matches!(k.as_str(), Some("capabilities" | "driver" | "count" | "device_ids" | "options")));
        caps_ok && driver_ok && keys_ok
    })
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

    // FR-APP-16
    #[test]
    fn privileged_policy_allows_host_services_but_not_escapes() {
        let git = Policy {
            privileged: true,
            repo_depth: Some(2),
            ..Default::default()
        };
        let livekit = "services:\n  init:\n    build: .\n    env_file: .env\n    restart: 'no'\n  caddy:\n    image: caddy:2\n    network_mode: host\n  lk:\n    build: .\n    network_mode: host\n    tmpfs: [/run/livekit]\n";
        assert!(validate_with(livekit, git).is_ok());
        let relay = "services:\n  relay:\n    build: {context: ., dockerfile: Dockerfile.relay}\n    ports: ['80:80', '7842:7842/udp']\n  mailbox:\n    build: {context: .., dockerfile: apps/mailbox/Dockerfile}\n";
        assert!(validate_with(
            relay,
            Policy {
                privileged: true,
                repo_depth: Some(1),
                ..Default::default()
            }
        )
        .is_ok());
        // 一般 App 仍不可
        assert!(validate(livekit).is_err());
        // 上傳的 compose 即使特權也不能 build
        assert!(validate_with(
            livekit,
            Policy {
                privileged: true,
                repo_depth: None,
                ..Default::default()
            }
        )
        .is_err());
        for y in [
            "services: {a: {image: x, privileged: true}}",
            "services: {a: {image: x, network_mode: bridge}}",
            "services: {a: {image: x, pid: host}}",
            "services: {a: {image: x, cap_add: [NET_ADMIN]}}",
            "services: {a: {image: x, volumes: ['/var/run/docker.sock:/var/run/docker.sock']}}",
            "services: {a: {build: ../../..}}",
            "services: {a: {build: {context: ., dockerfile: ../../../etc/x}}}",
            "services: {a: {build: /etc}}",
            "services: {a: {build: {context: ., ssh: [default]}}}",
            "services: {a: {image: x, env_file: ../../../secret.env}}",
        ] {
            assert!(validate_with(y, git).is_err(), "should reject: {y}");
        }
    }

    #[test]
    fn within_repo_tracks_depth() {
        assert_eq!(within_repo("..", 1), Some(0));
        assert_eq!(within_repo("../..", 1), None);
        assert_eq!(within_repo("./a/../b", 0), Some(1));
        assert_eq!(within_repo("/abs", 3), None);
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

    // FR-APP-02：anchor 合併後再檢查；tag 與變數替換不可繞過
    #[test]
    fn merge_keys_tags_and_vars_cannot_hide_keys() {
        let hidden = "x-base: &b\n  privileged: true\nservices:\n  web:\n    <<: *b\n    image: nginx\n";
        assert!(matches!(validate(hidden), Err(PolicyError::Forbidden { .. })));
        let ok = "x-base: &b\n  image: nginx\n  restart: always\nservices:\n  web:\n    <<: *b\n";
        assert_eq!(validate(ok).unwrap().services, vec!["web"]);
        for y in [
            "services:\n  web:\n    image: nginx\n    volumes: !override ['/:/host']\n",
            "services:\n  web: !reset {image: nginx, privileged: true}\n",
        ] {
            assert!(matches!(validate(y), Err(PolicyError::Tag(_))), "{y}");
        }
        for y in [
            "services:\n  web:\n    image: nginx\n    volumes: ['${SRC}:/x']\n",
            "services:\n  web:\n    image: nginx\n    volumes: [{type: '${T}', source: data, target: /x}]\n",
        ] {
            assert!(matches!(validate(y), Err(PolicyError::Interpolated { .. })), "{y}");
        }
        // Hills 的寫法：x-storage anchor 內含 build（特權 Git App）
        let hills = "x-s: &s\n  build: {context: .., dockerfile: apps/storage/Dockerfile}\nservices:\n  storage-0:\n    <<: *s\n    ports: ['7850:7850/udp']\n";
        let git = Policy {
            privileged: true,
            repo_depth: Some(1),
            ..Default::default()
        };
        assert!(validate_with(hills, git).is_ok());
        let ctx = "services:\n  a:\n    build: {context: '${CTX}'}\n";
        assert!(matches!(validate_with(ctx, git), Err(PolicyError::Interpolated { .. })));
    }

    // M5-06：平臺 App 政策，以實際專案的 compose 片段為例（docs/07 §3.6）
    #[test]
    fn platform_policy_converts_and_screens() {
        // SinoBee.ai：web 的 ports 移除、env_file 轉為秘密變數盤點；migrate 用 profiles
        let sinobee = "services:\n  web:\n    build: {context: ., dockerfile: Dockerfile}\n    ports: ['3000:3000']\n    env_file: [.env, {path: .env.local, required: false}]\n    depends_on: [db]\n  migrate:\n    build: .\n    command: npx prisma migrate deploy\n    profiles: [tools]\n  db:\n    image: postgres:16\n    volumes: [pg:/var/lib/postgresql/data]\nvolumes:\n  pg: {}\n";
        let p = prepare_platform(sinobee, 0).unwrap();
        assert_eq!(p.compose.services, vec!["web", "migrate", "db"]);
        assert_eq!(p.removed_ports, vec![("web".to_string(), "3000:3000".to_string())]);
        assert_eq!(
            p.env_files,
            vec![
                ("web".to_string(), ".env".to_string()),
                ("web".to_string(), ".env.local".to_string())
            ]
        );
        assert!(!p.yaml.contains("ports") && !p.yaml.contains("env_file"));
        // 轉換後的結果再檢查一次仍通過
        assert!(validate_with(&p.yaml, Policy::platform(0)).is_ok());

        // SinoBee 的 worker 掛 docker.sock：拒絕
        let worker = "services:\n  worker:\n    build: .\n    volumes: ['/var/run/docker.sock:/var/run/docker.sock']\n";
        assert!(matches!(prepare_platform(worker, 0), Err(PolicyError::HostMount { .. })));

        // iSunFA：postgres 對外 port 與 env_file（子目錄專案，深度 1）
        let isunfa = "services:\n  postgres:\n    image: postgres:15\n    ports: [{target: 5432, published: 5432}]\n    env_file: ../.env\n    volumes: [db:/var/lib/postgresql/data]\nvolumes:\n  db: {}\n";
        let p = prepare_platform(isunfa, 1).unwrap();
        assert_eq!(p.removed_ports.len(), 1);
        assert!(p.removed_ports[0].1.contains("5432"));
        assert_eq!(p.env_files, vec![("postgres".to_string(), "../.env".to_string())]);
        assert!(prepare_platform(isunfa, 0).is_err(), "env_file 超出儲存庫");

        // SwarmStorage：ipfs 的數字 port 與具名 volume
        let swarm = "services:\n  ipfs:\n    image: ipfs/kubo:v0.29.0\n    ports: [4001, '5001:5001']\n    volumes: [ipfs:/data/ipfs]\n  app:\n    build: ./app\nvolumes:\n  ipfs: {}\n";
        let p = prepare_platform(swarm, 0).unwrap();
        assert_eq!(
            p.removed_ports.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(),
            vec!["4001", "5001:5001"]
        );

        // 仍然禁止的項目
        for y in [
            "services: {a: {build: ., network_mode: host}}",
            "services: {a: {build: ., pid: host}}",
            "services: {a: {build: ., privileged: true}}",
            "services: {a: {build: ../x}}",
            "services: {a: {build: {context: ., ssh: [default]}}}",
            "services: {a: {image: x, volumes: ['./data:/data']}}",
            "services: {a: {image: x, env_file: '${E}'}}",
        ] {
            assert!(prepare_platform(y, 0).is_err(), "should reject: {y}");
        }
        // GPU（ADR 0022）：平臺 App 只接受 capabilities: [gpu]；一般 App 仍拒絕
        let gpu = "services:\n  a:\n    image: x\n    deploy: {resources: {reservations: {devices: [{driver: nvidia, count: 1, capabilities: [gpu]}]}}}\n  b:\n    image: y\n";
        let c = prepare_platform(gpu, 0).unwrap();
        assert_eq!(c.compose.gpu_services, vec!["a".to_string()]);
        assert!(matches!(validate(gpu), Err(PolicyError::Gpu(_))));
        for bad in [
            "{capabilities: [gpu, utility]}",
            "{capabilities: [tpu]}",
            "{driver: amd, capabilities: [gpu]}",
            "{capabilities: [gpu], foo: 1}",
            "{}",
        ] {
            let y = format!("services:\n  a:\n    image: x\n    deploy: {{resources: {{reservations: {{devices: [{bad}]}}}}}}\n");
            assert!(matches!(prepare_platform(&y, 0), Err(PolicyError::Gpu(_))), "{bad}");
        }
        assert!(prepare_platform("services: {a: {image: x, devices: ['/dev/nvidia0']}}", 0).is_err());
        // 一般 App 與上傳的 compose 仍不能 build
        assert!(validate("services: {a: {build: .}}").is_err());
        // 未轉換就直接以平臺政策檢查：ports 仍被拒
        assert!(validate_with("services: {a: {build: ., ports: ['80:80']}}", Policy::platform(0)).is_err());
    }
}
