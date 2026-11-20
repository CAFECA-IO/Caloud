//! 在 Runner 上部署與刪除環境（FR-OPS-13，ADR 0015）。
//!
//! 每個環境的檔案在 `<state>/envs/<id>/`：`compose.yml`、`caloud.override.yml`、`resolv.conf`；
//! 以 `docker compose -p caloud-env-<id>` 執行，容器跑在 gVisor 與環境自己的網段。
use crate::build::{self, Log, BUILD_TIMEOUT};
use caloud_core::agent::{valid_env_id, AgentMsg, EnvService, EnvSource, EnvSpec, EnvValue};
use caloud_orchestrator::sandbox::{self, EnvNet, Limits};
use caloud_orchestrator::{validate_with, Policy};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// 一次部署的逾時
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(600);
/// 預設外部 DNS（ADR 0015）
pub const DEFAULT_DNS: [&str; 2] = ["1.1.1.1", "8.8.8.8"];

#[derive(Clone)]
pub struct EnvRunner {
    pub docker_bin: String,
    pub dir: PathBuf,
    pub dns: Vec<String>,
    /// git CLI（從原始碼部署，ADR 0018）
    pub git_bin: String,
    /// 建置用的 BuildKit 映像
    pub buildkit_image: String,
    /// 漏洞掃描映像（ADR 0020）
    pub osv_image: String,
    /// 測試用：覆寫煙霧測試的目標（`host:port`）
    pub smoke_target: Option<String>,
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// 進行中的驗證：ID → 取消訊號
    pub(crate) runs: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
}

impl Default for EnvRunner {
    fn default() -> Self {
        Self::new(
            "docker".into(),
            crate::state::default_dir(),
            DEFAULT_DNS.iter().map(|s| s.to_string()).collect(),
        )
    }
}

fn status(env_id: &str, status: &str, error: Option<String>, services: Vec<EnvService>) -> AgentMsg {
    AgentMsg::EnvStatus {
        env_id: env_id.to_string(),
        status: status.to_string(),
        stage: None,
        error,
        services,
    }
}

/// 部署失敗：附上失敗的階段
fn failed_at(env_id: &str, stage: Option<&str>, error: String) -> AgentMsg {
    AgentMsg::EnvStatus {
        env_id: env_id.to_string(),
        status: "failed".into(),
        stage: stage.map(str::to_string),
        error: Some(error),
        services: vec![],
    }
}

/// 錯誤訊息只取最後幾行，避免過長
pub(crate) fn short(err: &str) -> String {
    let lines: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(5)..].join("\n");
    tail.chars().take(1000).collect()
}

impl EnvRunner {
    pub fn new(docker_bin: String, state_dir: PathBuf, dns: Vec<String>) -> Self {
        Self {
            docker_bin,
            dir: state_dir.join("envs"),
            dns,
            git_bin: "git".into(),
            buildkit_image: build::BUILDKIT_IMAGE.into(),
            osv_image: crate::verify::OSV_IMAGE.into(),
            smoke_target: None,
            locks: Default::default(),
            runs: Default::default(),
        }
    }

    pub(crate) fn builds_dir(&self, env_id: &str) -> PathBuf {
        self.dir.with_file_name("builds").join(env_id)
    }

    pub(crate) fn lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    pub(crate) async fn docker(&self, args: &[&str], timeout: Duration) -> anyhow::Result<String> {
        self.docker_env(args, timeout, &[]).await
    }

    /// 帶入行程環境變數（秘密變數，ADR 0019）；值不出現在參數或檔案中
    async fn docker_env(&self, args: &[&str], timeout: Duration, vars: &[EnvValue]) -> anyhow::Result<String> {
        let mut c = Command::new(&self.docker_bin);
        c.args(args).stdin(Stdio::null()).kill_on_drop(true);
        for v in vars {
            c.env(&v.name, &v.value);
        }
        let out = tokio::time::timeout(timeout, c.output())
            .await
            .map_err(|_| anyhow::anyhow!("docker {} 逾時", args.first().unwrap_or(&"")))??;
        if !out.status.success() {
            anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `docker compose -p <project> --project-directory <dir> -f compose.yml -f caloud.override.yml <args>`
    pub(crate) async fn compose(&self, env_id: &str, args: &[&str], timeout: Duration) -> anyhow::Result<String> {
        self.compose_env(env_id, args, timeout, &[]).await
    }

    pub(crate) async fn compose_env(&self, env_id: &str, args: &[&str], timeout: Duration, vars: &[EnvValue]) -> anyhow::Result<String> {
        let dir = self.dir.join(env_id);
        let project = sandbox::project(env_id);
        let d = dir.display().to_string();
        let compose_file = dir.join("compose.yml").display().to_string();
        let override_file = dir.join("caloud.override.yml").display().to_string();
        let mut full = vec![
            "compose",
            "-p",
            &project,
            "--project-directory",
            &d,
            "-f",
            &compose_file,
            "-f",
            &override_file,
        ];
        full.extend_from_slice(args);
        self.docker_env(&full, timeout, vars).await
    }

    /// 由控制面送來的部署；索引不可使用自我檢查保留的範圍
    pub async fn deploy(&self, env: &EnvSpec) -> AgentMsg {
        self.deploy_with(env, None).await
    }

    /// 同 [`deploy`](Self::deploy)，並把階段與 log 送到 `tx`（ADR 0018）
    pub async fn deploy_with(&self, env: &EnvSpec, tx: Option<&UnboundedSender<AgentMsg>>) -> AgentMsg {
        if env.subnet_index > sandbox::MAX_ALLOC_INDEX {
            return status(&env.id, "failed", Some("網段索引超出範圍。".into()), vec![]);
        }
        self.deploy_inner(env, tx).await
    }

    pub(crate) async fn deploy_unchecked(&self, env: &EnvSpec) -> AgentMsg {
        self.deploy_inner(env, None).await
    }

    async fn deploy_inner(&self, env: &EnvSpec, tx: Option<&UnboundedSender<AgentMsg>>) -> AgentMsg {
        if !valid_env_id(&env.id) {
            return status(&env.id, "failed", Some("環境 ID 格式錯誤。".into()), vec![]);
        }
        let lock = self.lock(&env.id);
        let _guard = lock.lock().await;
        let stage = |s: &str| {
            if let Some(tx) = tx {
                let _ = tx.send(AgentMsg::EnvStatus {
                    env_id: env.id.clone(),
                    status: "deploying".into(),
                    stage: Some(s.into()),
                    error: None,
                    services: vec![],
                });
            }
        };
        let secrets: Vec<String> = env.vars.iter().map(|v| v.value.clone()).collect();
        let (compose, built) = match &env.source {
            None => (env.compose.clone(), vec![]),
            Some(src) => match self.build_source(env, src, tx, &stage).await {
                Ok(c) => c,
                Err((st, e)) => {
                    tracing::warn!(env = %env.id, stage = st, "build failed");
                    let _ = tokio::fs::remove_dir_all(self.builds_dir(&env.id)).await;
                    return failed_at(&env.id, Some(st), build::mask(&short(&e.to_string()), &secrets));
                }
            },
        };
        if env.source.is_some() {
            stage("start");
        }
        // ADR 0019：用戶變數只注入從儲存庫建置的服務
        let names: Vec<String> = env.vars.iter().filter(|v| v.user).map(|v| v.name.clone()).collect();
        let inject: Vec<(String, Vec<String>)> = built.into_iter().map(|s| (s, names.clone())).collect();
        match self.try_deploy(env, &compose, &inject).await {
            Ok(services) => {
                if env.source.is_some() {
                    self.prune_images(&env.id).await;
                }
                status(&env.id, "running", None, services)
            }
            Err(e) => {
                let msg = build::mask(&short(&e.to_string()), &secrets);
                tracing::warn!(env = %env.id, error = %msg, "deploy failed");
                failed_at(&env.id, env.source.as_ref().map(|_| "start"), msg)
            }
        }
    }

    /// fetch → build；回傳改寫成 image 的 compose
    async fn build_source(
        &self,
        env: &EnvSpec,
        src: &EnvSource,
        tx: Option<&UnboundedSender<AgentMsg>>,
        stage: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<(String, Vec<String>), (&'static str, anyhow::Error)> {
        self.build_source_with(env, src, tx, stage, None).await
    }

    /// `verify`：驗證 ID；log 改送 `VerifyLog`，原始碼保留給之後的階段（ADR 0020）
    pub(crate) async fn build_source_with(
        &self,
        env: &EnvSpec,
        src: &EnvSource,
        tx: Option<&UnboundedSender<AgentMsg>>,
        stage: &(dyn Fn(&str) + Send + Sync),
        verify: Option<&str>,
    ) -> Result<(String, Vec<String>), (&'static str, anyhow::Error)> {
        let work = self.builds_dir(&env.id);
        let repo = work.join("src");
        stage("fetch");
        let secrets: Vec<String> = env.vars.iter().map(|v| v.value.clone()).collect();
        let mut log = Log::new(tx, &env.id, "fetch").for_verify(verify).masking(&secrets);
        log.line(&format!(
            "取得 {} @ {}",
            src.repo_url,
            src.commit.chars().take(12).collect::<String>()
        ));
        self.fetch(src, &repo).await.map_err(|e| ("fetch", e))?;
        log.line("完成");

        stage("build");
        let (mut jobs, compose) = build::plan(&env.compose, &repo, src, &env.id).map_err(|e| ("build", e))?;
        let mut log = Log::new(tx, &env.id, "build").for_verify(verify).masking(&secrets);
        // ADR 0019：build args 的 `${NAME}` 以秘密變數內插
        let values: std::collections::HashMap<String, String> = env.vars.iter().map(|v| (v.name.clone(), v.value.clone())).collect();
        for j in &mut jobs {
            for (k, v) in &mut j.args {
                let (out, missing) = build::interpolate(v, &values);
                for m in missing {
                    log.line(&format!(
                        "⚠ `{}` 的 build arg `{k}` 用到未設定的變數 `{m}`，以空字串建置",
                        j.service
                    ));
                }
                *v = out;
            }
        }
        let built: Vec<String> = jobs.iter().map(|j| j.service.clone()).collect();
        let job = async {
            let resolv = work.join("resolv.conf");
            tokio::fs::write(&resolv, sandbox::resolv_conf(&self.dns)).await?;
            for j in &jobs {
                log.line(&format!("▶ 建置 {} → {}", j.service, j.tag));
                let out = work.join("out");
                let _ = tokio::fs::remove_dir_all(&out).await;
                tokio::fs::create_dir_all(&out).await?;
                set_mode(&out, 0o777).await;
                let (df_dir, df_name) = match (&j.inline, &j.dockerfile) {
                    (Some(text), _) => {
                        let d = work.join("inline").join(build::container_name(&env.id, &j.service));
                        tokio::fs::create_dir_all(&d).await?;
                        tokio::fs::write(d.join("Dockerfile"), text).await?;
                        (d, "Dockerfile".to_string())
                    }
                    (None, Some((d, f))) => (d.clone(), f.clone()),
                    (None, None) => anyhow::bail!("service `{}` 沒有 Dockerfile", j.service),
                };
                let args = build::build_command(j, &env.id, &self.buildkit_image, &resolv, &df_dir, &df_name, &out);
                let mut cmd = Command::new(&self.docker_bin);
                cmd.args(&args);
                build::run_logged(cmd, &mut log).await?;
                let tar = out.join("image.tar").display().to_string();
                let mut load = Command::new(&self.docker_bin);
                load.args(["load", "-i", &tar]);
                build::run_logged(load, &mut log).await?;
                let _ = tokio::fs::remove_dir_all(&out).await;
            }
            anyhow::Ok(())
        };
        let res = tokio::time::timeout(BUILD_TIMEOUT, job).await;
        // 逾時時 docker CLI 已被終止，建置容器要另外移除
        if res.is_err() {
            for j in &jobs {
                let _ = self
                    .docker(&["rm", "-f", &build::container_name(&env.id, &j.service)], Duration::from_secs(30))
                    .await;
            }
        }
        if verify.is_none() {
            let _ = tokio::fs::remove_dir_all(&work).await;
        } else {
            let _ = tokio::fs::remove_dir_all(work.join("out")).await;
            let _ = tokio::fs::remove_dir_all(work.join("inline")).await;
        }
        match res {
            Err(_) => Err(("build", anyhow::anyhow!("建置超過 20 分鐘，已中止。"))),
            Ok(Err(e)) => Err(("build", e)),
            Ok(Ok(())) => {
                if jobs.is_empty() {
                    log.line("沒有需要建置的服務");
                }
                Ok((compose, built))
            }
        }
    }

    pub(crate) async fn fetch(&self, src: &EnvSource, repo: &std::path::Path) -> anyhow::Result<()> {
        use caloud_orchestrator::git::{fetch_repo, fetch_repo_with_auth};
        let get = |r: Option<String>| {
            let r = r.clone();
            async move {
                match &src.auth_header {
                    Some(h) => fetch_repo_with_auth(&self.git_bin, repo, &src.repo_url, r.as_deref(), h).await,
                    None => fetch_repo(&self.git_bin, repo, &src.repo_url, r.as_deref()).await,
                }
            }
        };
        // 先以 commit 取得；不支援時改取分支再比對
        let fetched = match get(Some(src.commit.clone())).await {
            Ok(f) => f,
            Err(e) if src.git_ref.is_some() => {
                tracing::debug!(error = %e, "fetch by commit failed, trying ref");
                get(src.git_ref.clone()).await?
            }
            Err(e) => return Err(e),
        };
        if !fetched.commit.is_empty() && !src.commit.is_empty() && fetched.commit != src.commit {
            anyhow::bail!(
                "取得的版本 {} 與預期的 {} 不同，請重新部署。",
                fetched.commit.chars().take(12).collect::<String>(),
                src.commit.chars().take(12).collect::<String>()
            );
        }
        Ok(())
    }

    /// 每個服務只保留最近的映像（ADR 0018）
    async fn prune_images(&self, env_id: &str) {
        let filter = format!("reference={}{env_id}/*", build::IMAGE_PREFIX);
        let Ok(out) = self
            .docker(
                &["image", "ls", "--filter", &filter, "--format", "{{.Repository}}:{{.Tag}}"],
                Duration::from_secs(30),
            )
            .await
        else {
            return;
        };
        let mut seen: HashMap<String, usize> = HashMap::new();
        for img in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let repo = img.rsplit_once(':').map(|(r, _)| r).unwrap_or(img).to_string();
            let n = seen.entry(repo).or_default();
            *n += 1;
            if *n > build::KEEP_IMAGES {
                let _ = self.docker(&["rmi", img], Duration::from_secs(60)).await;
            }
        }
    }

    pub(crate) async fn try_deploy(
        &self,
        env: &EnvSpec,
        compose_yaml: &str,
        inject: &[(String, Vec<String>)],
    ) -> anyhow::Result<Vec<EnvService>> {
        // 控制面已檢查過；agent 再以同一份平臺政策檢查一次（docs/07）
        let compose = validate_with(compose_yaml, Policy::platform(0)).map_err(|e| anyhow::anyhow!("{e}"))?;
        if !compose.services.contains(&env.public_service) {
            anyhow::bail!("compose 中沒有服務 `{}`", env.public_service);
        }
        let net = EnvNet::new(env.subnet_index).ok_or_else(|| anyhow::anyhow!("網段索引超出範圍"))?;
        let overlay = sandbox::env_override_with(&env.id, &compose, &net, Limits::default(), inject).map_err(|e| anyhow::anyhow!(e))?;
        let dir = self.dir.join(&env.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("compose.yml"), compose_yaml).await?;
        tokio::fs::write(dir.join("caloud.override.yml"), overlay).await?;
        tokio::fs::write(dir.join("resolv.conf"), sandbox::resolv_conf(&self.dns)).await?;
        self.compose_env(&env.id, &["up", "-d", "--wait", "--remove-orphans"], DEPLOY_TIMEOUT, &env.vars)
            .await?;
        Ok(sandbox::service_ips(&compose, &net)
            .into_iter()
            .map(|(name, ip)| EnvService { name, ip: ip.to_string() })
            .collect())
    }

    /// 停止並刪除環境與其 volume
    pub async fn destroy(&self, env_id: &str) -> AgentMsg {
        if !valid_env_id(env_id) {
            return status(env_id, "failed", Some("環境 ID 格式錯誤。".into()), vec![]);
        }
        let lock = self.lock(env_id);
        let _guard = lock.lock().await;
        let dir = self.dir.join(env_id);
        let res = if tokio::fs::try_exists(dir.join("compose.yml")).await.unwrap_or(false) {
            self.compose(env_id, &["down", "-v", "--remove-orphans"], DEPLOY_TIMEOUT).await
        } else {
            let p = sandbox::project(env_id);
            self.docker(&["compose", "-p", &p, "down", "-v", "--remove-orphans"], DEPLOY_TIMEOUT)
                .await
        };
        match res {
            Ok(_) => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                let _ = tokio::fs::remove_dir_all(self.builds_dir(env_id)).await;
                // 平臺建置的映像與建置快取（ADR 0018）
                let filter = format!("reference={}{env_id}/*", build::IMAGE_PREFIX);
                if let Ok(ids) = self
                    .docker(&["image", "ls", "-q", "--filter", &filter], Duration::from_secs(30))
                    .await
                {
                    let mut args = vec!["rmi", "-f"];
                    args.extend(ids.split_whitespace());
                    if args.len() > 2 {
                        let _ = self.docker(&args, Duration::from_secs(120)).await;
                    }
                }
                let _ = self
                    .docker(
                        &["volume", "rm", "-f", &format!("caloud-bkcache-{env_id}")],
                        Duration::from_secs(60),
                    )
                    .await;
                status(env_id, "destroyed", None, vec![])
            }
            Err(e) => status(env_id, "failed", Some(short(&e.to_string())), vec![]),
        }
    }
}

async fn set_mode(p: &std::path::Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).await;
    }
    #[cfg(not(unix))]
    let _ = (p, mode);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 替身 docker：記錄參數；compose 的 up 在 compose.yml 含 fail 時失敗
    #[cfg(unix)]
    pub(crate) fn fake_docker(dir: &std::path::Path, extra: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("docker.sh");
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\n{extra}\ncase \"$*\" in *' up '*) f=$(echo \"$*\" | sed 's/.* -f \\([^ ]*compose.yml\\) .*/\\1/'); grep -q fail \"$f\" && {{ echo 'image not found' >&2; exit 1; }};; esac\nexit 0\n",
                log = dir.join("docker.log").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.display().to_string()
    }

    // FR-OPS-13、ADR 0015
    #[cfg(unix)]
    #[tokio::test]
    async fn deploys_and_destroys_environment() {
        let base = std::env::temp_dir().join(format!("caloud-envs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let r = EnvRunner::new(fake_docker(&base, ""), base.join("state"), vec!["1.1.1.1".into()]);
        let spec = |compose: &str, idx: u16| EnvSpec {
            id: "k2m9xq7a4tz1ab3c".into(),
            kind: "verify".into(),
            compose: compose.into(),
            subnet_index: idx,
            public_service: "web".into(),
            source: None,
            vars: vec![],
        };
        let ok = "services:\n  web:\n    image: nginx:1.27\n  db:\n    image: postgres:16\n";
        match r.deploy(&spec(ok, 3)).await {
            AgentMsg::EnvStatus {
                status, services, error, ..
            } => {
                assert_eq!(status, "running", "{error:?}");
                assert_eq!(
                    services[0],
                    EnvService {
                        name: "web".into(),
                        ip: "10.240.3.10".into()
                    }
                );
                assert_eq!(services[1].ip, "10.240.3.11");
            }
            m => panic!("{m:?}"),
        }
        let dir = base.join("state/envs/k2m9xq7a4tz1ab3c");
        assert!(std::fs::read_to_string(dir.join("caloud.override.yml"))
            .unwrap()
            .contains("runtime: runsc"));
        assert_eq!(
            std::fs::read_to_string(dir.join("resolv.conf")).unwrap(),
            "nameserver 1.1.1.1\noptions ndots:0\n"
        );
        let log = std::fs::read_to_string(base.join("docker.log")).unwrap();
        assert!(
            log.contains("compose -p caloud-env-k2m9xq7a4tz1ab3c") && log.contains("up -d --wait --remove-orphans"),
            "{log}"
        );

        // 拒絕：保留的探測索引、違反平臺政策、build、ID 格式
        for (compose, idx) in [
            (ok, 4094),
            ("services:\n  web:\n    image: x\n    privileged: true\n", 1),
            ("services:\n  web:\n    build: .\n", 1),
        ] {
            match r.deploy(&spec(compose, idx)).await {
                AgentMsg::EnvStatus { status, .. } => assert_eq!(status, "failed", "{compose}"),
                m => panic!("{m:?}"),
            }
        }
        let mut bad = spec(ok, 1);
        bad.id = "../etc".into();
        assert!(matches!(r.deploy(&bad).await, AgentMsg::EnvStatus { status, .. } if status == "failed"));
        // docker 失敗時回報錯誤
        match r.deploy(&spec("services:\n  web:\n    image: fail:1\n", 2)).await {
            AgentMsg::EnvStatus { status, error, .. } => {
                assert_eq!(status, "failed");
                assert!(error.unwrap().contains("image not found"));
            }
            m => panic!("{m:?}"),
        }

        match r.destroy("k2m9xq7a4tz1ab3c").await {
            AgentMsg::EnvStatus { status, .. } => assert_eq!(status, "destroyed"),
            m => panic!("{m:?}"),
        }
        assert!(!dir.exists());
        assert!(std::fs::read_to_string(base.join("docker.log"))
            .unwrap()
            .contains("down -v --remove-orphans"));
        let _ = std::fs::remove_dir_all(&base);
    }

    // FR-ENV-12、ADR 0018：fetch → build（gVisor BuildKit）→ start，回報階段與 log
    #[cfg(unix)]
    #[tokio::test]
    async fn deploys_from_source_with_stages_and_logs() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("caloud-envs-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("app")).unwrap();
        std::fs::write(repo.join("app/Dockerfile"), "FROM alpine\n").unwrap();
        let git = base.join("git.sh");
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\necho \"$* [auth=${{GIT_CONFIG_VALUE_0:-none}}]\" >> '{log}'\ncase \"$*\" in *' checkout '*) cp -R '{repo}/.' \"$2\";; *' rev-parse '*) echo 98b705d0123456789abcdef0123456789abcdef0;; esac\nexit 0\n",
                log = base.join("git.log").display(),
                repo = repo.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        // 建置失敗：buildctl 輸出含 boom
        let docker = fake_docker(
            &base,
            "case \"$*\" in *buildctl-daemonless*) echo '#5 RUN make'; echo \"#6 token=$SESSION_SECRET arg=$(echo \"$*\" | grep -o 'build-arg:A=[^ ]*')\"; case \"$*\" in *fail-me*) echo 'boom' >&2; exit 1;; esac;; esac\ncase \"$*\" in *' up '*) echo \"UPENV S=$SESSION_SECRET D=$CALOUD_DB_PASSWORD\" >> '{log}';; esac".replace("{log}", &base.join("docker.log").display().to_string()).as_str(),
        );
        let mut r = EnvRunner::new(docker, base.join("state"), vec!["1.1.1.1".into()]);
        r.git_bin = git.display().to_string();
        let mut spec = EnvSpec {
            id: "k2m9xq7a".into(),
            kind: "production".into(),
            compose: "services:\n  web:\n    build: {context: ./app, args: {A: '${NEXT_PUBLIC_X}', B: '${NOPE}'}}\n  db:\n    image: postgres:16\n    environment: {POSTGRES_PASSWORD: '${CALOUD_DB_PASSWORD}'}\n".into(),
            subnet_index: 5,
            public_service: "web".into(),
            vars: vec![
                EnvValue {
                    name: "SESSION_SECRET".into(),
                    value: "s3cr3t-value-123".into(),
                    user: true,
                },
                EnvValue {
                    name: "NEXT_PUBLIC_X".into(),
                    value: "https://x.example".into(),
                    user: true,
                },
                EnvValue {
                    name: "CALOUD_DB_PASSWORD".into(),
                    value: "dbpass-0000-1111".into(),
                    user: false,
                },
            ],
            source: Some(EnvSource {
                repo_url: "https://github.com/a/b".into(),
                commit: "98b705d0123456789abcdef0123456789abcdef0".into(),
                git_ref: Some("main".into()),
                compose_dir: String::new(),
                auth_header: Some("AUTHORIZATION: basic c2VjcmV0".into()),
            }),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let res = r.deploy_with(&spec, Some(&tx)).await;
        assert!(matches!(&res, AgentMsg::EnvStatus { status, .. } if status == "running"), "{res:?}");
        let mut stages = Vec::new();
        let mut logs = Vec::new();
        while let Ok(m) = rx.try_recv() {
            match m {
                AgentMsg::EnvStatus { stage: Some(s), .. } => stages.push(s),
                AgentMsg::EnvLog { stage, line, .. } => logs.push(format!("{stage}: {line}")),
                m => panic!("{m:?}"),
            }
        }
        assert_eq!(stages, ["fetch", "build", "start"]);
        assert!(logs.iter().any(|l| l == "build: #5 RUN make"), "{logs:?}");
        let compose = std::fs::read_to_string(base.join("state/envs/k2m9xq7a/compose.yml")).unwrap();
        assert!(compose.contains("image: caloud-env/k2m9xq7a/web:98b705d01234") && !compose.contains("build"));
        let log = std::fs::read_to_string(base.join("docker.log")).unwrap();
        assert!(
            log.contains("run --rm --name caloud-build-k2m9xq7a-web --runtime runsc --network caloud-build"),
            "{log}"
        );
        assert!(log.contains("load -i"));
        // ADR 0019：build arg 內插；值經行程環境給 compose；log 遮蔽；override 只列名稱
        assert!(log.contains("--opt build-arg:A=https://x.example --opt build-arg:B="), "{log}");
        assert!(logs.iter().any(|l| l.contains("未設定的變數 `NOPE`")), "{logs:?}");
        assert!(logs.iter().any(|l| l.contains("token= arg=build-arg:A=***")), "{logs:?}");
        assert!(!logs.iter().any(|l| l.contains("s3cr3t")));
        assert!(log.contains("UPENV S=s3cr3t-value-123 D=dbpass-0000-1111"), "{log}");
        let ov: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(base.join("state/envs/k2m9xq7a/caloud.override.yml")).unwrap()).unwrap();
        assert_eq!(
            ov["services"]["web"]["environment"],
            serde_yaml::from_str::<serde_yaml::Value>("[SESSION_SECRET, NEXT_PUBLIC_X]").unwrap()
        );
        assert!(ov["services"]["db"].get("environment").is_none());
        for f in ["compose.yml", "caloud.override.yml"] {
            let t = std::fs::read_to_string(base.join("state/envs/k2m9xq7a").join(f)).unwrap();
            assert!(!t.contains("s3cr3t") && !t.contains("dbpass"), "{f}");
        }
        assert!(!format!("{spec:?}").contains("s3cr3t"));
        // 權杖只經環境變數給 git，不在參數中
        let glog = std::fs::read_to_string(base.join("git.log")).unwrap();
        assert!(glog
            .contains("fetch -q --depth 1 --no-tags https://github.com/a/b 98b705d0123456789abcdef0123456789abcdef0 [auth=AUTHORIZATION"));
        assert!(!glog.lines().any(|l| l.split(" [auth=").next().unwrap().contains("c2VjcmV0")));
        assert!(!base.join("state/builds/k2m9xq7a").exists(), "原始碼建置後清除");
        assert!(!format!("{:?}", spec.source).contains("c2VjcmV0"));

        // 建置失敗：回報失敗階段與最後幾行 log，不動到 compose
        spec.compose = "services:\n  web:\n    build: {context: ./app, args: {X: fail-me}}\n".into();
        match r.deploy_with(&spec, None).await {
            AgentMsg::EnvStatus { status, stage, error, .. } => {
                assert_eq!((status.as_str(), stage.as_deref()), ("failed", Some("build")));
                assert!(error.unwrap().contains("boom"));
            }
            m => panic!("{m:?}"),
        }
        // 版本不符
        spec.source.as_mut().unwrap().commit = "1111111111111111111111111111111111111111".into();
        match r.deploy_with(&spec, None).await {
            AgentMsg::EnvStatus { stage, error, .. } => {
                assert_eq!(stage.as_deref(), Some("fetch"));
                assert!(error.unwrap().contains("不同"));
            }
            m => panic!("{m:?}"),
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
