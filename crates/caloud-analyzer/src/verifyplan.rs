//! 驗證計畫（M6，[ADR 0020](../../docs/adr/0020-verification.md)）：專案測試、lint 指令與煙霧測試路徑。
//!
//! 依語言偵測（Node `scripts.test`／`scripts.lint`、`cargo test`、`go test ./...`、`pytest`），
//! `.caloud/verify.yml` 可整個覆寫。
use crate::fs::RepoFs;
use crate::{Confidence, Ctx};
use serde::{Deserialize, Serialize};

/// 在 gVisor 測試沙箱中執行的指令
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCmd {
    pub image: String,
    #[serde(default)]
    pub setup: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VerifyPlan {
    #[serde(default)]
    pub test: Option<VerifyCmd>,
    #[serde(default)]
    pub lint: Option<VerifyCmd>,
    /// 煙霧測試的路徑（至少 `/`）
    #[serde(default)]
    pub smoke: Vec<String>,
    /// 來自 `.caloud/verify.yml`
    #[serde(default)]
    pub custom: bool,
}

/// `.caloud/verify.yml` 的格式；`lint` 的 image、setup 省略時沿用 `test`
#[derive(Debug, Deserialize)]
struct FileCmd {
    image: Option<String>,
    setup: Option<String>,
    command: String,
}

#[derive(Debug, Deserialize)]
struct VerifyFile {
    test: Option<FileCmd>,
    lint: Option<FileCmd>,
    smoke: Option<Vec<String>>,
}

pub const VERIFY_FILE: &str = ".caloud/verify.yml";

/// npm init 產生的預設 test 指令不算測試
fn placeholder(script: &str) -> bool {
    script.contains("no test specified")
}

fn valid_path(p: &str) -> bool {
    p.starts_with('/') && p.len() <= 200 && !p.contains(char::is_whitespace) && !p.contains("://")
}

fn valid_image(s: &str) -> bool {
    !s.is_empty() && s.len() <= 200 && s.chars().all(|c| c.is_ascii_alphanumeric() || "/:.-_@".contains(c))
}

/// 依語言偵測
fn detect(fs: &RepoFs) -> VerifyPlan {
    let mut plan = VerifyPlan {
        smoke: vec!["/".into()],
        ..Default::default()
    };
    if let Some(text) = fs.read("package.json") {
        let pkg: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let script = |k: &str| pkg["scripts"][k].as_str().map(str::to_string);
        let node_v = pkg["engines"]["node"]
            .as_str()
            .and_then(crate::templates::major)
            .or_else(|| fs.read(".nvmrc").as_deref().and_then(crate::templates::major))
            .filter(|v| (18..=30).contains(v))
            .unwrap_or(22);
        let (setup, run) = if fs.is_file("pnpm-lock.yaml") {
            ("corepack enable && pnpm install --frozen-lockfile", "pnpm")
        } else if fs.is_file("yarn.lock") {
            ("corepack enable && yarn install --frozen-lockfile", "yarn")
        } else if fs.is_file("package-lock.json") {
            ("npm ci", "npm run")
        } else {
            ("npm install", "npm run")
        };
        let cmd = |name: &str| VerifyCmd {
            image: format!("node:{node_v}-bookworm"),
            setup: setup.into(),
            command: format!("{run} {name}"),
        };
        if script("test").is_some_and(|t| !placeholder(&t)) {
            plan.test = Some(cmd("test"));
        }
        if script("lint").is_some() {
            plan.lint = Some(cmd("lint"));
        }
    } else if fs.is_file("Cargo.toml") {
        plan.test = Some(VerifyCmd {
            image: "rust:1-bookworm".into(),
            setup: String::new(),
            command: "cargo test --locked || cargo test".into(),
        });
    } else if let Some(gomod) = fs.read("go.mod") {
        let v = gomod
            .lines()
            .find_map(|l| l.strip_prefix("go "))
            .map(|v| v.trim().split('.').take(2).collect::<Vec<_>>().join("."))
            .filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .unwrap_or_else(|| "1".into());
        plan.test = Some(VerifyCmd {
            image: format!("golang:{v}-bookworm"),
            setup: String::new(),
            command: "go test ./...".into(),
        });
    } else {
        let deps = crate::templates::python_deps(fs);
        let has_pytest = deps.iter().any(|(n, _)| n == "pytest") || fs.is_file("pytest.ini") || fs.is_file("conftest.py");
        if has_pytest {
            let setup = if fs.is_file("requirements.txt") {
                "pip install -r requirements.txt pytest"
            } else {
                "pip install . pytest"
            };
            plan.test = Some(VerifyCmd {
                image: "python:3.12-bookworm".into(),
                setup: setup.into(),
                command: "pytest".into(),
            });
        }
    }
    plan
}

/// 偵測或讀取 `.caloud/verify.yml`；檔案格式錯誤時列為 finding 並沿用偵測結果
pub(crate) fn plan(cx: &mut Ctx) -> VerifyPlan {
    let detected = detect(cx.fs);
    let Some(text) = cx.fs.read(VERIFY_FILE) else {
        return detected;
    };
    let ev = cx.ev(VERIFY_FILE, None);
    let file: VerifyFile = match serde_yaml::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            cx.note(
                "verify_file",
                Confidence::Low,
                format!("`{VERIFY_FILE}` 無法解析（{e}），改用自動偵測的測試指令。"),
                vec![ev],
            );
            return detected;
        }
    };
    let base = detected.test.clone();
    let to_cmd = |c: FileCmd, fallback: Option<&VerifyCmd>| -> Option<VerifyCmd> {
        let image = c.image.or_else(|| fallback.map(|f| f.image.clone()))?;
        valid_image(&image).then(|| VerifyCmd {
            image,
            setup: c.setup.or_else(|| fallback.map(|f| f.setup.clone())).unwrap_or_default(),
            command: c.command,
        })
    };
    let test = file.test.and_then(|c| to_cmd(c, base.as_ref()));
    let lint = file.lint.and_then(|c| to_cmd(c, test.as_ref().or(base.as_ref())));
    let mut smoke: Vec<String> = file
        .smoke
        .unwrap_or_default()
        .into_iter()
        .filter(|p| valid_path(p))
        .take(20)
        .collect();
    if smoke.is_empty() {
        smoke.push("/".into());
    }
    cx.note(
        "verify_file",
        Confidence::High,
        format!("使用 `{VERIFY_FILE}` 的驗證設定。"),
        vec![ev],
    );
    VerifyPlan {
        test,
        lint,
        smoke,
        custom: true,
    }
}
