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

/// 發布前任務的逾時（ADR 0021）
const RELEASE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
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
    /// 建置與沙箱的累計秒數（ADR 0021）：環境 ID → 秒
    usage: Arc<Mutex<HashMap<String, u64>>>,
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

/// 注入建置服務 `environment` 的變數名稱：用戶變數（ADR 0019）與平臺變數（ADR 0023）
pub(crate) fn inject_names(env: &EnvSpec) -> Vec<String> {
    env.vars
        .iter()
        .filter(|v| v.user)
        .map(|v| v.name.clone())
        .chain(env.platform_vars.iter().map(|v| v.name.clone()))
        .collect()
}

/// `docker compose` 的行程環境變數：秘密變數與平臺變數
pub(crate) fn proc_env(env: &EnvSpec) -> Vec<EnvValue> {
    env.vars
        .iter()
        .cloned()
        .chain(env.platform_vars.iter().map(|v| EnvValue {
            name: v.name.clone(),
            value: v.value.clone(),
            user: false,
        }))
        .collect()
}

/// `docker compose ps --format json`（每行一個 JSON，或一個陣列）→ 服務 → 映像
pub(crate) fn container_images(out: &str) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    let mut add = |v: &serde_json::Value| {
        if let (Some(s), Some(i)) = (v["Service"].as_str(), v["Image"].as_str()) {
            m.insert(s.to_string(), i.to_string());
        }
    };
    for line in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(serde_json::Value::Array(list)) => list.iter().for_each(&mut add),
            Ok(v) => add(&v),
            Err(_) => {}
        }
    }
    m
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
            usage: Default::default(),
        }
    }

    /// 記錄建置與沙箱的時間
    pub(crate) fn add_usage(&self, env_id: &str, d: Duration) {
        *self
            .usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(env_id.to_string())
            .or_default() += d.as_secs().max(1);
    }

    /// 取出並清除累計秒數
    pub(crate) fn take_usage(&self, env_id: &str) -> u64 {
        self.usage.lock().unwrap_or_else(|e| e.into_inner()).remove(env_id).unwrap_or(0)
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
        self.compose_files(env_id, false, args, timeout, vars).await
    }

    /// `next`：使用 `compose.next.yml`／`caloud.override.next.yml`（發布前任務，ADR 0021）
    fn files(&self, env_id: &str, next: bool) -> (String, String) {
        let dir = self.dir.join(env_id);
        let sfx = if next { ".next" } else { "" };
        (
            dir.join(format!("compose{sfx}.yml")).display().to_string(),
            dir.join(format!("caloud.override{sfx}.yml")).display().to_string(),
        )
    }

    async fn compose_files(&self, env_id: &str, next: bool, args: &[&str], timeout: Duration, vars: &[EnvValue]) -> anyhow::Result<String> {
        let dir = self.dir.join(env_id);
        let project = sandbox::project(env_id);
        let d = dir.display().to_string();
        let (compose_file, override_file) = self.files(env_id, next);
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
                    self.send_usage(tx, "deploy", &env.id, &env.id);
                    return failed_at(&env.id, Some(st), build::mask(&short(&e.to_string()), &secrets));
                }
            },
        };
        // ADR 0019：用戶變數只注入從儲存庫建置的服務；ADR 0023：平臺變數也一樣
        let names = inject_names(env);
        let inject: Vec<(String, Vec<String>)> = built.into_iter().map(|s| (s, names.clone())).collect();
        // ADR 0021：發布前任務以新設定執行，成功才切換；失敗時舊版繼續執行
        if env.source.is_some() && !env.release.is_empty() {
            stage("release");
            let mut log = Log::new(tx, &env.id, "release").masking(&secrets);
            let res = async {
                self.prepare(env, &compose, &inject, true).await?;
                self.run_release(env, true, &mut log).await
            }
            .await;
            // 成功後由 try_deploy 寫入正式檔；next 檔不再需要
            let (c, o) = self.files(&env.id, true);
            let _ = tokio::fs::remove_file(c).await;
            let _ = tokio::fs::remove_file(o).await;
            if let Err(e) = res {
                self.send_usage(tx, "deploy", &env.id, &env.id);
                return failed_at(&env.id, Some("release"), build::mask(&short(&e.to_string()), &secrets));
            }
        }
        if env.source.is_some() {
            stage("start");
        }
        self.send_usage(tx, "deploy", &env.id, &env.id);
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
        let keep: Vec<String> = env.release.iter().map(|r| r.service.clone()).collect();
        let (mut jobs, compose) = build::plan(&env.compose, &repo, src, &env.id, &keep).map_err(|e| ("build", e))?;
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
            // ADR 0023：平臺變數作為 build arg（Dockerfile 沒有宣告 ARG 時不影響建置）
            for pv in &env.platform_vars {
                if !j.args.iter().any(|(k, _)| k == &pv.name) {
                    j.args.push((pv.name.clone(), pv.value.clone()));
                }
            }
        }
        let built: Vec<String> = jobs.iter().map(|j| j.service.clone()).collect();
        let limits = env.build.unwrap_or_default();
        let started = std::time::Instant::now();
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
                let args = build::build_command(limits, j, &env.id, &self.buildkit_image, &resolv, &df_dir, &df_name, &out);
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
        self.add_usage(&env.id, started.elapsed());
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
        let mut services = self.prepare(env, compose_yaml, inject, false).await?;
        self.compose_env(&env.id, &["up", "-d", "--wait", "--remove-orphans"], DEPLOY_TIMEOUT, &proc_env(env))
            .await?;
        // ADR 0023：容器實際的映像
        if let Ok(out) = self
            .compose(&env.id, &["ps", "-a", "--format", "json"], Duration::from_secs(30))
            .await
        {
            let images = container_images(&out);
            for s in &mut services {
                s.image = images.get(&s.name).cloned();
            }
        }
        Ok(services)
    }

    /// 送出建置用量（ADR 0021）
    pub(crate) fn send_usage(&self, tx: Option<&UnboundedSender<AgentMsg>>, kind: &str, env_id: &str, id: &str) {
        let secs = self.take_usage(env_id);
        if let (Some(tx), true) = (tx, secs > 0) {
            let _ = tx.send(AgentMsg::BuildUsage {
                kind: kind.into(),
                id: id.into(),
                secs,
            });
        }
    }

    /// 發布前任務：`docker compose run --rm -T <service> [sh -c <command>]`，逾時 10 分鐘（ADR 0021）
    pub(crate) async fn run_release(&self, env: &EnvSpec, next: bool, log: &mut Log<'_>) -> anyhow::Result<()> {
        let dir = self.dir.join(&env.id);
        let project = sandbox::project(&env.id);
        let (c, o) = self.files(&env.id, next);
        for t in &env.release {
            log.line(&format!(
                "▶ {}{}",
                t.service,
                t.command.as_deref().map(|c| format!("：{c}")).unwrap_or_default()
            ));
            let mut cmd = Command::new(&self.docker_bin);
            cmd.args(["compose", "-p", &project, "--project-directory"])
                .arg(&dir)
                .args(["-f", &c, "-f", &o, "run", "--rm", "-T", &t.service]);
            if let Some(command) = &t.command {
                cmd.args(["sh", "-c", command]);
            }
            for v in proc_env(env) {
                cmd.env(&v.name, &v.value);
            }
            tokio::time::timeout(RELEASE_TIMEOUT, build::run_logged(cmd, log))
                .await
                .map_err(|_| anyhow::anyhow!("發布前任務 `{}` 超過 10 分鐘，已中止。", t.service))?
                .map_err(|e| anyhow::anyhow!("發布前任務 `{}` 失敗：{e}", t.service))?;
        }
        Ok(())
    }

    /// 檢查並寫入 compose、override、resolv.conf；`next` 時寫到 `*.next.yml`
    async fn prepare(
        &self,
        env: &EnvSpec,
        compose_yaml: &str,
        inject: &[(String, Vec<String>)],
        next: bool,
    ) -> anyhow::Result<Vec<EnvService>> {
        // 控制面已檢查過；agent 再以同一份平臺政策檢查一次（docs/07）
        let compose = validate_with(compose_yaml, Policy::platform(0)).map_err(|e| anyhow::anyhow!("{e}"))?;
        if !compose.services.contains(&env.public_service) {
            anyhow::bail!("compose 中沒有服務 `{}`", env.public_service);
        }
        let net = EnvNet::new(env.subnet_index).ok_or_else(|| anyhow::anyhow!("網段索引超出範圍"))?;
        // ADR 0025：控制面依專案上限決定的各服務資源
        let per: Vec<(String, Limits)> = env
            .limits
            .iter()
            .map(|l| {
                (
                    l.service.clone(),
                    Limits {
                        memory_mb: l.memory_mb.max(64),
                        cpus: (l.cpu_millis.max(100) as f64) / 1000.0,
                        ..Limits::default()
                    },
                )
            })
            .collect();
        let overlay =
            sandbox::env_override_limits(&env.id, &compose, &net, Limits::default(), &per, inject).map_err(|e| anyhow::anyhow!(e))?;
        let dir = self.dir.join(&env.id);
        tokio::fs::create_dir_all(&dir).await?;
        let (c, o) = self.files(&env.id, next);
        tokio::fs::write(&c, compose_yaml).await?;
        tokio::fs::write(&o, overlay).await?;
        tokio::fs::write(dir.join("resolv.conf"), sandbox::resolv_conf(&self.dns)).await?;
        Ok(sandbox::service_ips(&compose, &net)
            .into_iter()
            .map(|(name, ip)| EnvService {
                name,
                ip: ip.to_string(),
                image: None,
            })
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

    // ADR 0023：`docker compose ps --format json` 的兩種格式
    #[test]
    fn parses_container_images() {
        let lines = "{\"Service\":\"web\",\"Image\":\"caloud-env/k1/web:98b705d01234\"}\n{\"Service\":\"db\",\"Image\":\"postgres:16\"}\nnot json\n";
        let m = container_images(lines);
        assert_eq!(m["web"], "caloud-env/k1/web:98b705d01234");
        assert_eq!(m["db"], "postgres:16");
        let arr = container_images("[{\"Service\":\"web\",\"Image\":\"x:1\"},{\"Service\":\"api\"}]");
        assert_eq!((arr.len(), arr["web"].as_str()), (1, "x:1"));
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
            build: None,
            release: vec![],
            platform_vars: vec![],
            limits: vec![],
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
                        ip: "10.240.3.10".into(),
                        image: None
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
            "case \"$*\" in *buildctl-daemonless*) echo '#5 RUN make'; echo \"#6 token=$SESSION_SECRET arg=$(echo \"$*\" | grep -o 'build-arg:A=[^ ]*')\"; case \"$*\" in *fail-me*) echo 'boom' >&2; exit 1;; esac;; esac\ncase \"$*\" in *' up '*) echo \"UPENV S=$SESSION_SECRET D=$CALOUD_DB_PASSWORD\" >> '{log}'; echo \"PLATFORM C=$CALOUD_COMMIT\" >> '{log}';; esac".replace("{log}", &base.join("docker.log").display().to_string()).as_str(),
        );
        let mut r = EnvRunner::new(docker, base.join("state"), vec!["1.1.1.1".into()]);
        r.git_bin = git.display().to_string();
        let mut spec = EnvSpec {
            id: "k2m9xq7a".into(),
            kind: "production".into(),
            compose: "services:\n  web:\n    build: {context: ./app, args: {A: '${NEXT_PUBLIC_X}', B: '${NOPE}'}}\n  db:\n    image: postgres:16\n    environment: {POSTGRES_PASSWORD: '${CALOUD_DB_PASSWORD}'}\n".into(),
            subnet_index: 5,
            public_service: "web".into(),
            build: None,
            release: vec![],
            limits: vec![],
            platform_vars: vec![caloud_core::agent::PlatformVar {
                name: "CALOUD_COMMIT".into(),
                value: "98b705d0123456789abcdef0123456789abcdef0".into(),
            }],
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
        let mut usage = 0;
        while let Ok(m) = rx.try_recv() {
            match m {
                AgentMsg::EnvStatus { stage: Some(s), .. } => stages.push(s),
                AgentMsg::EnvLog { stage, line, .. } => logs.push(format!("{stage}: {line}")),
                AgentMsg::BuildUsage { kind, id, secs } => {
                    assert_eq!((kind.as_str(), id.as_str()), ("deploy", "k2m9xq7a"));
                    usage += secs;
                }
                m => panic!("{m:?}"),
            }
        }
        assert!(usage >= 1, "ADR 0021：回報建置秒數");
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
            serde_yaml::from_str::<serde_yaml::Value>("[SESSION_SECRET, NEXT_PUBLIC_X, CALOUD_COMMIT]").unwrap()
        );
        // ADR 0023：平臺變數作為 build arg 與執行期變數，不遮蔽
        assert!(
            log.contains("--opt build-arg:CALOUD_COMMIT=98b705d0123456789abcdef0123456789abcdef0"),
            "{log}"
        );
        assert!(log.contains("PLATFORM C=98b705d0123456789abcdef0123456789abcdef0"), "{log}");
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

    // ADR 0021：發布前任務以 next 設定執行，成功才 up；失敗時不動到正在執行的版本
    #[cfg(unix)]
    #[tokio::test]
    async fn release_runs_before_switch() {
        use caloud_core::agent::ReleaseTask;
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("caloud-envs-rel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Dockerfile"), "FROM alpine\n").unwrap();
        let git = base.join("git.sh");
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\ncase \"$*\" in *' checkout '*) cp -R '{repo}/.' \"$2\";; *' rev-parse '*) echo 98b705d0123456789abcdef0123456789abcdef0;; esac\nexit 0\n",
                repo = repo.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let docker = fake_docker(
            &base,
            &format!(
                "case \"$*\" in *' run --rm -T migrate'*) echo \"migrating S=$S\"; [ -f '{f}' ] && {{ echo 'migration failed' >&2; exit 1; }};; esac",
                f = base.join("REL_FAIL").display()
            ),
        );
        let mut r = EnvRunner::new(docker, base.join("state"), vec!["1.1.1.1".into()]);
        r.git_bin = git.display().to_string();
        let spec = EnvSpec {
            id: "r1r2r3r4".into(),
            kind: "production".into(),
            compose: "services:\n  web:\n    build: .\n  migrate:\n    build: .\n    profiles: [tools]\n  debug:\n    image: busybox\n    profiles: [debug]\n".into(),
            subnet_index: 9,
            public_service: "web".into(),
            source: Some(EnvSource {
                repo_url: "https://github.com/a/b".into(),
                commit: "98b705d0123456789abcdef0123456789abcdef0".into(),
                git_ref: None,
                compose_dir: String::new(),
                auth_header: None,
            }),
            build: Some(caloud_core::agent::BuildLimits { cpus: 2, memory_mb: 3072 }),
            release: vec![ReleaseTask { service: "migrate".into(), command: None }],
            platform_vars: vec![],
            limits: vec![],
            vars: vec![EnvValue {
                name: "S".into(),
                value: "secret-value".into(),
                user: true,
            }],
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let res = r.deploy_with(&spec, Some(&tx)).await;
        assert!(matches!(&res, AgentMsg::EnvStatus { status, .. } if status == "running"), "{res:?}");
        let log = std::fs::read_to_string(base.join("docker.log")).unwrap();
        let run_at = log.find("run --rm -T migrate").expect("release run");
        let up_at = log.find(" up -d --wait").expect("up");
        assert!(run_at < up_at, "先執行任務再切換：{log}");
        assert!(log.contains("compose.next.yml") && log.contains("--cpus 2 --memory 3072m"), "{log}");
        let compose = std::fs::read_to_string(base.join("state/envs/r1r2r3r4/compose.yml")).unwrap();
        assert!(
            compose.contains("migrate:") && !compose.contains("debug:"),
            "release 服務保留、其他 profiles 移除：{compose}"
        );
        assert!(!base.join("state/envs/r1r2r3r4/compose.next.yml").exists(), "next 檔已換成正式檔");
        let mut stages = vec![];
        let mut lines = vec![];
        while let Ok(m) = rx.try_recv() {
            match m {
                AgentMsg::EnvStatus { stage: Some(s), .. } => stages.push(s),
                AgentMsg::EnvLog { line, .. } => lines.push(line),
                _ => {}
            }
        }
        assert_eq!(stages, ["fetch", "build", "release", "start"]);
        assert!(lines.iter().any(|l| l == "migrating S=***"), "{lines:?}");

        // 失敗：不 up、回報 release、正式檔不變
        std::fs::write(base.join("REL_FAIL"), "").unwrap();
        std::fs::write(base.join("docker.log"), "").unwrap();
        match r.deploy_with(&spec, None).await {
            AgentMsg::EnvStatus { status, stage, error, .. } => {
                assert_eq!((status.as_str(), stage.as_deref()), ("failed", Some("release")));
                assert!(error.unwrap().contains("migration failed"));
            }
            m => panic!("{m:?}"),
        }
        let log = std::fs::read_to_string(base.join("docker.log")).unwrap();
        assert!(!log.contains(" up -d"), "失敗時不切換：{log}");
        assert!(!base.join("state/envs/r1r2r3r4/compose.next.yml").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
