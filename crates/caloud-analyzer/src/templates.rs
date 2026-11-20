//! 語言範本（設計稿 §6「語言範本」）：以標記檔判斷框架，產生 `dockerfile_inline`。
use crate::deps::{Db, DbNeed};
use crate::fs::{line_of, RepoFs};
use crate::runtime::{self, apt_run, Lang, Persist};
use crate::{Confidence, Ctx, Evidence, Finding};

pub(crate) struct Detected {
    /// 例：`node/next`、`rust`、`go`、`python/fastapi`、`static`
    pub kind: String,
    pub dockerfile: String,
    /// App 的環境變數（由平臺提供，例：PORT）
    pub env: Vec<(String, String)>,
    pub databases: Vec<DbNeed>,
    pub findings: Vec<Finding>,
    /// 要掛具名 volume 的目錄（FR-AN-07）
    pub persist: Vec<Persist>,
    /// 以 build args 傳入的建置時變數（FR-AN-09）
    pub build_args: Vec<String>,
}

struct Out<'a, 'b> {
    cx: &'a Ctx<'b>,
    findings: Vec<Finding>,
    databases: Vec<DbNeed>,
    /// Prisma SQLite 的連線變數（範本改指向 volume）
    sqlite_env: Option<(String, Evidence)>,
    build_args: Vec<String>,
}

impl Out<'_, '_> {
    fn ev(&self, file: &str, text: &str, needle: &str) -> Evidence {
        self.cx.ev(file, line_of(text, needle))
    }
    fn note(&mut self, key: &str, c: Confidence, msg: impl Into<String>, ev: Vec<Evidence>) {
        self.findings.push(Finding {
            key: key.into(),
            message: msg.into(),
            confidence: c,
            evidence: ev,
        });
    }
    fn db(&mut self, kind: Db, env: &str, ev: Evidence) {
        if !self.databases.iter().any(|d| d.kind == kind) {
            self.databases.push(DbNeed {
                kind,
                env: env.to_string(),
                evidence: ev,
            });
        }
    }
    fn done(self, kind: &str, dockerfile: String, mut env: Vec<(String, String)>) -> Detected {
        let mut findings = self.findings;
        // FR-AN-07：靜態網站不寫入資料
        let mut persist = Vec::new();
        if kind != "static" {
            let (ok, blocked) = runtime::persistent(self.cx);
            persist = ok;
            findings.extend(blocked);
            if let Some((var, ev)) = self.sqlite_env {
                env.push((var.clone(), "file:/app/data/app.db".into()));
                findings.push(Finding {
                    key: "persistent_dir".into(),
                    message: format!("Prisma 使用 SQLite：範本把 `{var}` 設為 `file:/app/data/app.db` 並以具名 volume 保存 `data/`。"),
                    confidence: Confidence::Medium,
                    evidence: vec![ev.clone()],
                });
                match persist.iter_mut().find(|p| p.dir == "data") {
                    Some(p) => p.why.push("Prisma SQLite".into()),
                    None => persist.push(Persist {
                        dir: "data".into(),
                        confidence: Confidence::Medium,
                        evidence: vec![ev],
                        why: vec!["Prisma SQLite".into()],
                        weak: false,
                    }),
                }
            }
        }
        findings.insert(
            0,
            Finding {
                key: "source".into(),
                message: format!("以 {kind} 範本產生 Dockerfile。"),
                confidence: Confidence::High,
                evidence: vec![],
            },
        );
        Detected {
            kind: kind.into(),
            dockerfile,
            env,
            databases: self.databases,
            findings,
            persist,
            build_args: self.build_args,
        }
    }
}

/// Dockerfile 中第一個 EXPOSE 的 port
pub(crate) fn expose_port(dockerfile: &str) -> Option<u16> {
    dockerfile.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("EXPOSE ").or_else(|| l.strip_prefix("expose "))?;
        rest.split_whitespace().next()?.split('/').next()?.parse().ok()
    })
}

/// 依標記檔判斷（順序同設計稿 §6）
pub(crate) fn detect(cx: &Ctx) -> Option<Detected> {
    let mut o = Out {
        cx,
        findings: vec![],
        databases: vec![],
        sqlite_env: None,
        build_args: vec![],
    };
    if let Some(t) = cx.fs.read("package.json") {
        return node(&mut o, &t).map(|(k, d, e)| o.done(&k, d, e));
    }
    if let Some(t) = cx.fs.read("Cargo.toml") {
        return rust(&mut o, &t).map(|(k, d, e)| o.done(&k, d, e));
    }
    if let Some(t) = cx.fs.read("go.mod") {
        return go(&mut o, &t).map(|(k, d, e)| o.done(&k, d, e));
    }
    if cx.fs.is_file("pyproject.toml") || cx.fs.is_file("requirements.txt") {
        return python(&mut o).map(|(k, d, e)| o.done(&k, d, e));
    }
    if cx.fs.is_file("index.html") {
        let ev = cx.ev("index.html", None);
        o.note("port", Confidence::High, "純靜態網站，以 nginx 提供（port 80）。", vec![ev]);
        let df = "FROM nginx:1.27-alpine\nCOPY . /usr/share/nginx/html\nEXPOSE 80\n".to_string();
        return Some(o.done("static", df, vec![]));
    }
    None
}

type Tpl = (String, String, Vec<(String, String)>);

/// `engines.node`、`.nvmrc` 的主版本
fn major(s: &str) -> Option<u32> {
    let digits: String = s
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// `next start -p 10002`、`--port 10002`、`-p=10002`
fn flag_port(script: &str) -> Option<u16> {
    let words: Vec<&str> = script.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        for f in ["-p", "--port"] {
            if *w == f {
                if let Some(p) = words.get(i + 1).and_then(|n| n.parse().ok()) {
                    return Some(p);
                }
            } else if let Some(v) = w.strip_prefix(&format!("{f}=")) {
                if let Ok(p) = v.parse() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn node(o: &mut Out, text: &str) -> Option<Tpl> {
    let fs = o.cx.fs;
    let pkg: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            let ev = o.cx.ev("package.json", None);
            o.note("manifest", Confidence::Low, "package.json 無法解析。", vec![ev]);
            return None;
        }
    };
    let deps: Vec<String> = ["dependencies", "devDependencies"]
        .iter()
        .filter_map(|k| pkg.get(k).and_then(|d| d.as_object()))
        .flat_map(|m| m.keys().cloned())
        .collect();
    let has = |d: &str| deps.iter().any(|x| x == d);
    let script = |k: &str| {
        pkg.get("scripts")
            .and_then(|s| s.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    // 套件管理工具
    let (install, run, setup) = if fs.is_file("pnpm-lock.yaml") {
        ("pnpm install --frozen-lockfile", "pnpm run", "RUN corepack enable\n")
    } else if fs.is_file("yarn.lock") {
        ("yarn install --frozen-lockfile", "yarn run", "RUN corepack enable\n")
    } else if fs.is_file("package-lock.json") {
        ("npm ci", "npm run", "")
    } else {
        let ev = o.cx.ev("package.json", None);
        o.note(
            "lockfile",
            Confidence::Medium,
            "沒有 lockfile，改用 npm install，版本可能與開發時不同。",
            vec![ev],
        );
        ("npm install", "npm run", "")
    };
    let node_v = pkg
        .get("engines")
        .and_then(|e| e.get("node"))
        .and_then(|v| v.as_str())
        .and_then(major)
        .or_else(|| fs.read(".nvmrc").as_deref().and_then(major))
        .filter(|v| (18..=30).contains(v))
        .unwrap_or(22);

    let start = script("start");
    let build = script("build");
    let (framework, mut port, conf) = if has("next") {
        ("next", 3000, Confidence::High)
    } else if has("vite") && start.is_none() {
        ("vite", 4173, Confidence::High)
    } else if has("@nestjs/core") {
        ("nestjs", 3000, Confidence::Medium)
    } else if has("express") {
        ("express", 3000, Confidence::Medium)
    } else {
        ("node", 3000, Confidence::Low)
    };
    let fw_ev = o.ev("package.json", text, &format!("\"{framework}\""));
    if let Some(p) = start.as_deref().and_then(flag_port) {
        port = p;
        let ev = o.ev("package.json", text, "\"start\"");
        o.note("port", Confidence::High, format!("start 指令指定 port {p}。"), vec![ev]);
    } else {
        let msg = match framework {
            "next" => "Next.js 預設 port 3000（以 PORT 設定）。".to_string(),
            "vite" => "Vite preview 使用 port 4173。".to_string(),
            _ => format!("假設程式讀取 PORT（設為 {port}）；若固定其他 port 請修改。"),
        };
        o.note("port", conf, msg, vec![fw_ev.clone()]);
    }
    if let Some(s) = &start {
        if s.contains("build") {
            let ev = o.ev("package.json", text, "\"start\"");
            o.note(
                "start_rebuilds",
                Confidence::Medium,
                "start 指令會重新建置，啟動較慢；範本改為在建置階段執行 build，啟動時直接執行框架指令。",
                vec![ev],
            );
        }
    }

    // 資料庫
    let prisma = has("@prisma/client") || has("prisma");
    if prisma {
        prisma_db(o);
    }
    for (d, kind) in [
        ("pg", Db::Postgres),
        ("postgres", Db::Postgres),
        ("mysql2", Db::Mysql),
        ("mysql", Db::Mysql),
        ("mongoose", Db::Mongo),
        ("mongodb", Db::Mongo),
        ("redis", Db::Redis),
        ("ioredis", Db::Redis),
    ] {
        if has(d) {
            let ev = o.ev("package.json", text, &format!("\"{d}\""));
            o.db(kind, kind.default_env(), ev);
        }
    }
    // FR-AN-08：系統套件與原生模組
    let needs = runtime::system_needs(o.cx, Lang::Node, &runtime::node_deps(o.cx, &pkg, text));
    let inst = runtime::plan_install(&needs, &mut o.findings);
    let mut apt = inst.apt.clone();
    if inst.build_tools {
        for t in ["python3", "make", "g++"] {
            if !apt.iter().any(|a| a == t) {
                apt.push(t.into());
            }
        }
    }

    // Dockerfile
    let mut df = format!(
        "FROM node:{node_v}-bookworm-slim\nWORKDIR /app\n{}{setup}COPY . .\nRUN {install}\n",
        apt_run(&apt)
    );
    if inst.playwright {
        df.push_str("RUN npx playwright install --with-deps chromium\n");
    }
    // FR-AN-09：建置時變數以 build args 傳入
    o.build_args = runtime::build_env_names(o.cx);
    for a in &o.build_args {
        df.push_str(&format!("ARG {a}\n"));
    }
    if prisma && !build.as_deref().unwrap_or_default().contains("prisma generate") {
        df.push_str("RUN npx prisma generate\n");
    }
    if build.is_some() {
        df.push_str(&format!("RUN {run} build\n"));
    } else if framework == "next" || framework == "vite" {
        df.push_str(&format!("RUN npx {framework} build\n"));
    }
    df.push_str(&format!("ENV NODE_ENV=production PORT={port}\nEXPOSE {port}\n"));
    let cmd = match framework {
        "next" => format!("[\"npx\", \"next\", \"start\", \"-p\", \"{port}\"]"),
        "vite" => format!("[\"npx\", \"vite\", \"preview\", \"--host\", \"0.0.0.0\", \"--port\", \"{port}\"]"),
        _ if start.is_some() => {
            let parts: Vec<String> = run.split(' ').chain(["start"]).map(|p| format!("\"{p}\"")).collect();
            format!("[{}]", parts.join(", "))
        }
        _ => {
            let main = pkg.get("main").and_then(|v| v.as_str()).unwrap_or("index.js");
            let ev = o.cx.ev("package.json", None);
            o.note(
                "start",
                Confidence::Low,
                format!("沒有 start 指令，假設以 `node {main}` 啟動。"),
                vec![ev],
            );
            format!("[\"node\", \"{main}\"]")
        }
    };
    df.push_str(&format!("CMD {cmd}\n"));
    Some((format!("node/{framework}"), df, vec![("PORT".into(), port.to_string())]))
}

/// schema.prisma 的 datasource provider 與連線變數名稱
fn prisma_db(o: &mut Out) {
    let fs = o.cx.fs;
    let file = ["prisma/schema.prisma", "schema.prisma"].into_iter().find(|f| fs.is_file(f));
    let Some(file) = file else {
        // prisma 7 以後可分成多檔；找不到時以 package.json 為依據猜 postgres
        let ev = o.cx.ev("package.json", None);
        o.note(
            "database",
            Confidence::Low,
            "使用 Prisma 但找不到 schema.prisma，假設為 PostgreSQL。",
            vec![ev.clone()],
        );
        o.db(Db::Postgres, "DATABASE_URL", ev);
        return;
    };
    let text = fs.read(file).unwrap_or_default();
    let block = text.split("datasource").nth(1).unwrap_or_default();
    let block = block.split('}').next().unwrap_or_default();
    let provider = block
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("provider")
                .map(|r| r.trim_start_matches([' ', '=']).trim().trim_matches('"').to_string())
        })
        .unwrap_or_default();
    let env = block
        .lines()
        .find_map(|l| l.split("env(\"").nth(1).and_then(|r| r.split('"').next()).map(str::to_string))
        .unwrap_or_else(|| "DATABASE_URL".into());
    let ev = o.ev(file, &text, "provider");
    match provider.as_str() {
        "postgresql" | "postgres" | "cockroachdb" => o.db(Db::Postgres, &env, ev),
        "mysql" => o.db(Db::Mysql, &env, ev),
        "mongodb" => o.db(Db::Mongo, &env, ev),
        "sqlite" => o.sqlite_env = Some((env, ev)),
        other => o.note(
            "database",
            Confidence::Low,
            format!("無法判斷 Prisma provider `{other}`，請確認需要的資料庫。"),
            vec![ev],
        ),
    }
}

fn rust(o: &mut Out, text: &str) -> Option<Tpl> {
    let doc: toml::Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            let ev = o.cx.ev("Cargo.toml", None);
            o.note("manifest", Confidence::Low, "Cargo.toml 無法解析。", vec![ev]);
            return None;
        }
    };
    let bin = doc
        .get("bin")
        .and_then(|b| b.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("name"))
        .and_then(|n| n.as_str())
        .map(|n| (n.to_string(), "[[bin]]"))
        .or_else(|| {
            doc.get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .map(|n| (n.to_string(), "name"))
        });
    let bin = match bin {
        Some((b, needle)) => {
            let ev = o.ev("Cargo.toml", text, needle);
            o.note("binary", Confidence::High, format!("執行檔 `{b}`。"), vec![ev]);
            b
        }
        None => {
            let ev = o.cx.ev("Cargo.toml", None);
            o.note(
                "binary",
                Confidence::Low,
                "只有 workspace，無法判斷要執行哪個執行檔，請指定。",
                vec![ev],
            );
            "app".into()
        }
    };
    let deps = doc.get("dependencies").and_then(|d| d.as_table()).cloned().unwrap_or_default();
    let features = |name: &str| -> Vec<String> {
        deps.get(name)
            .and_then(|d| d.get("features"))
            .and_then(|f| f.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    for crate_name in ["sqlx", "diesel", "sea-orm"] {
        for f in features(crate_name) {
            let kind = match f.as_str() {
                x if x.contains("postgres") => Some(Db::Postgres),
                x if x.contains("mysql") => Some(Db::Mysql),
                _ => None,
            };
            if let Some(k) = kind {
                let ev = o.ev("Cargo.toml", text, crate_name);
                o.db(k, "DATABASE_URL", ev);
            }
        }
    }
    for (c, k) in [
        ("tokio-postgres", Db::Postgres),
        ("postgres", Db::Postgres),
        ("deadpool-postgres", Db::Postgres),
        ("redis", Db::Redis),
        ("mongodb", Db::Mongo),
    ] {
        if deps.contains_key(c) {
            let ev = o.ev("Cargo.toml", text, c);
            o.db(k, k.default_env(), ev);
        }
    }
    let ev = o.ev("Cargo.toml", text, "[package]");
    o.note(
        "port",
        Confidence::Medium,
        "假設程式讀取 PORT（設為 8080）；若固定其他 port 請修改。",
        vec![ev],
    );
    // FR-AN-08：rust 建置映像已含 libssl-dev／pkg-config；runtime 補上執行時函式庫
    let needs = runtime::system_needs(o.cx, Lang::Rust, &runtime::rust_deps(o.cx, text));
    let inst = runtime::plan_install(&needs, &mut o.findings);
    let mut apt = vec!["ca-certificates".to_string()];
    apt.extend(inst.apt);
    let df = format!(
        "FROM rust:1-bookworm AS build\nWORKDIR /src\nCOPY . .\nRUN cargo build --release --bin {bin}\n\
         FROM debian:bookworm-slim\n{}WORKDIR /app\n\
         COPY --from=build /src/target/release/{bin} /usr/local/bin/app\nENV PORT=8080\nEXPOSE 8080\nCMD [\"app\"]\n",
        apt_run(&apt)
    );
    Some(("rust".into(), df, vec![("PORT".into(), "8080".into())]))
}

fn go(o: &mut Out, text: &str) -> Option<Tpl> {
    let fs = o.cx.fs;
    let version = text
        .lines()
        .find_map(|l| l.strip_prefix("go "))
        .map(|v| v.trim().split('.').take(2).collect::<Vec<_>>().join("."))
        .filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.') && !v.is_empty())
        .unwrap_or_else(|| "1".into());
    let is_main = |dir: &str| {
        fs.walk(dir, 0, &[".go"])
            .iter()
            .filter(|f| !f.ends_with("_test.go"))
            .any(|f| fs.read(f).is_some_and(|t| t.lines().any(|l| l.trim() == "package main")))
    };
    let pkg = if is_main("") {
        let ev = o.cx.ev("go.mod", None);
        o.note("binary", Confidence::High, "根目錄是 main 套件。", vec![ev]);
        ".".to_string()
    } else {
        let mains: Vec<String> = fs
            .list("cmd")
            .into_iter()
            .filter(|(n, d)| *d && is_main(&format!("cmd/{n}")))
            .map(|(n, _)| format!("./cmd/{n}"))
            .collect();
        match mains.as_slice() {
            [one] => {
                let ev = o.cx.ev(&one[2..], None);
                o.note("binary", Confidence::High, format!("main 套件在 `{one}`。"), vec![ev]);
                one.clone()
            }
            [first, ..] => {
                o.note(
                    "binary",
                    Confidence::Low,
                    format!("有多個 main 套件（{}），先選 `{first}`。", mains.join("、")),
                    vec![],
                );
                first.clone()
            }
            [] => {
                let ev = o.cx.ev("go.mod", None);
                o.note("binary", Confidence::Low, "找不到 main 套件，請指定。", vec![ev]);
                ".".to_string()
            }
        }
    };
    for (m, k) in [
        ("github.com/lib/pq", Db::Postgres),
        ("github.com/jackc/pgx", Db::Postgres),
        ("github.com/go-sql-driver/mysql", Db::Mysql),
        ("github.com/redis/go-redis", Db::Redis),
        ("github.com/go-redis/redis", Db::Redis),
        ("go.mongodb.org/mongo-driver", Db::Mongo),
    ] {
        if text.contains(m) {
            let ev = o.ev("go.mod", text, m);
            o.db(k, k.default_env(), ev);
        }
    }
    let ev = o.cx.ev("go.mod", None);
    o.note(
        "port",
        Confidence::Medium,
        "假設程式讀取 PORT（設為 8080）；若固定其他 port 請修改。",
        vec![ev],
    );
    // FR-AN-08：cgo 依賴或需要 apt 套件時改用 debian runtime
    let needs = runtime::system_needs(o.cx, Lang::Go, &runtime::go_deps(o.cx, text));
    let inst = runtime::plan_install(&needs, &mut o.findings);
    let df = if inst.cgo || !inst.apt.is_empty() {
        let mut apt = vec!["ca-certificates".to_string()];
        apt.extend(inst.apt);
        format!(
            "FROM golang:{version}-bookworm AS build\nWORKDIR /src\nCOPY . .\nRUN CGO_ENABLED={} go build -o /out/app {pkg}\n\
             FROM debian:bookworm-slim\n{}WORKDIR /app\nCOPY --from=build /out/app /usr/local/bin/app\nENV PORT=8080\nEXPOSE 8080\nENTRYPOINT [\"app\"]\n",
            if inst.cgo { 1 } else { 0 },
            apt_run(&apt)
        )
    } else {
        format!(
            "FROM golang:{version} AS build\nWORKDIR /src\nCOPY . .\nRUN CGO_ENABLED=0 go build -o /out/app {pkg}\n\
             FROM gcr.io/distroless/static-debian12\nWORKDIR /app\nCOPY --from=build /out/app /usr/local/bin/app\nENV PORT=8080\nEXPOSE 8080\nENTRYPOINT [\"/usr/local/bin/app\"]\n"
        )
    };
    Some(("go".into(), df, vec![("PORT".into(), "8080".into())]))
}

/// requirements.txt 或 pyproject.toml 的套件名稱（小寫）與所在檔案
pub(crate) fn python_deps(fs: &RepoFs) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(t) = fs.read("requirements.txt") {
        for l in t.lines() {
            let l = l.split('#').next().unwrap_or_default().trim();
            if l.is_empty() || l.starts_with('-') {
                continue;
            }
            let name: String = l
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
                .collect();
            if !name.is_empty() {
                out.push((name.to_ascii_lowercase().replace('_', "-"), "requirements.txt".to_string()));
            }
        }
    }
    if let Some(doc) = fs.read("pyproject.toml").and_then(|t| toml::from_str::<toml::Value>(&t).ok()) {
        let list = doc
            .get("project")
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>())
            .unwrap_or_default();
        let poetry = doc
            .get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_table())
            .map(|t| t.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for d in list.into_iter().chain(poetry) {
            let name: String = d
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
                .collect();
            if !name.is_empty() && name != "python" {
                out.push((name.to_ascii_lowercase().replace('_', "-"), "pyproject.toml".to_string()));
            }
        }
    }
    out
}

/// 找出所有 `<var> = <ctor>(` 的模組與變數名稱，例：`app/main.py` 的 `app = FastAPI()` → `app.main:app`
///
/// 依「檔名為 main／app／server／api」與目錄深度排序。
fn find_apps(o: &Out, ctor: &str) -> Vec<(String, String, Evidence)> {
    let fs = o.cx.fs;
    let mut out = Vec::new();
    for f in fs.walk("", 3, &[".py"]) {
        if f.contains("test") {
            continue;
        }
        let Some(t) = fs.read(&f) else { continue };
        for (i, l) in t.lines().enumerate() {
            let Some((lhs, rhs)) = l.split_once('=') else { continue };
            if rhs.trim_start().starts_with(&format!("{ctor}(")) && !lhs.contains('(') {
                let var = lhs.trim().split(':').next().unwrap_or_default().trim().to_string();
                if var.is_empty() || !var.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let module = f.trim_end_matches(".py").replace('/', ".");
                out.push((module, var, o.cx.ev(&f, Some(i as u32 + 1))));
                break;
            }
        }
    }
    let rank = |m: &str| {
        let file = m.rsplit('.').next().unwrap_or(m);
        let named = ["main", "app", "server", "api", "asgi", "wsgi"].contains(&file);
        (!named, m.matches('.').count(), m.to_string())
    };
    out.sort_by_key(|(m, _, _)| rank(m));
    out
}

/// Procfile 的 `web:`，或 Makefile 中 serve／start／run 目標的第一行指令
fn declared_command(o: &Out) -> Option<(String, Evidence, Confidence)> {
    let fs = o.cx.fs;
    if let Some(t) = fs.read("Procfile") {
        for (i, l) in t.lines().enumerate() {
            if let Some(cmd) = l.strip_prefix("web:") {
                return Some((cmd.trim().to_string(), o.cx.ev("Procfile", Some(i as u32 + 1)), Confidence::High));
            }
        }
    }
    let t = fs.read("Makefile")?;
    let lines: Vec<&str> = t.lines().collect();
    for target in ["serve", "start", "run"] {
        let Some(i) = lines.iter().position(|l| l.starts_with(&format!("{target}:"))) else {
            continue;
        };
        let Some(cmd) = lines.get(i + 1).and_then(|l| l.strip_prefix('\t')) else {
            continue;
        };
        let cmd = cmd.trim();
        if cmd.starts_with("python") || cmd.starts_with("uvicorn") || cmd.starts_with("gunicorn") {
            return Some((cmd.to_string(), o.cx.ev("Makefile", Some(i as u32 + 2)), Confidence::Medium));
        }
    }
    None
}

fn python(o: &mut Out) -> Option<Tpl> {
    let fs = o.cx.fs;
    let deps = python_deps(fs);
    let has = |d: &str| deps.iter().find(|(n, _)| n == d).map(|(_, f)| f.clone());
    let install = if fs.is_file("requirements.txt") {
        "pip install --no-cache-dir -r requirements.txt"
    } else {
        "pip install --no-cache-dir ."
    };
    let mut extra = Vec::new();
    let declared = declared_command(o);
    let (framework, cmd) = if let Some((cmd, ev, conf)) = declared {
        o.note("entrypoint", conf, format!("依專案宣告的啟動指令 `{cmd}`。"), vec![ev]);
        o.note(
            "port",
            Confidence::Low,
            "無法從啟動指令判斷 port，暫設 8000（以 PORT 傳入），請確認。",
            vec![],
        );
        (
            "declared",
            format!("[\"sh\", \"-c\", {}]", serde_json::to_string(&cmd).unwrap_or_default()),
        )
    } else if has("fastapi").is_some() {
        if has("uvicorn").is_none() {
            extra.push("uvicorn");
        }
        let apps = find_apps(o, "FastAPI");
        match apps.as_slice() {
            [] => {
                o.note(
                    "entrypoint",
                    Confidence::Low,
                    "找不到 `FastAPI(` 的位置，假設為 `main:app`。",
                    vec![],
                );
                (
                    "fastapi",
                    "[\"uvicorn\", \"main:app\", \"--host\", \"0.0.0.0\", \"--port\", \"8000\"]".to_string(),
                )
            }
            [(m, v, ev), rest @ ..] => {
                if rest.is_empty() {
                    o.note(
                        "entrypoint",
                        Confidence::High,
                        format!("FastAPI 應用 `{m}:{v}`。"),
                        vec![ev.clone()],
                    );
                } else {
                    let all: Vec<String> = apps.iter().map(|(m, v, _)| format!("`{m}:{v}`")).collect();
                    o.note(
                        "entrypoint",
                        Confidence::Low,
                        format!(
                            "找到 {} 個 FastAPI 應用（{}），先選 `{m}:{v}`，請確認。",
                            apps.len(),
                            all.join("、")
                        ),
                        apps.iter().map(|(_, _, e)| e.clone()).collect(),
                    );
                }
                (
                    "fastapi",
                    format!("[\"uvicorn\", \"{m}:{v}\", \"--host\", \"0.0.0.0\", \"--port\", \"8000\"]"),
                )
            }
        }
    } else if has("django").is_some() {
        if has("gunicorn").is_none() {
            extra.push("gunicorn");
        }
        let wsgi = fs.walk("", 2, &["wsgi.py"]).into_iter().next();
        match wsgi {
            Some(w) => {
                let m = w.trim_end_matches(".py").replace('/', ".");
                let ev = o.cx.ev(&w, None);
                o.note("entrypoint", Confidence::High, format!("Django WSGI `{m}`。"), vec![ev]);
                (
                    "django",
                    format!("[\"gunicorn\", \"{m}:application\", \"--bind\", \"0.0.0.0:8000\"]"),
                )
            }
            None => {
                o.note("entrypoint", Confidence::Low, "找不到 wsgi.py，請指定 Django 專案模組。", vec![]);
                (
                    "django",
                    "[\"gunicorn\", \"wsgi:application\", \"--bind\", \"0.0.0.0:8000\"]".to_string(),
                )
            }
        }
    } else if has("flask").is_some() {
        if has("gunicorn").is_none() {
            extra.push("gunicorn");
        }
        match find_apps(o, "Flask").into_iter().next() {
            Some((m, v, ev)) => {
                o.note("entrypoint", Confidence::High, format!("Flask 應用 `{m}:{v}`。"), vec![ev]);
                ("flask", format!("[\"gunicorn\", \"{m}:{v}\", \"--bind\", \"0.0.0.0:8000\"]"))
            }
            None => {
                o.note("entrypoint", Confidence::Low, "找不到 `Flask(` 的位置，假設為 `app:app`。", vec![]);
                ("flask", "[\"gunicorn\", \"app:app\", \"--bind\", \"0.0.0.0:8000\"]".to_string())
            }
        }
    } else {
        let main = ["main.py", "app.py", "server.py"]
            .into_iter()
            .find(|f| fs.is_file(f))
            .unwrap_or("main.py");
        o.note(
            "entrypoint",
            Confidence::Low,
            format!("無法判斷框架，假設以 `python {main}` 啟動並讀取 PORT。"),
            vec![],
        );
        ("python", format!("[\"python\", \"{main}\"]"))
    };
    if !extra.is_empty() {
        o.note(
            "install_extra",
            Confidence::Medium,
            format!("依賴中沒有 {}，範本會另外安裝。", extra.join("、")),
            vec![],
        );
    }
    for (d, k) in [
        ("psycopg", Db::Postgres),
        ("psycopg2", Db::Postgres),
        ("psycopg2-binary", Db::Postgres),
        ("asyncpg", Db::Postgres),
        ("mysqlclient", Db::Mysql),
        ("pymysql", Db::Mysql),
        ("redis", Db::Redis),
        ("pymongo", Db::Mongo),
        ("motor", Db::Mongo),
    ] {
        if let Some(f) = has(d) {
            let t = fs.read(&f).unwrap_or_default();
            let ev = o.ev(&f, &t, d);
            o.db(k, k.default_env(), ev);
        }
    }
    // FR-AN-08
    let needs = runtime::system_needs(o.cx, Lang::Python, &runtime::python_dep_evidence(o.cx));
    let inst = runtime::plan_install(&needs, &mut o.findings);
    let mut df = format!(
        "FROM python:3.12-slim\nWORKDIR /app\n{}COPY . .\nRUN {install}\n",
        apt_run(&inst.apt)
    );
    if !extra.is_empty() {
        df.push_str(&format!("RUN pip install --no-cache-dir {}\n", extra.join(" ")));
    }
    if inst.playwright {
        df.push_str("RUN playwright install --with-deps chromium\n");
    }
    df.push_str(&format!("ENV PORT=8000\nEXPOSE 8000\nCMD {cmd}\n"));
    if framework != "declared" {
        o.note("port", Confidence::Medium, "Python 範本使用 port 8000。", vec![]);
    }
    Some((format!("python/{framework}"), df, vec![("PORT".into(), "8000".into())]))
}
