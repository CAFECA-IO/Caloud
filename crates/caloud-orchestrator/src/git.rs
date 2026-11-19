//! 取得公開儲存庫（FR-APP-13、FR-GH-04）：Git 來源 App 與平臺專案共用。
//!
//! 安全設定見 docs/07 §1「匯入公開儲存庫」：只走 https、不跟隨 symlink、不執行 hook、不下載 LFS、
//! 淺層取得、逾時與大小上限。
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// checkout 後的大小上限
pub const MAX_REPO_BYTES: u64 = 1024 * 1024 * 1024;
/// 整個取得流程的逾時
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// 取得結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedRepo {
    /// checkout 的 commit（40 字元十六進位；取不到時為空字串）
    pub commit: String,
    /// 未指定 ref 時的預設分支名稱
    pub branch: Option<String>,
}

const SAFE_CONFIG: &[&str] = &[
    "protocol.allow=never",
    "protocol.https.allow=always",
    "core.symlinks=false",
    "core.hooksPath=/dev/null",
    "advice.detachedHead=false",
];

fn git(git_bin: &str, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(git_bin);
    c.arg("-C").arg(dir);
    for kv in SAFE_CONFIG {
        c.arg("-c").arg(kv);
    }
    c.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    c
}

async fn run(mut cmd: Command) -> anyhow::Result<String> {
    let out = cmd.output().await?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 目錄大小（不跟隨 symlink）；超過 `limit` 即停止並回傳 None
fn dir_size(dir: &Path, limit: u64) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                total += m.len();
                if total > limit {
                    return None;
                }
            }
        }
    }
    Some(total)
}

/// 清空 `dir` 後淺層取得 `url` 的 `git_ref`（None 為預設分支）並 checkout
pub async fn fetch_repo(git_bin: &str, dir: &Path, url: &str, git_ref: Option<&str>) -> anyhow::Result<FetchedRepo> {
    fetch_repo_limited(git_bin, dir, url, git_ref, None, MAX_REPO_BYTES).await
}

/// 同 [`fetch_repo`]，以 HTTP 標頭帶入權杖（私有儲存庫，ADR 0017）。
///
/// 標頭經 `GIT_CONFIG_COUNT`／`GIT_CONFIG_KEY_0`／`GIT_CONFIG_VALUE_0` 環境變數傳給 git：
/// 不出現在網址、命令列參數或 `.git/config`。
pub async fn fetch_repo_with_auth(
    git_bin: &str,
    dir: &Path,
    url: &str,
    git_ref: Option<&str>,
    auth_header: &str,
) -> anyhow::Result<FetchedRepo> {
    fetch_repo_limited(git_bin, dir, url, git_ref, Some(auth_header), MAX_REPO_BYTES).await
}

/// 只對需要網路的指令加上授權標頭
fn with_auth(mut cmd: Command, auth: Option<&str>) -> Command {
    if let Some(h) = auth {
        cmd.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env("GIT_CONFIG_VALUE_0", h);
    }
    cmd
}

pub(crate) async fn fetch_repo_limited(
    git_bin: &str,
    dir: &Path,
    url: &str,
    git_ref: Option<&str>,
    auth: Option<&str>,
    max_bytes: u64,
) -> anyhow::Result<FetchedRepo> {
    let job = async {
        if tokio::fs::try_exists(dir).await? {
            tokio::fs::remove_dir_all(dir).await?;
        }
        tokio::fs::create_dir_all(dir).await?;
        run(git(git_bin, dir, &["init", "-q"])).await?;
        let r = git_ref.unwrap_or("HEAD");
        run(with_auth(
            git(git_bin, dir, &["fetch", "-q", "--depth", "1", "--no-tags", url, r]),
            auth,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("無法取得 {url} 的 {r}：{e}"))?;
        run(git(git_bin, dir, &["checkout", "-q", "FETCH_HEAD"])).await?;
        let commit = run(git(git_bin, dir, &["rev-parse", "HEAD"]))
            .await
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let branch = match git_ref {
            Some(_) => None,
            None => run(with_auth(git(git_bin, dir, &["ls-remote", "--symref", url, "HEAD"]), auth))
                .await
                .ok()
                .and_then(|out| parse_symref(&out)),
        };
        anyhow::Ok(FetchedRepo { commit, branch })
    };
    let fetched = tokio::time::timeout(FETCH_TIMEOUT, job)
        .await
        .map_err(|_| anyhow::anyhow!("取得儲存庫逾時（{} 秒）。", FETCH_TIMEOUT.as_secs()))??;
    let d = dir.to_path_buf();
    let size = tokio::task::spawn_blocking(move || dir_size(&d, max_bytes)).await?;
    if size.is_none() {
        let _ = tokio::fs::remove_dir_all(dir).await;
        anyhow::bail!("儲存庫超過 {} MB 上限。", max_bytes / 1024 / 1024);
    }
    Ok(fetched)
}

/// `ref: refs/heads/main\tHEAD` → `main`
fn parse_symref(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let rest = l.strip_prefix("ref: refs/heads/")?;
        Some(rest.split(['\t', ' ']).next()?.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_symref() {
        assert_eq!(parse_symref("ref: refs/heads/main\tHEAD\nabc\tHEAD\n").as_deref(), Some("main"));
        assert_eq!(parse_symref("abc\tHEAD\n"), None);
    }

    // FR-GH-04、docs/07：安全設定、預設分支與大小上限
    #[cfg(unix)]
    #[tokio::test]
    async fn fetches_with_safe_config_and_size_limit() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("caloud-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let log = base.join("git.log");
        let fake = base.join("git.sh");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$* [auth=${{GIT_CONFIG_VALUE_0:-none}}]\" >> '{log}'\ncase \"$*\" in *' checkout '*) head -c 2048 /dev/zero > \"$2/blob\";; \
                 *' rev-parse '*) echo 0123456789abcdef0123456789abcdef01234567;; *' ls-remote '*) printf 'ref: refs/heads/trunk\\tHEAD\\n';; esac\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = base.join("src");
        let git_bin = fake.display().to_string();

        let r = fetch_repo(&git_bin, &dir, "https://github.com/x/y", None).await.unwrap();
        assert_eq!(r.commit, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(r.branch.as_deref(), Some("trunk"));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("-c protocol.allow=never -c protocol.https.allow=always -c core.symlinks=false -c core.hooksPath=/dev/null"));
        assert!(calls.contains("fetch -q --depth 1 --no-tags https://github.com/x/y HEAD"));

        let r = fetch_repo(&git_bin, &dir, "https://github.com/x/y", Some("v1")).await.unwrap();
        assert_eq!(r.branch, None);

        let err = fetch_repo_limited(&git_bin, &dir, "https://github.com/x/y", Some("v1"), None, 1024)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("上限"), "{err}");
        assert!(!dir.exists(), "超過上限時刪除");

        // ADR 0017：權杖只經環境變數給需要網路的指令，不在網址與參數中
        std::fs::write(&log, "").unwrap();
        fetch_repo_with_auth(&git_bin, &dir, "https://github.com/x/y", None, "Authorization: Basic dG9r")
            .await
            .unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        for l in calls.lines() {
            let (args, auth) = l.split_once(" [auth=").unwrap();
            assert!(!args.contains("dG9r"), "{l}");
            let net = args.contains(" fetch ") || args.contains(" ls-remote ");
            assert_eq!(auth.starts_with("Authorization: Basic dG9r"), net, "{l}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
