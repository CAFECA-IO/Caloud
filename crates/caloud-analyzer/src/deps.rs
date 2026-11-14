//! 附加服務（資料庫、快取）與環境變數（設計稿 §6「附加服務」「環境變數」）。
use crate::{Confidence, Ctx, EnvVar, Evidence, Provider};
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Db {
    Postgres,
    Mysql,
    Redis,
    Mongo,
}

impl Db {
    pub fn default_env(self) -> &'static str {
        match self {
            Db::Postgres | Db::Mysql => "DATABASE_URL",
            Db::Redis => "REDIS_URL",
            Db::Mongo => "MONGODB_URI",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Db::Postgres => "PostgreSQL",
            Db::Mysql => "MySQL",
            Db::Redis => "Redis",
            Db::Mongo => "MongoDB",
        }
    }
    fn image_names(self) -> &'static [&'static str] {
        match self {
            Db::Postgres => &["postgres", "postgis", "timescaledb"],
            Db::Mysql => &["mysql", "mariadb"],
            Db::Redis => &["redis", "valkey", "keydb"],
            Db::Mongo => &["mongo"],
        }
    }
}

pub(crate) struct DbNeed {
    pub kind: Db,
    /// App 讀取連線字串的變數名稱
    pub env: String,
    pub evidence: Evidence,
}

/// 由平臺產生的秘密值
pub const DB_PASSWORD: &str = "CALOUD_DB_PASSWORD";

fn image_base(image: &str) -> &str {
    image
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split([':', '@'])
        .next()
        .unwrap_or_default()
}

fn m(pairs: &[(&str, Value)]) -> Value {
    Value::Mapping(pairs.iter().map(|(k, v)| (Value::from(*k), v.clone())).collect())
}

/// 加入資料庫服務並把連線字串注入 App；已有同類服務時不重複加入。回傳平臺提供的變數名稱
pub(crate) fn add_databases(cx: &mut Ctx, doc: &mut Value, app: &str, needs: &[DbNeed]) -> Vec<String> {
    let mut provided = Vec::new();
    let mut volumes: Vec<String> = Vec::new();
    {
        let Some(services) = doc.get_mut("services").and_then(Value::as_mapping_mut) else {
            return provided;
        };
        for need in needs {
            let existing = services.iter().find_map(|(n, s)| {
                let img = s.get("image").and_then(Value::as_str).map(image_base)?;
                need.kind
                    .image_names()
                    .contains(&img)
                    .then(|| n.as_str().unwrap_or_default().to_string())
            });
            if let Some(name) = existing {
                cx.note(
                    "database",
                    Confidence::Medium,
                    format!(
                        "App 使用 {}，沿用 compose 中的 `{name}`；請把 `{}` 設為指向 `{name}` 的連線字串。",
                        need.kind.label(),
                        need.env
                    ),
                    vec![need.evidence.clone()],
                );
                continue;
            }
            let pw = format!("${{{DB_PASSWORD}}}");
            let (name, svc, url, vol) = match need.kind {
                Db::Postgres => (
                    "db",
                    m(&[
                        ("image", "postgres:16".into()),
                        (
                            "environment",
                            m(&[
                                ("POSTGRES_USER", "app".into()),
                                ("POSTGRES_DB", "app".into()),
                                ("POSTGRES_PASSWORD", pw.as_str().into()),
                            ]),
                        ),
                        ("volumes", Value::Sequence(vec!["db-data:/var/lib/postgresql/data".into()])),
                    ]),
                    format!("postgres://app:{pw}@db:5432/app"),
                    Some("db-data"),
                ),
                Db::Mysql => (
                    "mysql",
                    m(&[
                        ("image", "mysql:8".into()),
                        (
                            "environment",
                            m(&[
                                ("MYSQL_DATABASE", "app".into()),
                                ("MYSQL_USER", "app".into()),
                                ("MYSQL_PASSWORD", pw.as_str().into()),
                                ("MYSQL_RANDOM_ROOT_PASSWORD", "yes".into()),
                            ]),
                        ),
                        ("volumes", Value::Sequence(vec!["mysql-data:/var/lib/mysql".into()])),
                    ]),
                    format!("mysql://app:{pw}@mysql:3306/app"),
                    Some("mysql-data"),
                ),
                Db::Redis => (
                    "redis",
                    m(&[("image", "redis:7-alpine".into())]),
                    "redis://redis:6379".to_string(),
                    None,
                ),
                Db::Mongo => (
                    "mongo",
                    m(&[
                        ("image", "mongo:7".into()),
                        (
                            "environment",
                            m(&[
                                ("MONGO_INITDB_ROOT_USERNAME", "app".into()),
                                ("MONGO_INITDB_ROOT_PASSWORD", pw.as_str().into()),
                            ]),
                        ),
                        ("volumes", Value::Sequence(vec!["mongo-data:/data/db".into()])),
                    ]),
                    format!("mongodb://app:{pw}@mongo:27017/?authSource=admin"),
                    Some("mongo-data"),
                ),
            };
            if services.contains_key(name) {
                cx.note(
                    "database",
                    Confidence::Low,
                    format!("服務名稱 `{name}` 已被使用，未自動加入 {}。", need.kind.label()),
                    vec![need.evidence.clone()],
                );
                continue;
            }
            services.insert(name.into(), svc);
            if let Some(v) = vol {
                volumes.push(v.into());
            }
            if let Some(a) = services.get_mut(app).and_then(Value::as_mapping_mut) {
                let env = a.entry("environment".into()).or_insert_with(|| Value::Mapping(Mapping::new()));
                if let Some(e) = env.as_mapping_mut() {
                    e.insert(need.env.as_str().into(), url.into());
                }
                let dep = a.entry("depends_on".into()).or_insert_with(|| Value::Sequence(vec![]));
                if let Some(d) = dep.as_sequence_mut() {
                    d.push(name.into());
                }
            }
            provided.push(need.env.clone());
            cx.note(
                "database",
                Confidence::High,
                format!(
                    "依賴 {}：加入 `{name}` 服務，並以 `{}` 注入連線字串（密碼由平臺產生）。",
                    need.kind.label(),
                    need.env
                ),
                vec![need.evidence.clone()],
            );
        }
    }
    if !volumes.is_empty() {
        if let Some(top) = doc.as_mapping_mut() {
            let vols = top.entry("volumes".into()).or_insert_with(|| Value::Mapping(Mapping::new()));
            if let Some(vm) = vols.as_mapping_mut() {
                for v in volumes {
                    vm.entry(v.into()).or_insert_with(|| Value::Mapping(Mapping::new()));
                }
            }
        }
    }
    provided
}

/// 一律由平臺提供的變數
const PLATFORM_VARS: &[&str] = &["PORT", "PUBLIC_URL", "NODE_ENV", "HOSTNAME"];

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// 從一行取出 `prefix` 之後的變數名稱（可能被引號包住）
fn names_after<'a>(line: &'a str, prefix: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find(prefix) {
        rest = &rest[i + prefix.len()..];
        let r = rest.trim_start_matches(['"', '\'', '`']);
        let end = r.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(r.len());
        let name = &r[..end];
        if is_name(name) {
            out.push(name);
        }
    }
    out
}

const PATTERNS: &[&str] = &[
    "process.env.",
    "process.env[",
    "import.meta.env.",
    "env::var(",
    "env::var_os(",
    "os.environ[",
    "os.environ.get(",
    "os.getenv(",
    "os.Getenv(",
    "os.LookupEnv(",
];

/// 收集環境變數（.env 範例檔＋程式碼）並分類提供者
pub(crate) fn scan_env(cx: &mut Ctx, platform: &[String]) -> Vec<EnvVar> {
    let mut found: BTreeMap<String, Vec<Evidence>> = BTreeMap::new();
    let mut add = |name: &str, ev: Evidence| {
        let e = found.entry(name.to_string()).or_default();
        if e.len() < 3 {
            e.push(ev);
        }
    };
    for f in [".env.example", ".env.sample", ".env.template", "env.example"] {
        let Some(t) = cx.fs.read(f) else { continue };
        for (i, l) in t.lines().enumerate() {
            let l = l.trim().trim_start_matches("export ").trim();
            if l.starts_with('#') {
                continue;
            }
            if let Some((k, _)) = l.split_once('=') {
                let k = k.trim();
                if is_name(k) {
                    add(k, cx.ev(f, Some(i as u32 + 1)));
                }
            }
        }
    }
    let files = cx
        .fs
        .walk("", 6, &[".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx", ".rs", ".py", ".go"]);
    for f in files.iter().take(2000) {
        let Some(t) = cx.fs.read(f) else { continue };
        for (i, l) in t.lines().enumerate() {
            if !PATTERNS.iter().any(|p| l.contains(p)) {
                continue;
            }
            for p in PATTERNS {
                for n in names_after(l, p) {
                    add(n, cx.ev(f, Some(i as u32 + 1)));
                }
            }
        }
    }
    let mut out: Vec<EnvVar> = found
        .into_iter()
        .map(|(name, evidence)| {
            let provider = if PLATFORM_VARS.contains(&name.as_str()) || platform.contains(&name) {
                Provider::Platform
            } else {
                Provider::User
            };
            EnvVar { name, provider, evidence }
        })
        .collect();
    for p in platform {
        if !out.iter().any(|e| e.name == *p) {
            out.push(EnvVar {
                name: p.clone(),
                provider: Provider::Platform,
                evidence: vec![],
            });
        }
    }
    out
}

/// compose 中使用 `${CALOUD_DB_PASSWORD}` 時加入「平臺產生」的變數
pub(crate) fn generated(compose: &str) -> Option<EnvVar> {
    compose.contains(DB_PASSWORD).then(|| EnvVar {
        name: DB_PASSWORD.into(),
        provider: Provider::Generated,
        evidence: vec![],
    })
}
