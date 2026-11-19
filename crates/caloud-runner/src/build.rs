//! 從原始碼部署（FR-ENV-12，ADR 0018）：取得原始碼、在 gVisor 臨時容器中以 BuildKit 建置、改寫 compose。
//!
//! - 每次建置一個 `buildctl-daemonless.sh` 容器（runsc、`caloud-build` 網路），之間不共用 daemon 或 socket；
//! - 快取只在同一個環境的 volume 內共用；
//! - 產出 `docker load` 為 `caloud-env/<env>/<service>:<commit 前 12 碼>`，compose 改為 image 後交給既有流程。
use caloud_core::agent::{AgentMsg, EnvSource};
use caloud_orchestrator::{validate_with, Policy};
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// 整個建置階段的逾時
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// 建置用的 docker 網路（bridge 名稱 `cldbuild`，沿用 `cld+` 防火牆）
pub const BUILD_NETWORK: &str = "caloud-build";
/// 以 digest 固定的 rootless BuildKit 映像（v0.35.0-rootless）
pub const BUILDKIT_IMAGE: &str = "moby/buildkit@sha256:5987568f92dafbfb679879de59c1cd8d26597f5f6635c314e166164e2615870c";
/// 平臺產生的映像前綴；compose 不可自行引用
pub const IMAGE_PREFIX: &str = "caloud-env/";
/// 每行 log 上限（字元）
pub const MAX_LINE: usize = 500;
/// 每個階段 log 上限（行）
pub const MAX_LINES: usize = 2000;
/// 每個環境保留的映像數
pub const KEEP_IMAGES: usize = 2;

/// 逐行回報 log（ADR 0018 §3）
pub struct Log<'a> {
    tx: Option<&'a UnboundedSender<AgentMsg>>,
    env_id: &'a str,
    stage: &'static str,
    count: usize,
    /// 最後幾行，失敗時當作錯誤訊息
    pub tail: Vec<String>,
}

impl<'a> Log<'a> {
    pub fn new(tx: Option<&'a UnboundedSender<AgentMsg>>, env_id: &'a str, stage: &'static str) -> Self {
        Self {
            tx,
            env_id,
            stage,
            count: 0,
            tail: Vec::new(),
        }
    }

    pub fn line(&mut self, raw: &str) {
        let line: String = raw.trim_end().chars().take(MAX_LINE).collect();
        if line.is_empty() {
            return;
        }
        self.tail.push(line.clone());
        if self.tail.len() > 8 {
            self.tail.remove(0);
        }
        self.count += 1;
        let line = match self.count {
            n if n < MAX_LINES => line,
            n if n == MAX_LINES => "…（log 過長，以下省略）".to_string(),
            _ => return,
        };
        if let Some(tx) = self.tx {
            let _ = tx.send(AgentMsg::EnvLog {
                env_id: self.env_id.to_string(),
                stage: self.stage.to_string(),
                line,
            });
        }
    }
}

/// 執行指令並把 stdout／stderr 逐行送進 log；回傳是否成功
pub async fn run_logged(mut cmd: Command, log: &mut Log<'_>) -> anyhow::Result<()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let pipe = |r: Box<dyn tokio::io::AsyncRead + Unpin + Send>, tx: tokio::sync::mpsc::UnboundedSender<String>| {
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if tx.send(l).is_err() {
                    break;
                }
            }
        })
    };
    if let Some(o) = child.stdout.take() {
        pipe(Box::new(o), tx.clone());
    }
    if let Some(e) = child.stderr.take() {
        pipe(Box::new(e), tx.clone());
    }
    drop(tx);
    while let Some(l) = rx.recv().await {
        log.line(&l);
    }
    let st = child.wait().await?;
    if !st.success() {
        anyhow::bail!("{}", log.tail.join("\n"));
    }
    Ok(())
}

/// 映像名稱只能用小寫英數與 `-`
fn image_part(s: &str) -> String {
    let p: String = s
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let p = p.trim_matches('-').to_string();
    if p.is_empty() {
        "svc".into()
    } else {
        p
    }
}

pub fn image_tag(env_id: &str, service: &str, commit: &str) -> String {
    let c: String = commit.chars().filter(char::is_ascii_hexdigit).take(12).collect();
    format!(
        "{IMAGE_PREFIX}{env_id}/{}:{}",
        image_part(service),
        if c.is_empty() { "latest".into() } else { c }
    )
}

/// 一個要建置的服務
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildJob {
    pub service: String,
    /// 儲存庫內的 context 目錄
    pub context: PathBuf,
    /// Dockerfile 所在目錄與檔名（`dockerfile_inline` 時為 None，由呼叫端寫入暫存檔）
    pub dockerfile: Option<(PathBuf, String)>,
    pub inline: Option<String>,
    pub target: Option<String>,
    pub args: Vec<(String, String)>,
    pub tag: String,
}

/// 路徑必須留在儲存庫內（含 symlink 解析後）
fn inside(repo: &Path, p: &Path) -> anyhow::Result<PathBuf> {
    let root = std::fs::canonicalize(repo)?;
    let real = std::fs::canonicalize(p).map_err(|_| anyhow::anyhow!("找不到 `{}`", rel(repo, p)))?;
    if !real.starts_with(&root) {
        anyhow::bail!("`{}` 不在儲存庫內", rel(repo, p));
    }
    Ok(real)
}

fn rel(repo: &Path, p: &Path) -> String {
    p.strip_prefix(repo).unwrap_or(p).display().to_string()
}

fn build_args(v: Option<&Value>) -> Vec<(String, String)> {
    let scalar = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };
    match v {
        Some(Value::Mapping(m)) => m.iter().filter_map(|(k, v)| Some((k.as_str()?.to_string(), scalar(v)?))).collect(),
        Some(Value::Sequence(l)) => l
            .iter()
            .filter_map(|x| x.as_str()?.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect(),
        _ => vec![],
    }
}

/// 檢查 compose、列出要建置的服務，並回傳改寫成 image 的 compose。
///
/// - 以平臺政策（含 compose 目錄深度）檢查；
/// - 不可自行引用平臺產生的映像（`caloud-env/…`）；
/// - 有 `profiles` 的服務（工具類）不會被 `up` 啟動，直接移除。
pub fn plan(compose: &str, repo: &Path, src: &EnvSource, env_id: &str) -> anyhow::Result<(Vec<BuildJob>, String)> {
    let depth = src.compose_dir.split('/').filter(|c| !c.is_empty()).count();
    let validated = validate_with(compose, Policy::platform(depth)).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut doc = validated.doc;
    let base = repo.join(&src.compose_dir);
    let mut jobs = Vec::new();
    let services = doc
        .get_mut("services")
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| anyhow::anyhow!("compose 沒有 services"))?;
    services.retain(|_, svc| svc.get("profiles").is_none());
    for (name, svc) in services.iter_mut() {
        let name = name.as_str().unwrap_or_default().to_string();
        let Some(svc) = svc.as_mapping_mut() else { continue };
        if let Some(img) = svc.get("image").and_then(Value::as_str) {
            if img.to_ascii_lowercase().starts_with(IMAGE_PREFIX) {
                anyhow::bail!("service `{name}`：不可使用平臺保留的映像名稱 `{img}`");
            }
        }
        let Some(build) = svc.remove("build") else { continue };
        let empty = Mapping::new();
        let m = match &build {
            Value::Mapping(m) => m,
            _ => &empty,
        };
        let context = match &build {
            Value::String(c) => c.clone(),
            _ => m.get("context").and_then(Value::as_str).unwrap_or(".").to_string(),
        };
        let ctx = inside(repo, &base.join(&context))?;
        if !ctx.is_dir() {
            anyhow::bail!("service `{name}`：build context `{context}` 不是目錄");
        }
        let inline = m.get("dockerfile_inline").and_then(Value::as_str).map(str::to_string);
        let dockerfile = match &inline {
            Some(_) => None,
            None => {
                let f = m.get("dockerfile").and_then(Value::as_str).unwrap_or("Dockerfile");
                let path = inside(repo, &ctx.join(f)).map_err(|e| anyhow::anyhow!("service `{name}`：{e}"))?;
                let dir = path.parent().unwrap_or(&ctx).to_path_buf();
                let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("Dockerfile").to_string();
                Some((dir, file))
            }
        };
        let tag = image_tag(env_id, &name, &src.commit);
        svc.insert("image".into(), tag.clone().into());
        if svc.get("pull_policy").and_then(Value::as_str) == Some("build") {
            svc.remove("pull_policy");
        }
        jobs.push(BuildJob {
            service: name,
            context: ctx,
            dockerfile,
            inline,
            target: m.get("target").and_then(Value::as_str).map(str::to_string),
            args: build_args(m.get("args")),
            tag,
        });
    }
    Ok((jobs, serde_yaml::to_string(&doc)?))
}

/// 建置容器名稱（逾時時以此強制移除）
pub fn container_name(env_id: &str, service: &str) -> String {
    format!("caloud-build-{env_id}-{}", image_part(service))
}

/// `docker run … buildctl-daemonless.sh build …` 的參數（ADR 0018 §2）
pub fn build_command(job: &BuildJob, env_id: &str, image: &str, resolv: &Path, df_dir: &Path, df_name: &str, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "run",
        "--rm",
        "--name",
        &container_name(env_id, &job.service),
        "--runtime",
        "runsc",
        "--network",
        BUILD_NETWORK,
        "--cpus",
        "4",
        "--memory",
        "8g",
        "--pids-limit",
        "4096",
        "--label",
        "caloud.build=1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for (src, dst, mode) in [
        (resolv.display().to_string(), "/etc/resolv.conf", ":ro"),
        (job.context.display().to_string(), "/ctx", ":ro"),
        (df_dir.display().to_string(), "/df", ":ro"),
        (out.display().to_string(), "/out", ""),
        (format!("caloud-bkcache-{env_id}"), "/home/user/.local/share/buildkit", ""),
    ] {
        a.push("-v".into());
        a.push(format!("{src}:{dst}{mode}"));
    }
    a.extend(
        [
            "-e",
            "BUILDKITD_FLAGS=--oci-worker-no-process-sandbox",
            "--entrypoint",
            "buildctl-daemonless.sh",
            image,
            "build",
            "--frontend",
            "dockerfile.v0",
            "--local",
            "context=/ctx",
            "--local",
            "dockerfile=/df",
            "--opt",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    a.push(format!("filename={df_name}"));
    if let Some(t) = &job.target {
        a.push("--opt".into());
        a.push(format!("target={t}"));
    }
    for (k, v) in &job.args {
        a.push("--opt".into());
        a.push(format!("build-arg:{k}={v}"));
    }
    a.push("--progress".into());
    a.push("plain".into());
    a.push("--output".into());
    a.push(format!("type=docker,name={},dest=/out/image.tar", job.tag));
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(dir: &str) -> EnvSource {
        EnvSource {
            repo_url: "https://github.com/a/b".into(),
            commit: "98b705d0123456789abcdef0123456789abcdef0".into(),
            git_ref: None,
            compose_dir: dir.into(),
            auth_header: None,
        }
    }

    // FR-ENV-12：compose 改寫為 image、建置參數
    #[test]
    fn plans_builds_and_rewrites_compose() {
        let repo = std::env::temp_dir().join(format!("caloud-build-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("deploy")).unwrap();
        std::fs::create_dir_all(repo.join("app/docker")).unwrap();
        std::fs::write(repo.join("app/docker/Dockerfile.web"), "FROM scratch\n").unwrap();
        let compose = "services:\n  Web_1:\n    build: {context: ../app, dockerfile: docker/Dockerfile.web, target: prod, args: {A: 1, B: x}}\n    pull_policy: build\n  api:\n    build: {context: ../app, dockerfile_inline: \"FROM alpine\\n\"}\n  migrate:\n    build: ../app\n    profiles: [tools]\n  db:\n    image: postgres:16\n";
        let (jobs, out) = plan(compose, &repo, &src("deploy"), "k2m9xq7a").unwrap();
        assert_eq!(jobs.len(), 2);
        let web = &jobs[0];
        assert_eq!(web.tag, "caloud-env/k2m9xq7a/web-1:98b705d01234");
        assert_eq!(web.target.as_deref(), Some("prod"));
        assert_eq!(
            web.args,
            vec![("A".to_string(), "1".to_string()), ("B".to_string(), "x".to_string())]
        );
        let (dfdir, dfname) = web.dockerfile.clone().unwrap();
        assert!(dfdir.ends_with("app/docker") && dfname == "Dockerfile.web");
        assert_eq!(jobs[1].inline.as_deref(), Some("FROM alpine\n"));
        let doc: Value = serde_yaml::from_str(&out).unwrap();
        assert_eq!(doc["services"]["Web_1"]["image"], "caloud-env/k2m9xq7a/web-1:98b705d01234");
        assert!(doc["services"]["Web_1"].get("build").is_none() && doc["services"]["Web_1"].get("pull_policy").is_none());
        assert!(doc["services"].get("migrate").is_none(), "profiles 服務移除");
        assert_eq!(doc["services"]["db"]["image"], "postgres:16");

        let args = build_command(web, "k2m9xq7a", BUILDKIT_IMAGE, Path::new("/r"), &dfdir, &dfname, Path::new("/o")).join(" ");
        for want in [
            "--runtime runsc --network caloud-build",
            "-v /r:/etc/resolv.conf:ro",
            ":/ctx:ro",
            "caloud-bkcache-k2m9xq7a:/home/user/.local/share/buildkit",
            "--entrypoint buildctl-daemonless.sh moby/buildkit@sha256:",
            "--opt filename=Dockerfile.web --opt target=prod --opt build-arg:A=1",
            "type=docker,name=caloud-env/k2m9xq7a/web-1:98b705d01234,dest=/out/image.tar",
        ] {
            assert!(args.contains(want), "{want}\n{args}");
        }

        // 拒絕：平臺保留映像、跳出儲存庫、主機路徑、缺少的 Dockerfile
        for bad in [
            "services:\n  a:\n    image: caloud-env/other/web:abc\n",
            "services:\n  a:\n    build: ../..\n",
            "services:\n  a:\n    image: x\n    volumes: ['/etc:/x']\n",
            "services:\n  a:\n    build: {context: ../app, dockerfile: nope}\n",
        ] {
            assert!(plan(bad, &repo, &src("deploy"), "k2m9xq7a").is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", repo.join("app/escape")).unwrap();
            assert!(plan("services:\n  a:\n    build: ../app/escape\n", &repo, &src("deploy"), "k2m9xq7a").is_err());
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    // ADR 0018 §3：每行與每階段的 log 上限
    #[test]
    fn log_truncates_lines_and_count() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut log = Log::new(Some(&tx), "e1", "build");
        log.line(&"x".repeat(900));
        for i in 0..MAX_LINES + 50 {
            log.line(&format!("l{i}"));
        }
        let mut n = 0;
        let mut first = None;
        let mut last = None;
        while let Ok(AgentMsg::EnvLog { line, stage, .. }) = rx.try_recv() {
            assert_eq!(stage, "build");
            first.get_or_insert(line.clone());
            last = Some(line);
            n += 1;
        }
        assert_eq!(n, MAX_LINES);
        assert_eq!(first.unwrap().chars().count(), MAX_LINE);
        assert!(last.unwrap().contains("省略"));
        assert_eq!(log.tail.len(), 8);
    }

    #[test]
    fn tags_are_valid_image_names() {
        assert_eq!(image_tag("ab12cd34", "My.Svc", "XYZ"), "caloud-env/ab12cd34/my-svc:latest");
        assert_eq!(container_name("ab12cd34", "__"), "caloud-build-ab12cd34-svc");
    }
}
