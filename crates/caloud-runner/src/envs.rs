//! 在 Runner 上部署與刪除環境（FR-OPS-13，ADR 0015）。
//!
//! 每個環境的檔案在 `<state>/envs/<id>/`：`compose.yml`、`caloud.override.yml`、`resolv.conf`；
//! 以 `docker compose -p caloud-env-<id>` 執行，容器跑在 gVisor 與環境自己的網段。
use caloud_core::agent::{valid_env_id, AgentMsg, EnvService, EnvSpec};
use caloud_orchestrator::sandbox::{self, EnvNet, Limits};
use caloud_orchestrator::{validate_with, Policy};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;

/// 一次部署的逾時
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(600);
/// 預設外部 DNS（ADR 0015）
pub const DEFAULT_DNS: [&str; 2] = ["1.1.1.1", "8.8.8.8"];

#[derive(Clone)]
pub struct EnvRunner {
    pub docker_bin: String,
    pub dir: PathBuf,
    pub dns: Vec<String>,
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
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
        error,
        services,
    }
}

/// 錯誤訊息只取最後幾行，避免過長
fn short(err: &str) -> String {
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
            locks: Default::default(),
        }
    }

    fn lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    pub(crate) async fn docker(&self, args: &[&str], timeout: Duration) -> anyhow::Result<String> {
        let mut c = Command::new(&self.docker_bin);
        c.args(args).stdin(Stdio::null()).kill_on_drop(true);
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
        self.docker(&full, timeout).await
    }

    /// 由控制面送來的部署；索引不可使用自我檢查保留的範圍
    pub async fn deploy(&self, env: &EnvSpec) -> AgentMsg {
        if env.subnet_index > sandbox::MAX_ALLOC_INDEX {
            return status(&env.id, "failed", Some("網段索引超出範圍。".into()), vec![]);
        }
        self.deploy_unchecked(env).await
    }

    pub(crate) async fn deploy_unchecked(&self, env: &EnvSpec) -> AgentMsg {
        if !valid_env_id(&env.id) {
            return status(&env.id, "failed", Some("環境 ID 格式錯誤。".into()), vec![]);
        }
        let lock = self.lock(&env.id);
        let _guard = lock.lock().await;
        match self.try_deploy(env).await {
            Ok(services) => status(&env.id, "running", None, services),
            Err(e) => {
                tracing::warn!(env = %env.id, error = %e, "deploy failed");
                status(&env.id, "failed", Some(short(&e.to_string())), vec![])
            }
        }
    }

    async fn try_deploy(&self, env: &EnvSpec) -> anyhow::Result<Vec<EnvService>> {
        // 控制面已檢查過；agent 再以同一份平臺政策檢查一次（docs/07）
        let compose = validate_with(&env.compose, Policy::platform(0)).map_err(|e| anyhow::anyhow!("{e}"))?;
        if !compose.services.contains(&env.public_service) {
            anyhow::bail!("compose 中沒有服務 `{}`", env.public_service);
        }
        let net = EnvNet::new(env.subnet_index).ok_or_else(|| anyhow::anyhow!("網段索引超出範圍"))?;
        let overlay = sandbox::env_override(&env.id, &compose, &net, Limits::default()).map_err(|e| anyhow::anyhow!(e))?;
        let dir = self.dir.join(&env.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("compose.yml"), &env.compose).await?;
        tokio::fs::write(dir.join("caloud.override.yml"), overlay).await?;
        tokio::fs::write(dir.join("resolv.conf"), sandbox::resolv_conf(&self.dns)).await?;
        self.compose(&env.id, &["up", "-d", "--wait", "--remove-orphans"], DEPLOY_TIMEOUT)
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
                status(env_id, "destroyed", None, vec![])
            }
            Err(e) => status(env_id, "failed", Some(short(&e.to_string())), vec![]),
        }
    }
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
}
