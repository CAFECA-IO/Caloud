use super::*;
use caloud_orchestrator::{validate_with, Policy};
use std::path::PathBuf;

/// 在暫存目錄建出迷你儲存庫
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("caloud-an-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (f, body) in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        Repo(dir)
    }
    fn analyze(&self, root: &str) -> Analysis {
        let a = analyze(&self.0, &Options { root: root.into() });
        // 產出的 compose 一定通過平臺政策（FR-AN-02）
        if let Some(c) = &a.compose {
            let depth = a.compose_dir.split('/').filter(|s| !s.is_empty()).count();
            validate_with(c, Policy::platform(depth)).unwrap_or_else(|e| panic!("{e}\n{c}"));
        }
        a
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn has(a: &Analysis, key: &str) -> bool {
    a.findings.iter().any(|f| f.key == key)
}

fn public(a: &Analysis) -> &ServiceInfo {
    a.services.iter().find(|s| s.public).expect("public service")
}

fn env(a: &Analysis, name: &str) -> Provider {
    a.env
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("env {name}"))
        .provider
}

/// 實測：`CALOUD_ANALYZER_REPOS=<dir>` 指向含多個儲存庫的目錄，印出解析結果（不進 CI）
#[test]
#[ignore]
fn print_real_repos() {
    let Ok(dir) = std::env::var("CALOUD_ANALYZER_REPOS") else { return };
    let roots: Vec<(String, String)> = std::env::var("CALOUD_ANALYZER_ROOTS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|p| p.split_once('=').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect();
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();
    for n in names {
        let mut runs = vec![String::new()];
        runs.extend(roots.iter().filter(|(r, _)| *r == n).map(|(_, root)| root.clone()));
        for root in runs {
            let a = analyze(&PathBuf::from(&dir).join(&n), &Options { root: root.clone() });
            if let Ok(out) = std::env::var("CALOUD_ANALYZER_JSON") {
                let f = PathBuf::from(out).join(format!(
                    "{n}{}.json",
                    if root.is_empty() { String::new() } else { format!("-{root}") }
                ));
                std::fs::write(f, serde_json::to_string_pretty(&a).unwrap()).unwrap();
            }
            println!("===== {n} (root={root:?})");
            println!(
                "source: {:?}  compose_dir: {:?}  confirm: {}",
                a.source, a.compose_dir, a.needs_confirmation
            );
            for s in &a.services {
                println!("  svc {:<14} {:?} port={:?} public={}", s.name, s.role, s.port, s.public);
            }
            for f in &a.findings {
                let ev: Vec<String> = f.evidence.iter().map(|e| format!("{}:{}", e.file, e.line.unwrap_or(0))).collect();
                println!("  [{:?}] {} — {} {:?}", f.confidence, f.key, f.message, ev);
            }
            for e in &a.errors {
                println!("  ERROR {e}");
            }
            let user: Vec<&str> = a
                .env
                .iter()
                .filter(|e| e.provider == Provider::User)
                .map(|e| e.name.as_str())
                .collect();
            let plat: Vec<&str> = a
                .env
                .iter()
                .filter(|e| e.provider != Provider::User)
                .map(|e| e.name.as_str())
                .collect();
            println!("  env platform/generated: {plat:?}");
            println!("  env user ({}): {:?}", user.len(), &user[..user.len().min(15)]);
            if let Some(c) = &a.compose {
                println!("--- compose\n{c}");
            }
        }
    }
}

// FR-AN-03：`.caloud/compose.yml` 優先，原樣使用（仍須通過平臺政策）
#[test]
fn override_compose_wins() {
    let r = Repo::new(
        "override",
        &[
            (".caloud/compose.yml", "services:\n  api:\n    build: .\n    expose: ['9000']\n"),
            ("docker-compose.yml", "services:\n  other:\n    image: nginx\n"),
            ("package.json", "{}"),
        ],
    );
    let a = r.analyze("");
    assert_eq!(
        a.source,
        Source::Override {
            file: ".caloud/compose.yml".into()
        }
    );
    assert!(a.errors.is_empty(), "{:?}", a.errors);
    let p = public(&a);
    assert_eq!((p.name.as_str(), p.port), ("api", Some(9000)));
    assert!(!a.compose.unwrap().contains("other"));

    // 覆寫檔也不能繞過政策
    let bad = Repo::new(
        "override-bad",
        &[(".caloud/compose.yml", "services:\n  a:\n    build: .\n    network_mode: host\n")],
    );
    let a = bad.analyze("");
    assert!(a.compose.is_none());
    assert!(!a.errors.is_empty());
    assert!(a.needs_confirmation);
}

// FR-AN-01、FR-AN-02：沿用 compose；ports、container_name 移除，相對路徑掛載改具名 volume
#[test]
fn compose_is_normalized_for_platform() {
    let compose = "version: '3.8'\nservices:\n  web:\n    build: .\n    container_name: my_web\n    ports: ['8080:3000']\n    env_file: .env\n    volumes: ['./uploads:/app/uploads']\n  cache:\n    image: redis:7\n";
    let r = Repo::new(
        "compose",
        &[("docker-compose.yml", compose), ("Dockerfile", "FROM node\nEXPOSE 3000\n")],
    );
    let a = r.analyze("");
    assert_eq!(
        a.source,
        Source::Compose {
            file: "docker-compose.yml".into()
        }
    );
    let c = a.compose.clone().unwrap();
    for gone in ["container_name", "ports", "env_file", "version", "./uploads"] {
        assert!(!c.contains(gone), "{gone} still in\n{c}");
    }
    assert!(c.contains("web-uploads:/app/uploads"));
    let web = a.services.iter().find(|s| s.name == "web").unwrap();
    assert_eq!((web.role, web.port, web.public), (Role::App, Some(3000), true));
    assert_eq!(a.services.iter().find(|s| s.name == "cache").unwrap().role, Role::Cache);
    for k in ["ports_removed", "container_name_removed", "bind_to_volume", "env_file"] {
        assert!(has(&a, k), "missing {k}");
    }
    // 依據含行號（FR-AN-04）
    let f = a.findings.iter().find(|f| f.key == "container_name_removed").unwrap();
    assert_eq!(
        f.evidence[0],
        Evidence {
            file: "docker-compose.yml".into(),
            line: Some(5)
        }
    );
}

// FR-AN-05：docker.sock 等違反政策者列為阻擋項，不產出 compose
#[test]
fn policy_violations_become_errors() {
    let compose = "services:\n  web:\n    build: .\n  worker:\n    build: .\n    volumes: ['/var/run/docker.sock:/var/run/docker.sock']\n";
    let r = Repo::new("dockersock", &[("docker-compose.yml", compose)]);
    let a = r.analyze("");
    assert!(a.compose.is_none());
    assert!(a.errors[0].contains("worker"), "{:?}", a.errors);
    assert!(a.needs_confirmation);
    assert_eq!(a.services.len(), 2, "仍列出服務供確認頁顯示");
}

// ADR 0014：compose 只有附加服務時與範本合併
#[test]
fn auxiliary_compose_merges_with_template() {
    let compose = "services:\n  postgres:\n    image: postgres:16\n    ports: ['5435:5432']\n";
    let pkg =
        r#"{"scripts":{"build":"prisma generate && next build","start":"next start"},"dependencies":{"next":"15","@prisma/client":"6"}}"#;
    let schema = "datasource db {\n  provider = \"postgresql\"\n  url      = env(\"DATABASE_URL\")\n}\n";
    let r = Repo::new(
        "merge",
        &[
            ("docker-compose.yml", compose),
            ("package.json", pkg),
            ("package-lock.json", "{}"),
            ("prisma/schema.prisma", schema),
        ],
    );
    let a = r.analyze("");
    assert!(has(&a, "merge"));
    let c = a.compose.clone().unwrap();
    assert!(c.contains("dockerfile_inline"));
    assert!(!c.contains("db-data"), "已有 postgres，不重複加入");
    assert!(!c.contains("prisma generate\n"), "build 已含 prisma generate");
    assert_eq!(public(&a).name, "web");
    assert_eq!(a.services.iter().find(|s| s.name == "postgres").unwrap().role, Role::Database);
    let db = a.findings.iter().find(|f| f.key == "database").unwrap();
    assert_eq!(db.confidence, Confidence::Medium);
    assert_eq!(
        db.evidence[0],
        Evidence {
            file: "prisma/schema.prisma".into(),
            line: Some(2)
        }
    );
}

// FR-AN-01：Node 範本、Next 的 -p、Prisma 判斷 PostgreSQL 並注入連線字串
#[test]
fn node_template_with_database() {
    let pkg = "{\n  \"scripts\": {\n    \"build\": \"next build\",\n    \"start\": \"next start -p 10002\"\n  },\n  \"engines\": {\"node\": \">=20\"},\n  \"dependencies\": {\"next\": \"15\", \"@prisma/client\": \"6\", \"ioredis\": \"5\"}\n}\n";
    let schema = "datasource db {\n  provider = \"postgresql\"\n  url = env(\"PG_URL\")\n}\n";
    let src = "const k = process.env.STRIPE_KEY;\nconst u = process.env['PG_URL'];\nconst p = process.env.PORT;\n";
    let r = Repo::new(
        "node",
        &[
            ("package.json", pkg),
            ("pnpm-lock.yaml", ""),
            ("prisma/schema.prisma", schema),
            ("src/a.ts", src),
            (".env.example", "# x\nSTRIPE_KEY=\nexport OTHER=1\n"),
        ],
    );
    let a = r.analyze("");
    assert_eq!(
        a.source,
        Source::Template {
            template: "node/next".into()
        }
    );
    let c = a.compose.clone().unwrap();
    assert!(c.contains("FROM node:20-bookworm-slim"));
    assert!(c.contains("pnpm install --frozen-lockfile"));
    assert!(c.contains("RUN npx prisma generate"));
    assert!(c.contains("PG_URL: postgres://app:${CALOUD_DB_PASSWORD}@db:5432/app"), "{c}");
    assert!(c.contains("REDIS_URL: redis://redis:6379"));
    assert!(c.contains("db-data"));
    assert_eq!(public(&a).port, Some(10002));
    let port = a.findings.iter().find(|f| f.key == "port").unwrap();
    assert_eq!(port.evidence[0].line, Some(4));
    assert_eq!(env(&a, "PG_URL"), Provider::Platform);
    assert_eq!(env(&a, "REDIS_URL"), Provider::Platform);
    assert_eq!(env(&a, "PORT"), Provider::Platform);
    assert_eq!(env(&a, "STRIPE_KEY"), Provider::User);
    assert_eq!(env(&a, "OTHER"), Provider::User);
    assert!(!a.env.iter().any(|e| e.name.ends_with("_ONLY")), "測試檔的變數不列入");
    assert_eq!(env(&a, "CALOUD_DB_PASSWORD"), Provider::Generated);
    let stripe = a.env.iter().find(|e| e.name == "STRIPE_KEY").unwrap();
    assert!(stripe.evidence.contains(&Evidence {
        file: "src/a.ts".into(),
        line: Some(1)
    }));
    assert!(!a.needs_confirmation);
}

// FR-AN-01：Rust、Go、Python、靜態網站範本
#[test]
fn other_language_templates() {
    let rust = Repo::new(
        "rust",
        &[(
            "Cargo.toml",
            "[package]\nname = \"svc\"\n\n[dependencies]\nsqlx = { version = \"0.8\", features = [\"postgres\"] }\n",
        )],
    );
    let a = rust.analyze("");
    assert_eq!(a.source, Source::Template { template: "rust".into() });
    let c = a.compose.clone().unwrap();
    assert!(c.contains("--bin svc") && c.contains("DATABASE_URL"));
    assert_eq!(public(&a).port, Some(8080));

    let go = Repo::new(
        "go",
        &[
            ("go.mod", "module x\n\ngo 1.22.3\n\nrequire github.com/redis/go-redis/v9 v9.0.0\n"),
            ("cmd/server/main.go", "package main\nfunc main(){}\n"),
            ("lib/a.go", "package lib\nimport \"os\"\nvar _ = os.Getenv(\"API_TOKEN\")\n"),
        ],
    );
    let a = go.analyze("");
    let c = a.compose.clone().unwrap();
    assert!(c.contains("golang:1.22") && c.contains("./cmd/server") && c.contains("REDIS_URL"));
    assert_eq!(env(&a, "API_TOKEN"), Provider::User);

    let py = Repo::new(
        "py",
        &[
            ("requirements.txt", "fastapi>=0.110\npsycopg[binary]\n"),
            ("app/main.py", "from fastapi import FastAPI\napi = FastAPI()\n"),
        ],
    );
    let a = py.analyze("");
    assert_eq!(
        a.source,
        Source::Template {
            template: "python/fastapi".into()
        }
    );
    let c = a.compose.clone().unwrap();
    assert!(c.contains("uvicorn\", \"app.main:api\""), "{c}");
    assert!(c.contains("pip install --no-cache-dir uvicorn"), "依賴沒有 uvicorn 時另外安裝");
    assert!(c.contains("DATABASE_URL"));

    let site = Repo::new("static", &[("index.html", "<h1>hi</h1>")]);
    let a = site.analyze("");
    assert_eq!(a.source, Source::Static);
    assert_eq!(public(&a).port, Some(80));
}

// FR-AN-04：多個可能的進入點時為低信心，需要確認
#[test]
fn ambiguous_entrypoints_need_confirmation() {
    let py = Repo::new(
        "py-multi",
        &[
            ("requirements.txt", "fastapi\nuvicorn\n"),
            ("svc/a.py", "app = FastAPI()\n"),
            ("svc/main.py", "app = FastAPI()\n"),
        ],
    );
    let a = py.analyze("");
    assert!(a.needs_confirmation);
    assert!(a.compose.unwrap().contains("svc.main:app"), "main.py 優先");

    let df = Repo::new(
        "dockerfile-noexpose",
        &[("Dockerfile", "FROM alpine\n"), ("Dockerfile.worker", "FROM alpine\nEXPOSE 9000\n")],
    );
    let a = df.analyze("");
    assert_eq!(
        a.source,
        Source::Dockerfile {
            files: vec!["Dockerfile".into(), "Dockerfile.worker".into()]
        }
    );
    assert!(a.needs_confirmation, "web 沒有 EXPOSE");
    assert_eq!(
        a.services.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["web", "worker"]
    );
}

// FR-AN-06：子目錄根目錄；根目錄沒有可解析檔案時建議子目錄
#[test]
fn subdirectory_root_and_suggestion() {
    let pkg = r#"{"scripts":{"start":"node server.js"},"dependencies":{"express":"4"}}"#;
    let r = Repo::new(
        "mono",
        &[("app/package.json", pkg), ("app/package-lock.json", "{}"), ("README.md", "x")],
    );
    let a = r.analyze("");
    assert!(a.compose.is_none());
    assert!(has(&a, "suggest_root"));
    assert_eq!(a.findings[0].evidence[0].file, "app/package.json");

    let a = r.analyze("app");
    assert_eq!(a.root, "app");
    assert_eq!(a.compose_dir, "app");
    assert_eq!(
        a.source,
        Source::Template {
            template: "node/express".into()
        }
    );
    assert!(a.compose.unwrap().contains("CMD [\"npm\", \"run\", \"start\"]"));

    let a = r.analyze("../etc");
    assert!(!a.errors.is_empty());
    let a = r.analyze("nope");
    assert!(!a.errors.is_empty());
}

// ADR 0014：compose 在子目錄時，範本的 build context 指回根目錄
#[test]
fn compose_in_subdir_uses_relative_context() {
    let r = Repo::new(
        "subcompose",
        &[
            ("docker/docker-compose.yml", "services:\n  redis:\n    image: redis:7\n"),
            ("go.mod", "module x\ngo 1.23\n"),
            ("main.go", "package main\n"),
        ],
    );
    let a = r.analyze("");
    assert_eq!(a.compose_dir, "docker");
    let c = a.compose.clone().unwrap();
    assert!(c.contains("context: .."), "{c}");
    assert_eq!(
        a.findings.iter().find(|f| f.key == "source").unwrap().confidence,
        Confidence::Medium
    );
}

// docs/07：解析不跟隨 symlink
#[cfg(unix)]
#[test]
fn symlinked_manifests_are_ignored() {
    let outside = Repo::new("outside", &[("package.json", r#"{"dependencies":{"next":"15"}}"#)]);
    let r = Repo::new("symlink", &[("README.md", "x")]);
    std::os::unix::fs::symlink(outside.0.join("package.json"), r.0.join("package.json")).unwrap();
    let a = r.analyze("");
    assert_eq!(a.source, Source::None);
    assert!(a.compose.is_none());
}

// Analysis 可序列化（之後存入 analyses 表）
#[test]
fn analysis_round_trips_as_json() {
    let r = Repo::new("json", &[("index.html", "x")]);
    let a = r.analyze("");
    let j = serde_json::to_value(&a).unwrap();
    assert_eq!(j["source"]["kind"], "static");
    assert_eq!(j["findings"][0]["confidence"], "high");
    let back: Analysis = serde_json::from_value(j).unwrap();
    assert_eq!(back.source, Source::Static);
}

fn finding<'a>(a: &'a Analysis, key: &str, contains: &str) -> &'a Finding {
    a.findings
        .iter()
        .find(|f| f.key == key && f.message.contains(contains))
        .unwrap_or_else(|| panic!("finding {key} ~ {contains}: {:#?}", a.findings))
}

// FR-AN-08：Node 原生模組與系統套件由範本加裝；預編譯者不加裝
#[test]
fn node_system_packages_are_installed() {
    let pkg = r#"{"scripts":{"start":"node server.js"},"dependencies":{"express":"4","canvas":"2","bcrypt":"5","playwright":"1","sharp":"0.33","fluent-ffmpeg":"2"}}"#;
    let r = Repo::new("node-sys", &[("package.json", pkg), ("package-lock.json", "{}")]);
    let a = r.analyze("");
    let c = a.compose.clone().unwrap();
    assert!(c.contains("apt-get install -y --no-install-recommends"), "{c}");
    for p in ["libcairo2-dev", "build-essential", "ffmpeg", "python3", "make", "g++"] {
        assert!(c.contains(p), "{p}\n{c}");
    }
    // apt 在安裝依賴之前；Playwright 在安裝依賴之後
    let apt = c.find("apt-get install").unwrap();
    let npm = c.find("RUN npm ci").unwrap();
    let pw = c.find("npx playwright install --with-deps chromium").unwrap();
    assert!(apt < npm && npm < pw, "{c}");
    let f = finding(&a, "system_package", "`canvas`");
    assert_eq!(f.confidence, Confidence::High);
    assert_eq!(f.evidence[0].file, "package.json");
    assert_eq!(finding(&a, "native_module", "`bcrypt`").confidence, Confidence::High);
    assert_eq!(finding(&a, "native_module", "`sharp`").confidence, Confidence::High);
    assert!(!c.contains("libvips"));
}

// FR-AN-08：Python、Go、Rust 與 Aptfile
#[test]
fn other_language_system_packages() {
    let py = Repo::new(
        "py-sys",
        &[
            ("requirements.txt", "fastapi\nopencv-python==4.9\npsycopg2\nplaywright\n"),
            ("main.py", "from fastapi import FastAPI\napp = FastAPI()\n"),
            ("Aptfile", "# extra\nimagemagick\nhttps://example.com/x.deb\nfoo; rm -rf /\n"),
        ],
    );
    let a = py.analyze("");
    let c = a.compose.clone().unwrap();
    for p in ["libgl1", "libglib2.0-0", "libpq-dev", "gcc", "imagemagick"] {
        assert!(c.contains(p), "{p}\n{c}");
    }
    assert!(!c.contains("example.com") && !c.contains("foo;"), "Aptfile 只接受套件名稱\n{c}");
    assert!(c.contains("RUN playwright install --with-deps chromium"));
    assert_eq!(finding(&a, "system_package", "Aptfile").confidence, Confidence::Medium);
    assert_eq!(finding(&a, "system_package", "不是套件名稱").confidence, Confidence::Medium);

    let go = Repo::new(
        "go-sys",
        &[
            ("go.mod", "module x\n\ngo 1.23\n\nrequire github.com/mattn/go-sqlite3 v1.14.0\n"),
            ("main.go", "package main\nfunc main() {}\n"),
        ],
    );
    let a = go.analyze("");
    let c = a.compose.clone().unwrap();
    assert!(c.contains("CGO_ENABLED=1") && c.contains("FROM debian:bookworm-slim"), "{c}");
    assert!(!c.contains("distroless"));
    finding(&a, "native_module", "cgo");

    let rust = Repo::new(
        "rust-sys",
        &[
            ("Cargo.toml", "[package]\nname = \"svc\"\n"),
            ("Cargo.lock", "[[package]]\nname = \"openssl-sys\"\nversion = \"0.9\"\n"),
        ],
    );
    let a = rust.analyze("");
    let c = a.compose.clone().unwrap();
    assert!(c.contains("ca-certificates libssl3"), "{c}");
    assert_eq!(finding(&a, "system_package", "openssl-sys").evidence[0].file, "Cargo.lock");
}

// FR-AN-07：寫入的目錄掛具名 volume；暫存、建置產物與含原始碼的目錄不掛
#[test]
fn persistent_dirs_get_named_volumes() {
    let pkg = r#"{"scripts":{"start":"node server.js"},"dependencies":{"express":"4","multer":"1"}}"#;
    let server = "const upload = multer({ dest: 'uploads/' });\n\
                  fs.writeFileSync('/tmp/x.txt', 'a');\n\
                  fs.mkdirSync('./dist/cache', { recursive: true });\n\
                  const dir = process.env.DATA_DIR || './data';\n\
                  fs.writeFileSync(`./reports/${id}.json`, 'a');\n\
                  fs.writeFileSync('src/gen.js', 'x');\n\
                  fs.writeFileSync('state.json', '{}');\n";
    let r = Repo::new(
        "persist",
        &[
            ("package.json", pkg),
            ("package-lock.json", "{}"),
            ("server.js", server),
            ("src/index.js", "module.exports = 1;\n"),
            ("media/.gitkeep", ""),
        ],
    );
    let a = r.analyze("");
    let c = a.compose.clone().unwrap();
    assert!(c.contains("web-uploads:/app/uploads"), "{c}");
    assert!(c.contains("web-data:/app/data"), "{c}");
    assert!(c.contains("web-media:/app/media"), "{c}");
    for no in ["/app/tmp", "/app/dist", "/app/src", "reports"] {
        assert!(!c.contains(no), "{no}\n{c}");
    }
    // 頂層宣告具名 volume
    let doc: serde_yaml::Value = serde_yaml::from_str(&c).unwrap();
    assert!(doc["volumes"].get("web-uploads").is_some());
    let up = finding(&a, "persistent_dir", "`uploads/`");
    assert_eq!(up.confidence, Confidence::High);
    assert_eq!(
        up.evidence[0],
        Evidence {
            file: "server.js".into(),
            line: Some(1)
        }
    );
    assert_eq!(finding(&a, "persistent_dir", "`data/`").confidence, Confidence::Medium);
    assert_eq!(finding(&a, "persistent_dir", "`src/`").confidence, Confidence::Medium);
    assert_eq!(finding(&a, "persistent_dir", "`state.json`").confidence, Confidence::Medium);
}

// FR-AN-07：Prisma SQLite 改指向 volume
#[test]
fn prisma_sqlite_moves_to_volume() {
    let pkg = r#"{"scripts":{"build":"next build"},"dependencies":{"next":"15","@prisma/client":"6"}}"#;
    let schema = "datasource db {\n  provider = \"sqlite\"\n  url = env(\"DATABASE_URL\")\n}\n";
    let r = Repo::new(
        "sqlite",
        &[("package.json", pkg), ("package-lock.json", "{}"), ("prisma/schema.prisma", schema)],
    );
    let a = r.analyze("");
    let c = a.compose.clone().unwrap();
    assert!(c.contains("DATABASE_URL: file:/app/data/app.db"), "{c}");
    assert!(c.contains("web-data:/app/data"), "{c}");
    assert_eq!(env(&a, "DATABASE_URL"), Provider::Platform);
    finding(&a, "persistent_dir", "Prisma 使用 SQLite");
}

// FR-AN-09：建置時變數以 build args 傳入，並標示隨環境不同者
#[test]
fn build_time_variables() {
    let pkg = r#"{"scripts":{"build":"next build"},"dependencies":{"next":"15"}}"#;
    let src =
        "const api = process.env.NEXT_PUBLIC_API_URL;\nconst ga = process.env.NEXT_PUBLIC_GA_ID;\nconst s = process.env.SECRET_KEY;\n";
    let r = Repo::new(
        "buildenv",
        &[("package.json", pkg), ("package-lock.json", "{}"), ("app/page.tsx", src)],
    );
    let a = r.analyze("");
    let c = a.compose.clone().unwrap();
    let arg = c.find("ARG NEXT_PUBLIC_API_URL").expect(&c);
    assert!(arg < c.find("RUN npm run build").unwrap(), "{c}");
    let doc: serde_yaml::Value = serde_yaml::from_str(&c).unwrap();
    assert_eq!(doc["services"]["web"]["build"]["args"]["NEXT_PUBLIC_GA_ID"], "${NEXT_PUBLIC_GA_ID}");
    assert!(!c.contains("ARG SECRET_KEY"));
    let get = |n: &str| a.env.iter().find(|e| e.name == n).unwrap();
    assert!(get("NEXT_PUBLIC_API_URL").build_time && get("NEXT_PUBLIC_API_URL").varies_by_env);
    assert!(get("NEXT_PUBLIC_GA_ID").build_time && !get("NEXT_PUBLIC_GA_ID").varies_by_env);
    assert!(!get("SECRET_KEY").build_time);
    assert_eq!(finding(&a, "build_env", "2 個建置時變數").confidence, Confidence::High);
    let v = finding(&a, "build_env", "staging 與 production");
    assert_eq!(v.confidence, Confidence::Medium);
    assert!(v.message.contains("NEXT_PUBLIC_API_URL") && !v.message.contains("GA_ID"));
    // 舊紀錄沒有新欄位仍可讀取
    let old: EnvVar = serde_json::from_str(r#"{"name":"X","provider":"user","evidence":[]}"#).unwrap();
    assert!(!old.build_time && !old.varies_by_env);
}

// FR-AN-07、FR-AN-08：用戶自己的 Dockerfile／compose 只檢查不修改
#[test]
fn existing_dockerfile_is_checked_not_modified() {
    let pkg = r#"{"dependencies":{"express":"4","fluent-ffmpeg":"2","bcrypt":"5"}}"#;
    let df = "FROM node:22-slim\nWORKDIR /srv\nRUN apt-get update && apt-get install -y build-essential\nCOPY . .\nEXPOSE 3000\n";
    let server = "fs.mkdirSync('uploads', { recursive: true });\nfs.writeFileSync('cache/x', 'a');\n";
    let compose = "services:\n  api:\n    build: .\n    volumes:\n      - cache-data:/srv/cache\nvolumes:\n  cache-data: {}\n";
    let r = Repo::new(
        "existing",
        &[
            ("package.json", pkg),
            ("Dockerfile", df),
            ("server.js", server),
            ("compose.yml", compose),
        ],
    );
    let a = r.analyze("");
    let c = a.compose.clone().unwrap();
    assert!(!c.contains("ffmpeg") && !c.contains("uploads"), "不修改用戶設定\n{c}");
    let f = finding(&a, "system_package", "`fluent-ffmpeg`");
    assert_eq!(f.confidence, Confidence::Medium);
    assert!(f.message.contains("`api`") && f.message.contains("Dockerfile"));
    assert!(!a.findings.iter().any(|f| f.message.contains("`bcrypt`")), "已安裝編譯工具");
    let p = finding(&a, "persistent_dir", "`uploads/`");
    assert!(p.message.contains("/srv/uploads"), "{}", p.message);
    assert!(
        !a.findings
            .iter()
            .any(|f| f.key == "persistent_dir" && f.message.contains("`cache/`")),
        "已掛 volume"
    );
}
