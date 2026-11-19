//! 執行環境需求（M5-08，[ADR 0014](../../docs/adr/0014-analyzer-crate.md) 增補）：
//!
//! - FR-AN-07 持久化目錄：程式碼寫入工作目錄下的路徑、`*_DIR`／`*_PATH` 變數預設值、SQLite、保留的空目錄
//! - FR-AN-08 系統套件與原生模組：依依賴決定範本要加裝的 apt 套件、編譯工具或框架指令
//! - FR-AN-09 建置時變數：`NEXT_PUBLIC_*`、`VITE_*` 等，以及值是否隨環境不同
//!
//! 程式碼只掃一次（[`CodeScan`]），環境變數與持久化共用，避免重複消耗讀檔額度。
use crate::fs::{line_of, RepoFs};
use crate::{Confidence, Ctx, Evidence, Finding};
use std::collections::BTreeMap;

// ───────────────────────── 程式碼掃描 ─────────────────────────

/// 一次掃描的結果
#[derive(Default)]
pub(crate) struct CodeScan {
    /// 環境變數名稱 → 依據（最多 3 筆）
    pub env: BTreeMap<String, Vec<Evidence>>,
    /// 寫入工作目錄下路徑的訊號
    pub writes: Vec<Signal>,
}

#[derive(Debug, Clone)]
pub(crate) struct Signal {
    /// 程式中的路徑字面量（未正規化）
    pub path: String,
    pub evidence: Evidence,
    pub confidence: Confidence,
    /// 例：`寫入`、`SQLite`、`UPLOAD_DIR 預設值`
    pub why: String,
}

const ENV_PATTERNS: &[&str] = &[
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

/// 寫入檔案或建立目錄的呼叫（第一個字串字面量為路徑）
const WRITE_CALLS: &[&str] = &[
    "writeFile",
    "appendFile",
    "createWriteStream",
    "mkdir",
    "ensureDir",
    "outputFile",
    "dest:",
    "destination:",
    "uploadDir",
    "makedirs(",
    "MEDIA_ROOT",
    "UPLOAD_FOLDER",
    "os.WriteFile(",
    "os.Create(",
    "os.MkdirAll(",
    "fs::write(",
    "File::create(",
    "create_dir_all(",
];

/// 開啟 SQLite 檔案的呼叫
const SQLITE_CALLS: &[&str] = &["new Database(", "sqlite3.Database(", "sqlite3.connect(", "sql.Open(\"sqlite3\","];

const CODE_EXTS: &[&str] = &[".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx", ".rs", ".py", ".go"];
const ENV_EXAMPLES: &[&str] = &[".env.example", ".env.sample", ".env.template", "env.example"];

pub(crate) fn is_name(s: &str) -> bool {
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

/// 一行中的字串字面量（依出現順序）；含 `${` 的樣板字串以 None 佔位
fn literals(s: &str) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if !matches!(c, '\'' | '"' | '`') {
            continue;
        }
        let mut lit = String::new();
        let mut closed = false;
        while let Some(d) = chars.next() {
            if d == '\\' {
                if let Some(e) = chars.next() {
                    lit.push(e);
                }
                continue;
            }
            if d == c {
                closed = true;
                break;
            }
            lit.push(d);
        }
        if !closed {
            break;
        }
        out.push((!lit.contains("${") && !lit.contains('{')).then_some(lit));
    }
    out
}

/// 呼叫的第一個參數若是路徑字面量（或 `path.join(process.cwd(), 'x')` 這類以工作目錄為基準的組合）則回傳
fn first_arg_path(after_call: &str) -> Option<String> {
    let open = after_call.find('(')?;
    // 呼叫名稱與 `(` 之間只能是 Sync／Promise 之類的字尾
    if !after_call[..open].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    arg_path(top_arg(&after_call[open + 1..])?)
}

/// 第一個頂層參數（到 `,` 或 `)` 為止）
fn top_arg(s: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') if depth == 0 => return Some(s[..i].trim()),
            (None, ')' | ']' | '}') => depth -= 1,
            (None, ',') if depth == 0 => return Some(s[..i].trim()),
            _ => {}
        }
    }
    None
}

/// 參數是字面量，或以工作目錄為基準組合的路徑
fn arg_path(arg: &str) -> Option<String> {
    let lits = literals(arg);
    if arg.starts_with(['\'', '"', '`']) {
        return lits.into_iter().next().flatten();
    }
    const CWD: &[&str] = &["process.cwd()", "os.getcwd()", "BASE_DIR", "Path.cwd()"];
    let joined = ["path.join(", "path.resolve(", "os.path.join(", "join(", "resolve("]
        .iter()
        .find_map(|j| arg.strip_prefix(j))?;
    let parts: Vec<&str> = joined.trim_end_matches(')').split(',').map(str::trim).collect();
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if p.starts_with(['\'', '"']) {
            out.push(literals(p).into_iter().next().flatten()?);
        } else if !(i == 0 && CWD.contains(p)) {
            return None;
        }
    }
    (!out.is_empty()).then(|| out.join("/"))
}

/// 匯入語句（`import … from 'fs/promises'`）不是寫入
fn is_import(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("import ") || t.starts_with("from ") || t.starts_with("export ") && t.contains(" from ") || t.contains("require(")
}

/// 目錄變數的預設值是否像路徑：`*_PATH` 的裸名稱多半是執行檔（例：`ffmpeg`）
fn dir_default(name: &str, v: &str) -> bool {
    !v.is_empty() && !v.starts_with('/') && (v.starts_with("./") || v.contains('/') || !name.ends_with("_PATH"))
}

/// 去掉 `/* … */` 註解
fn strip_block_comments(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i..].find("*/") {
            Some(j) => rest = &rest[i + j + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// 變數名稱看起來是目錄或路徑設定
fn dirish(name: &str) -> bool {
    ["_DIR", "_PATH", "_FOLDER", "_ROOT"].iter().any(|s| name.ends_with(s)) || name.starts_with("UPLOAD") || name.starts_with("STORAGE")
}

fn is_test(f: &str) -> bool {
    f.split('/')
        .any(|c| ["test", "tests", "__tests__", "e2e", "spec", "fixtures"].contains(&c))
        || [".test.", ".spec.", "_test."].iter().any(|t| f.contains(t))
}

/// 部署、維運腳本不在執行環境中執行（只用於持久化判斷）
fn is_script(f: &str) -> bool {
    f.split('/').any(|c| ["scripts", "script", "tools", "seed", "seeds"].contains(&c))
}

fn is_comment(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("//") || t.starts_with('#') || t.starts_with('*') || t.starts_with("/*")
}

/// 掃描 `.env` 範例檔與程式碼一次
pub(crate) fn scan_code(fs: &RepoFs) -> CodeScan {
    let mut scan = CodeScan::default();
    let ev = |f: &str, i: usize| Evidence {
        file: fs.display(f),
        line: Some(i as u32 + 1),
    };
    fn add(scan: &mut CodeScan, name: &str, e: Evidence) {
        let v = scan.env.entry(name.to_string()).or_default();
        if v.len() < 3 {
            v.push(e);
        }
    }
    for f in ENV_EXAMPLES {
        let Some(t) = fs.read(f) else { continue };
        for (i, l) in t.lines().enumerate() {
            let l = l.trim().trim_start_matches("export ").trim();
            if l.starts_with('#') {
                continue;
            }
            let Some((k, v)) = l.split_once('=') else { continue };
            let k = k.trim();
            if !is_name(k) {
                continue;
            }
            add(&mut scan, k, ev(f, i));
            let v = v.trim().trim_matches(['"', '\'']);
            if dirish(k) && dir_default(k, v) {
                scan.writes.push(Signal {
                    path: v.to_string(),
                    evidence: ev(f, i),
                    confidence: Confidence::Medium,
                    why: format!("`{k}` 預設值"),
                });
            }
        }
    }
    let files = fs.walk("", 6, CODE_EXTS);
    for f in files.iter().filter(|f| !is_test(f)).take(2000) {
        let Some(t) = fs.read(f) else { continue };
        for (i, l) in t.lines().enumerate() {
            if is_comment(l) {
                continue;
            }
            // 環境變數
            if ENV_PATTERNS.iter().any(|p| l.contains(p)) {
                for p in ENV_PATTERNS {
                    for n in names_after(l, p) {
                        add(&mut scan, n, ev(f, i));
                        // `process.env.UPLOAD_DIR || './uploads'`、`os.getenv("DATA_DIR", "./data")`
                        if dirish(n) {
                            let fallback = ["||", "??", ","].iter().find_map(|op| {
                                let after = l.split(n).nth(1)?;
                                let expr = strip_block_comments(after.split_once(op)?.1);
                                let expr = expr.trim();
                                let end = top_arg(&format!("{expr})")).map(str::len).unwrap_or(expr.len());
                                arg_path(expr[..end.min(expr.len())].trim_end_matches(';').trim())
                            });
                            if let Some(path) = fallback.filter(|p| dir_default(n, p)) {
                                scan.writes.push(Signal {
                                    path,
                                    evidence: ev(f, i),
                                    confidence: Confidence::Medium,
                                    why: format!("`{n}` 預設值"),
                                });
                            }
                        }
                    }
                }
            }
            if is_script(f) || is_import(l) {
                continue;
            }
            let push = |scan: &mut CodeScan, path: String, conf: Confidence, why: &str| {
                scan.writes.push(Signal {
                    path,
                    evidence: ev(f, i),
                    confidence: conf,
                    why: why.into(),
                })
            };
            // SQLite
            if let Some(call) = SQLITE_CALLS.iter().find(|c| l.contains(*c)) {
                let after = l.split(call).nth(1).unwrap_or_default();
                if let Some(p) = top_arg(after).and_then(arg_path) {
                    push(&mut scan, p, Confidence::High, "SQLite 資料檔");
                }
                continue;
            }
            // Python open()：只算寫入模式
            if f.ends_with(".py") && l.contains("open(") {
                let after = l.split("open(").nth(1).unwrap_or_default();
                let lits = literals(after);
                let mode_w = lits
                    .get(1)
                    .and_then(|m| m.as_deref())
                    .is_some_and(|m| m.starts_with(['w', 'a', 'x']));
                if let Some(p) = top_arg(after).and_then(arg_path).filter(|_| mode_w) {
                    push(&mut scan, p, Confidence::High, "寫入檔案");
                }
                continue;
            }
            for call in WRITE_CALLS {
                let Some(after) = l.split(call).nth(1) else { continue };
                // `dest: 'uploads/'`、`MEDIA_ROOT = BASE_DIR / 'media'` 這類設定：字面量緊接在後
                let p = if call.ends_with(':') || call.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                    let rest = after.trim_start().trim_start_matches(['=', ' ']);
                    let rest = rest
                        .strip_prefix("BASE_DIR")
                        .map(|r| r.trim_start().trim_start_matches(['/', ' ']))
                        .unwrap_or(rest);
                    rest.starts_with(['\'', '"'])
                        .then(|| literals(rest).into_iter().next().flatten())
                        .flatten()
                } else if call.ends_with('(') {
                    top_arg(after).and_then(arg_path)
                } else {
                    first_arg_path(after)
                };
                if let Some(p) = p {
                    push(&mut scan, p, Confidence::High, "寫入檔案");
                    break;
                }
            }
        }
    }
    scan
}

// ───────────────────────── FR-AN-07 持久化目錄 ─────────────────────────

/// 建置產物、暫存與相依套件目錄：不掛 volume
const NOT_DATA: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "out",
    "target",
    "tmp",
    "temp",
    ".cache",
    "coverage",
    "logs",
    "log",
    "__pycache__",
    ".git",
];

/// 儲存庫保留的空目錄（含 `.gitkeep`）視為資料目錄
const DATA_DIR_NAMES: &[&str] = &["uploads", "storage", "media", "data", "var"];

/// 路徑字面量對應到的位置
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// 工作目錄下的子目錄（儲存庫相對）
    Dir(String),
    /// 直接寫在工作目錄的檔案
    CwdFile(String),
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty() && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn looks_like_file(seg: &str) -> bool {
    seg.rsplit_once('.')
        .is_some_and(|(a, ext)| !a.is_empty() && (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// 相對路徑字面量 → 目錄；絕對路徑、`..`、URL、樣板一律不算
fn target(lit: &str) -> Option<Target> {
    let p = lit.trim().strip_prefix("file:").unwrap_or(lit.trim());
    if p.is_empty() || p.starts_with(['/', '~', '$']) || p.contains("://") || p.contains('\\') || p.contains(':') {
        return None;
    }
    let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    if segs.is_empty() || !segs.iter().all(|s| valid_segment(s)) {
        return None;
    }
    let dirs: Vec<&str> = match segs.last() {
        Some(last) if looks_like_file(last) => segs[..segs.len() - 1].to_vec(),
        _ => segs.clone(),
    };
    let Some(first) = dirs.first() else {
        return Some(Target::CwdFile(segs[0].to_string()));
    };
    if NOT_DATA.contains(first) || first.starts_with('.') {
        return None;
    }
    // public/uploads 這類放在靜態目錄下的資料，掛第二層
    if matches!(*first, "public" | "static") {
        return dirs.get(1).map(|s| Target::Dir(format!("{first}/{s}")));
    }
    Some(Target::Dir(first.to_string()))
}

/// 目錄可以整個掛 volume：儲存庫中不存在，或只有佔位檔
fn mountable(fs: &RepoFs, dir: &str) -> bool {
    if !fs.is_dir(dir) {
        return true;
    }
    fs.list(dir)
        .iter()
        .all(|(n, d)| !d && [".gitkeep", ".keep", ".gitignore", "README.md"].contains(&n.as_str()))
}

/// 需要持久化的目錄
#[derive(Debug, Clone)]
pub(crate) struct Persist {
    pub dir: String,
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
    pub why: Vec<String>,
}

/// FR-AN-07：彙整成目錄。無法整個掛載（目錄內有原始碼，或檔案直接寫在工作目錄）者回傳在 `blocked`
pub(crate) fn persistent(cx: &Ctx) -> (Vec<Persist>, Vec<Finding>) {
    let mut dirs: BTreeMap<String, Persist> = BTreeMap::new();
    let mut blocked = Vec::new();
    let mut merge = |dir: String, conf: Confidence, ev: Evidence, why: &str| {
        let p = dirs.entry(dir.clone()).or_insert_with(|| Persist {
            dir,
            confidence: conf,
            evidence: vec![],
            why: vec![],
        });
        if conf == Confidence::High {
            p.confidence = Confidence::High;
        }
        if p.evidence.len() < 3 && !p.evidence.contains(&ev) {
            p.evidence.push(ev);
        }
        if !p.why.iter().any(|w| w == why) {
            p.why.push(why.to_string());
        }
    };
    for s in &cx.code().writes {
        match target(&s.path) {
            Some(Target::Dir(d)) => merge(d, s.confidence, s.evidence.clone(), &s.why),
            Some(Target::CwdFile(f)) if !blocked.iter().any(|b: &Finding| b.message.contains(&format!("`{f}`"))) => blocked.push(Finding {
                key: "persistent_dir".into(),
                message: format!(
                    "{}寫在工作目錄的 `{f}`，無法單獨掛 volume，重建後會遺失；建議改放到子目錄（例如 `data/{f}`）。",
                    s.why
                ),
                confidence: Confidence::Medium,
                evidence: vec![s.evidence.clone()],
            }),
            _ => {}
        }
    }
    for name in DATA_DIR_NAMES {
        if cx.fs.is_dir(name) && (cx.fs.is_file(&format!("{name}/.gitkeep")) || cx.fs.is_file(&format!("{name}/.keep"))) {
            let ev = cx.ev(&format!("{name}/.gitkeep"), None);
            merge(name.to_string(), Confidence::Medium, ev, "儲存庫保留的空目錄");
        }
    }
    let mut ok = Vec::new();
    for (_, p) in dirs {
        if mountable(cx.fs, &p.dir) {
            ok.push(p);
        } else {
            blocked.push(Finding {
                key: "persistent_dir".into(),
                message: format!(
                    "寫入 `{}/`（{}），但此目錄含原始碼，無法整個掛 volume；資料請改放到獨立目錄。",
                    p.dir,
                    p.why.join("、")
                ),
                confidence: Confidence::Medium,
                evidence: p.evidence,
            });
        }
    }
    (ok, blocked)
}

/// 具名 volume 名稱：`<服務>-<目錄>`（`/` 改為 `-`）
pub(crate) fn volume_name(service: &str, dir: &str) -> String {
    let slug: String = dir
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{service}-{}", slug.trim_matches('-'))
}

// ───────────────────────── FR-AN-08 系統套件與原生模組 ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Node,
    Python,
    Go,
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fix {
    /// apt 套件
    Apt(Vec<String>),
    /// 原生模組需要編譯工具
    BuildTools,
    /// 框架提供的安裝指令（Playwright 瀏覽器）
    Playwright,
    /// Go cgo（go-sqlite3）：CGO_ENABLED=1、debian runtime
    Cgo,
    /// 有 glibc 預編譯檔，不需處理
    Prebuilt,
    /// 範本無法處理，建議自備 Dockerfile
    Manual,
}

#[derive(Debug, Clone)]
pub(crate) struct SysNeed {
    pub dep: String,
    pub what: String,
    pub evidence: Evidence,
    pub fix: Fix,
}

const PUPPETEER_LIBS: &[&str] = &[
    "libnss3",
    "libatk1.0-0",
    "libatk-bridge2.0-0",
    "libcups2",
    "libdrm2",
    "libxkbcommon0",
    "libxcomposite1",
    "libxdamage1",
    "libxfixes3",
    "libxrandr2",
    "libgbm1",
    "libasound2",
    "libpango-1.0-0",
    "libcairo2",
    "fonts-liberation",
];

fn apt(p: &[&str]) -> Fix {
    Fix::Apt(p.iter().map(|s| s.to_string()).collect())
}

/// Debian 套件名稱（寫進 Dockerfile 前檢查，避免 Aptfile 夾帶指令）
fn valid_apt(name: &str) -> bool {
    let mut c = name.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase() || f.is_ascii_digit())
        && name.len() <= 64
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || matches!(x, '.' | '+' | '-'))
}

/// 依語言的依賴清單判斷需要的系統套件（`deps`：名稱與依據）
pub(crate) fn system_needs(cx: &Ctx, lang: Lang, deps: &[(String, Evidence)]) -> Vec<SysNeed> {
    let has = |d: &str| deps.iter().any(|(n, _)| n == d);
    let mut out = Vec::new();
    let mut need = |dep: &str, what: &str, fix: Fix| {
        if let Some((_, ev)) = deps.iter().find(|(n, _)| n == dep) {
            out.push(SysNeed {
                dep: dep.into(),
                what: what.into(),
                evidence: ev.clone(),
                fix,
            });
        }
    };
    match lang {
        Lang::Node => {
            need("playwright", "Chromium", Fix::Playwright);
            if !has("playwright") {
                need("@playwright/test", "Chromium", Fix::Playwright);
            }
            need("puppeteer", "Chromium 相依函式庫", apt(PUPPETEER_LIBS));
            if !has("ffmpeg-static") && !has("@ffmpeg-installer/ffmpeg") {
                need("fluent-ffmpeg", "ffmpeg", apt(&["ffmpeg"]));
            }
            need(
                "canvas",
                "cairo、pango 等函式庫",
                apt(&[
                    "build-essential",
                    "libcairo2-dev",
                    "libpango1.0-dev",
                    "libjpeg-dev",
                    "libgif-dev",
                    "librsvg2-dev",
                ]),
            );
            for d in ["bcrypt", "argon2", "sqlite3", "better-sqlite3", "node-gyp", "node-pty", "re2"] {
                need(d, "編譯工具", Fix::BuildTools);
            }
            need("sharp", "libvips", Fix::Prebuilt);
            need("onnxruntime-node", "onnxruntime", Fix::Prebuilt);
            need("onnxruntime-node-gpu", "CUDA", Fix::Manual);
            need("@tensorflow/tfjs-node-gpu", "CUDA", Fix::Manual);
        }
        Lang::Python => {
            need("opencv-python", "OpenGL 函式庫", apt(&["libgl1", "libglib2.0-0"]));
            need("opencv-contrib-python", "OpenGL 函式庫", apt(&["libgl1", "libglib2.0-0"]));
            need("psycopg2", "libpq", apt(&["libpq-dev", "gcc"]));
            need(
                "mysqlclient",
                "MySQL client 函式庫",
                apt(&["default-libmysqlclient-dev", "pkg-config", "gcc"]),
            );
            need("playwright", "Chromium", Fix::Playwright);
            for d in ["ffmpeg-python", "moviepy", "pydub"] {
                need(d, "ffmpeg", apt(&["ffmpeg"]));
            }
            need("pdf2image", "poppler", apt(&["poppler-utils"]));
            need("pytesseract", "tesseract", apt(&["tesseract-ocr"]));
            need("weasyprint", "pango", apt(&["libpango-1.0-0", "libpangoft2-1.0-0"]));
            need("onnxruntime-gpu", "CUDA", Fix::Manual);
        }
        Lang::Go => need("github.com/mattn/go-sqlite3", "cgo 與 SQLite", Fix::Cgo),
        Lang::Rust => need("openssl-sys", "libssl", apt(&["libssl3"])),
    }
    // Heroku 慣例的 Aptfile
    if let Some(t) = cx.fs.read("Aptfile") {
        let pkgs: Vec<String> = t
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter(|l| valid_apt(l))
            .map(str::to_string)
            .collect();
        let skipped = t
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !valid_apt(l))
            .count();
        if !pkgs.is_empty() {
            out.push(SysNeed {
                dep: "Aptfile".into(),
                what: pkgs.join("、"),
                evidence: cx.ev("Aptfile", None),
                fix: Fix::Apt(pkgs),
            });
        }
        if skipped > 0 {
            out.push(SysNeed {
                dep: "Aptfile".into(),
                what: format!("{skipped} 行不是套件名稱（例如 .deb 網址）"),
                evidence: cx.ev("Aptfile", None),
                fix: Fix::Manual,
            });
        }
    }
    out
}

/// 範本加裝的結果
#[derive(Debug, Default)]
pub(crate) struct Install {
    pub apt: Vec<String>,
    pub build_tools: bool,
    pub playwright: bool,
    pub cgo: bool,
}

/// 把需求套用到範本：回傳要加裝的內容，並記錄判斷
pub(crate) fn plan_install(needs: &[SysNeed], findings: &mut Vec<Finding>) -> Install {
    let mut inst = Install::default();
    for n in needs {
        let (key, conf, msg) = match &n.fix {
            Fix::Apt(p) => {
                for x in p {
                    if !inst.apt.contains(x) {
                        inst.apt.push(x.clone());
                    }
                }
                (
                    "system_package",
                    if n.dep == "Aptfile" { Confidence::Medium } else { Confidence::High },
                    if n.dep == "Aptfile" {
                        format!("依 Aptfile 加裝 {}。", n.what)
                    } else {
                        format!("依賴 `{}` 需要 {}：範本已加裝 {}。", n.dep, n.what, p.join(" "))
                    },
                )
            }
            Fix::BuildTools => {
                inst.build_tools = true;
                (
                    "native_module",
                    Confidence::High,
                    format!("`{}` 是原生模組：範本加裝編譯工具（python3、make、g++）。", n.dep),
                )
            }
            Fix::Playwright => {
                inst.playwright = true;
                (
                    "system_package",
                    Confidence::High,
                    format!("依賴 `{}`：範本以 `playwright install --with-deps chromium` 安裝 Chromium。", n.dep),
                )
            }
            Fix::Cgo => {
                inst.cgo = true;
                (
                    "native_module",
                    Confidence::High,
                    format!("`{}` 需要 cgo：範本以 CGO_ENABLED=1 建置並改用 debian runtime。", n.dep),
                )
            }
            Fix::Prebuilt => (
                "native_module",
                Confidence::High,
                format!("`{}` 使用 linux glibc 預編譯檔，不需額外套件。", n.dep),
            ),
            Fix::Manual => (
                "system_package",
                Confidence::Medium,
                format!("`{}` 需要 {}，範本無法自動安裝；請改用自備 Dockerfile。", n.dep, n.what),
            ),
        };
        findings.push(Finding {
            key: key.into(),
            message: msg,
            confidence: conf,
            evidence: vec![n.evidence.clone()],
        });
    }
    inst
}

/// `RUN apt-get …` 一行（沒有套件時為空字串）
pub(crate) fn apt_run(pkgs: &[String]) -> String {
    if pkgs.is_empty() {
        return String::new();
    }
    format!(
        "RUN apt-get update && apt-get install -y --no-install-recommends {} && rm -rf /var/lib/apt/lists/*\n",
        pkgs.join(" ")
    )
}

/// 用戶 Dockerfile 是否已處理此需求（只看原文）
fn satisfied(dockerfile: &str, need: &SysNeed) -> bool {
    let lower = dockerfile.to_ascii_lowercase();
    match &need.fix {
        Fix::Apt(p) => p.iter().all(|x| lower.contains(x.as_str())),
        Fix::BuildTools => ["build-essential", "g++", "build-base", "gcc"].iter().any(|x| lower.contains(x)),
        Fix::Playwright => lower.contains("playwright install") || lower.contains("playwright:") || lower.contains("chromium"),
        Fix::Cgo => lower.contains("cgo_enabled=1"),
        Fix::Prebuilt | Fix::Manual => true,
    }
}

/// compose／Dockerfile 來源：用戶 Dockerfile 沒有處理的需求列為中信心（不修改用戶檔案）
pub(crate) fn check_dockerfile(findings: &mut Vec<Finding>, service: &str, file: &str, text: &str, needs: &[SysNeed]) {
    for n in needs.iter().filter(|n| !satisfied(text, n)) {
        let what = match &n.fix {
            Fix::Apt(p) => p.join(" "),
            Fix::BuildTools => "編譯工具（build-essential 或 python3、make、g++）".into(),
            Fix::Playwright => "Chromium（playwright install --with-deps chromium）".into(),
            Fix::Cgo => "CGO_ENABLED=1 與 glibc runtime".into(),
            _ => continue,
        };
        findings.push(Finding {
            key: if matches!(n.fix, Fix::BuildTools | Fix::Cgo) {
                "native_module"
            } else {
                "system_package"
            }
            .into(),
            message: format!(
                "依賴 `{}` 需要 {}，但 `{service}` 的 `{file}` 看不到安裝 {what}；請確認建置不會失敗。",
                n.dep, n.what
            ),
            confidence: Confidence::Medium,
            evidence: vec![n.evidence.clone()],
        });
    }
}

/// 儲存庫的語言與依賴清單（名稱、依據），供 compose／Dockerfile 來源檢查
pub(crate) fn repo_deps(cx: &Ctx) -> Option<(Lang, Vec<(String, Evidence)>)> {
    let fs = cx.fs;
    if let Some(t) = fs.read("package.json") {
        let pkg: serde_json::Value = serde_json::from_str(&t).ok()?;
        return Some((Lang::Node, node_deps(cx, &pkg, &t)));
    }
    if let Some(t) = fs.read("Cargo.toml") {
        return Some((Lang::Rust, rust_deps(cx, &t)));
    }
    if let Some(t) = fs.read("go.mod") {
        return Some((Lang::Go, go_deps(cx, &t)));
    }
    if fs.is_file("pyproject.toml") || fs.is_file("requirements.txt") {
        return Some((Lang::Python, python_dep_evidence(cx)));
    }
    None
}

pub(crate) fn node_deps(cx: &Ctx, pkg: &serde_json::Value, text: &str) -> Vec<(String, Evidence)> {
    let mut out: Vec<(String, Evidence)> = ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .filter_map(|k| pkg.get(k).and_then(|d| d.as_object()))
        .flat_map(|m| m.keys().cloned())
        .map(|d| {
            let ev = cx.ev("package.json", line_of(text, &format!("\"{d}\"")));
            (d, ev)
        })
        .collect();
    if pkg.get("gypfile").and_then(|v| v.as_bool()) == Some(true) || cx.fs.is_file("binding.gyp") {
        out.push(("node-gyp".into(), cx.ev("package.json", line_of(text, "gypfile"))));
    }
    out
}

pub(crate) fn go_deps(cx: &Ctx, gomod: &str) -> Vec<(String, Evidence)> {
    ["github.com/mattn/go-sqlite3"]
        .iter()
        .filter(|m| gomod.contains(*m))
        .map(|m| (m.to_string(), cx.ev("go.mod", line_of(gomod, m))))
        .collect()
}

/// Rust：Cargo.lock 中的 `openssl-sys`（直接或間接依賴）
pub(crate) fn rust_deps(cx: &Ctx, cargo_toml: &str) -> Vec<(String, Evidence)> {
    if let Some(lock) = cx.fs.read("Cargo.lock") {
        if let Some(l) = line_of(&lock, "name = \"openssl-sys\"") {
            return vec![("openssl-sys".into(), cx.ev("Cargo.lock", Some(l)))];
        }
        return vec![];
    }
    ["openssl", "native-tls", "openssl-sys"]
        .iter()
        .find_map(|c| line_of(cargo_toml, &format!("{c} ")).or_else(|| line_of(cargo_toml, &format!("{c}="))))
        .map(|l| vec![("openssl-sys".into(), cx.ev("Cargo.toml", Some(l)))])
        .unwrap_or_default()
}

pub(crate) fn python_dep_evidence(cx: &Ctx) -> Vec<(String, Evidence)> {
    crate::templates::python_deps(cx.fs)
        .into_iter()
        .map(|(n, f)| {
            let t = cx.fs.read(&f).unwrap_or_default();
            let ev = cx.ev(&f, line_of(&t.to_ascii_lowercase(), &n));
            (n, ev)
        })
        .collect()
}

// ───────────────────────── FR-AN-09 建置時變數 ─────────────────────────

const BUILD_PREFIXES: &[&str] = &["NEXT_PUBLIC_", "VITE_", "REACT_APP_", "NUXT_PUBLIC_", "EXPO_PUBLIC_", "GATSBY_"];

/// 建置時寫入前端的變數；`PUBLIC_` 只在 SvelteKit／Astro 專案才算
pub(crate) fn is_build_time(name: &str, public_prefix: bool) -> bool {
    BUILD_PREFIXES.iter().any(|p| name.starts_with(p) && name.len() > p.len())
        || (public_prefix && name.starts_with("PUBLIC_") && name.len() > 7)
}

/// 值是否隨環境不同（網址、origin 類）
pub(crate) fn varies_by_env(name: &str) -> bool {
    let tokens: Vec<&str> = name.split('_').filter(|t| !t.is_empty()).collect();
    const URLISH: &[&str] = &[
        "URL", "URI", "ORIGIN", "HOST", "HOSTNAME", "DOMAIN", "ENDPOINT", "BASE", "WS", "WSS",
    ];
    tokens.iter().any(|t| URLISH.contains(t)) || tokens.last() == Some(&"API")
}

/// 專案是否為 SvelteKit／Astro（`PUBLIC_` 前綴為建置時變數）
pub(crate) fn uses_public_prefix(fs: &RepoFs) -> bool {
    fs.read("package.json")
        .is_some_and(|t| t.contains("\"@sveltejs/kit\"") || t.contains("\"astro\""))
}

/// 程式碼中出現的建置時變數名稱（排序）
pub(crate) fn build_env_names(cx: &Ctx) -> Vec<String> {
    let public = uses_public_prefix(cx.fs);
    cx.code().env.keys().filter(|n| is_build_time(n, public)).cloned().collect()
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn literal_and_target_rules() {
        assert_eq!(
            literals(r#"fs.writeFile('./uploads/a.png', "x", `t-${id}`)"#),
            vec![Some("./uploads/a.png".into()), Some("x".into()), None]
        );
        assert_eq!(target("./uploads/a.png"), Some(Target::Dir("uploads".into())));
        assert_eq!(target("data/"), Some(Target::Dir("data".into())));
        assert_eq!(target("public/uploads/x.jpg"), Some(Target::Dir("public/uploads".into())));
        assert_eq!(target("file:./dev.db"), Some(Target::CwdFile("dev.db".into())));
        assert_eq!(target("/tmp/x"), None);
        assert_eq!(target("tmp/x.txt"), None);
        assert_eq!(target("../x"), None);
        assert_eq!(target("https://a.b/c"), None);
        assert_eq!(target("node_modules/.cache"), None);
        assert_eq!(volume_name("web", "public/uploads"), "web-public-uploads");
    }

    // FR-AN-09
    #[test]
    fn build_time_and_varies() {
        assert!(is_build_time("NEXT_PUBLIC_API_URL", false));
        assert!(!is_build_time("NEXT_PUBLIC_", false));
        assert!(!is_build_time("PUBLIC_KEY_PATH", false));
        assert!(is_build_time("PUBLIC_SITE_URL", true));
        assert!(varies_by_env("NEXT_PUBLIC_API_URL"));
        assert!(varies_by_env("VITE_API"));
        assert!(varies_by_env("NEXT_PUBLIC_SITE_ORIGIN"));
        assert!(!varies_by_env("NEXT_PUBLIC_API_KEY"));
        assert!(!varies_by_env("NEXT_PUBLIC_GA_ID"));
    }

    #[test]
    fn apt_names_are_validated() {
        assert!(valid_apt("libgl1"));
        assert!(valid_apt("libatk-bridge2.0-0"));
        assert!(valid_apt("g++"));
        assert!(!valid_apt("ffmpeg && curl evil"));
        assert!(!valid_apt("https://x/y.deb"));
        assert!(!valid_apt("Foo"));
    }
}
