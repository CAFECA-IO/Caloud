//! 秘密外洩掃描（ADR 0020 `static` 階段）：以內建規則比對儲存庫檔案，回報只含規則、位置與遮蔽後的片段。
use caloud_core::agent::Issue;
use std::path::Path;

/// 一條規則：前綴＋允許的字元＋最短長度（前綴之後）
struct Rule {
    id: &'static str,
    title: &'static str,
    prefixes: &'static [&'static str],
    charset: fn(char) -> bool,
    min: usize,
}

fn alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}
fn upper_alnum(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}
fn token(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

const RULES: &[Rule] = &[
    Rule {
        id: "aws-access-key",
        title: "疑似 AWS Access Key",
        prefixes: &["AKIA", "ASIA"],
        charset: upper_alnum,
        min: 16,
    },
    Rule {
        id: "github-token",
        title: "疑似 GitHub 權杖",
        prefixes: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        charset: alnum,
        min: 36,
    },
    Rule {
        id: "github-pat",
        title: "疑似 GitHub 權杖",
        prefixes: &["github_pat_"],
        charset: token,
        min: 60,
    },
    Rule {
        id: "gitlab-token",
        title: "疑似 GitLab 權杖",
        prefixes: &["glpat-"],
        charset: token,
        min: 20,
    },
    Rule {
        id: "slack-token",
        title: "疑似 Slack 權杖",
        prefixes: &["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"],
        charset: token,
        min: 10,
    },
    Rule {
        id: "stripe-live-key",
        title: "疑似 Stripe 正式金鑰",
        prefixes: &["sk_live_", "rk_live_"],
        charset: alnum,
        min: 20,
    },
    Rule {
        id: "google-api-key",
        title: "疑似 Google API 金鑰",
        prefixes: &["AIza"],
        charset: token,
        min: 35,
    },
    Rule {
        id: "npm-token",
        title: "疑似 npm 權杖",
        prefixes: &["npm_"],
        charset: alnum,
        min: 36,
    },
];

/// 不掃描的目錄
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "__pycache__",
    ".venv",
    "venv",
    "coverage",
];
/// 範例與鎖定檔
const SKIP_FILES: &[&str] = &[
    ".env.example",
    ".env.sample",
    ".env.template",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "Cargo.lock",
    "go.sum",
    "poetry.lock",
];
const MAX_FILE: u64 = 1024 * 1024;
const MAX_FILES: usize = 20_000;
const MAX_ISSUES: usize = 100;

/// 前 4、後 4 字元
pub fn redact(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= 8 {
        return "****".into();
    }
    format!(
        "{}…{}",
        c[..4].iter().collect::<String>(),
        c[c.len() - 4..].iter().collect::<String>()
    )
}

/// 一行中命中的 (規則, 片段)
fn scan_line(line: &str) -> Vec<(&'static Rule, String)> {
    let mut out = Vec::new();
    for r in RULES {
        for p in r.prefixes {
            let mut start = 0;
            while let Some(i) = line[start..].find(p) {
                let at = start + i;
                // 前一個字元若是英數，代表是更長字串的一部分（例：`XAKIA…`）
                let prev_ok = line[..at].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric());
                let rest: String = line[at + p.len()..].chars().take_while(|c| (r.charset)(*c)).collect();
                if prev_ok && rest.chars().count() >= r.min {
                    out.push((r, format!("{p}{rest}")));
                }
                start = at + p.len();
            }
        }
    }
    if line.contains("-----BEGIN") && line.contains("PRIVATE KEY-----") {
        static KEY: Rule = Rule {
            id: "private-key",
            title: "私鑰",
            prefixes: &[],
            charset: alnum,
            min: 0,
        };
        out.push((&KEY, "-----BEGIN PRIVATE KEY-----".into()));
    }
    out
}

/// 測試、範例目錄的命中降為 Low
fn is_fixture(rel: &str) -> bool {
    rel.split('/')
        .any(|c| ["test", "tests", "__tests__", "fixtures", "spec", "examples", "example", "testdata"].contains(&c))
}

/// 掃描儲存庫
pub fn scan(repo: &Path) -> Vec<Issue> {
    let mut out = Vec::new();
    let mut stack = vec![repo.to_path_buf()];
    let mut files = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
            let name = e.file_name().to_string_lossy().to_string();
            if meta.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(e.path());
                }
                continue;
            }
            if !meta.is_file() || meta.len() > MAX_FILE || SKIP_FILES.contains(&name.as_str()) {
                continue;
            }
            files += 1;
            if files > MAX_FILES || out.len() >= MAX_ISSUES {
                stack.clear();
                break;
            }
            let Ok(bytes) = std::fs::read(e.path()) else { continue };
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let rel = e.path().strip_prefix(repo).unwrap_or(&e.path()).display().to_string();
            for (i, line) in text.lines().enumerate() {
                for (rule, hit) in scan_line(line) {
                    out.push(Issue {
                        severity: if is_fixture(&rel) { "low" } else { "high" }.into(),
                        stage: "static".into(),
                        rule: format!("secret/{}", rule.id),
                        title: format!("{}出現在原始碼中", rule.title),
                        detail: format!("`{}`；請撤銷並改用秘密變數。", redact(&hit)),
                        file: Some(rel.clone()),
                        line: Some(i as u32 + 1),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ADR 0020：命中、誤判、遮蔽
    #[test]
    fn finds_and_redacts_secrets() {
        let base = std::env::temp_dir().join(format!("caloud-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let w = |f: &str, t: &str| {
            let p = base.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        let aws = format!("AKIA{}", "ABCDEFGHIJKLMNOP");
        let gh = format!("ghp_{}", "a".repeat(36));
        w("src/config.ts", &format!("const k = '{aws}';\nconst t = \"{gh}\";\n"));
        w("keys/id_rsa", "-----BEGIN RSA PRIVATE KEY-----\nabc\n");
        w("tests/fixtures/key.pem", "-----BEGIN PRIVATE KEY-----\n");
        w(".env.example", &format!("AWS={aws}\n"));
        w("node_modules/x/index.js", &format!("{gh}\n"));
        w("src/ok.ts", "const s = 'XAKIAABCDEFGHIJKLMNOP'; const g = 'ghp_short';\n");
        let issues = scan(&base);
        let rules: Vec<(&str, &str, &str)> = issues
            .iter()
            .map(|i| (i.rule.as_str(), i.file.as_deref().unwrap(), i.severity.as_str()))
            .collect();
        assert_eq!(
            rules,
            vec![
                ("secret/private-key", "keys/id_rsa", "high"),
                ("secret/aws-access-key", "src/config.ts", "high"),
                ("secret/github-token", "src/config.ts", "high"),
                ("secret/private-key", "tests/fixtures/key.pem", "low"),
            ]
        );
        for i in &issues {
            assert!(!i.detail.contains(&aws) && !i.detail.contains(&gh), "{}", i.detail);
        }
        assert!(issues[1].detail.contains("AKIA…MNOP"));
        assert_eq!(issues[1].line, Some(1));
        let _ = std::fs::remove_dir_all(&base);
    }
}
