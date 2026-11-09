use crate::policy::ValidatedCompose;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// 以 `docker compose` CLI 管理每個 App（一個 App = 一個 compose project）。
///
/// 選擇 CLI 而非直接呼叫 Docker API，是為了完整相容 compose 規格（depends_on、healthcheck、profiles…）；
/// 狀態查詢走 `docker compose ps --format json`。
#[derive(Clone)]
pub struct ComposeRunner {
    /// 存放每個 App compose 檔的目錄：`<data_dir>/apps/<app_id>/`
    pub apps_dir: PathBuf,
    /// 被代理的容器加入的共用網路（Caloud 自己也在這個網路上）
    pub edge_network: String,
    /// docker CLI 路徑（`CALOUD_DOCKER_BIN`）
    pub docker_bin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    #[serde(rename = "Service")]
    pub service: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "Health", default)]
    pub health: String,
}

impl ComposeRunner {
    pub fn new(apps_dir: PathBuf, edge_network: String, docker_bin: String) -> Self {
        Self {
            apps_dir,
            edge_network,
            docker_bin,
        }
    }

    /// 確保 edge 網路存在（啟動時呼叫一次）
    pub async fn ensure_edge_network(&self) -> anyhow::Result<()> {
        let ok = Command::new(&self.docker_bin)
            .args(["network", "inspect", &self.edge_network])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?
            .success();
        if !ok {
            run(Command::new(&self.docker_bin).args(["network", "create", &self.edge_network])).await?;
        }
        Ok(())
    }

    /// 寫入 compose 檔與 override，然後 `up -d`。重複呼叫即為重新部署。
    pub async fn deploy(&self, app_id: &str, compose: &ValidatedCompose) -> anyhow::Result<String> {
        let project = caloud_core::project_name(app_id);
        let dir = self.apps_dir.join(app_id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("compose.yml"), serde_yaml::to_string(&compose.doc)?).await?;
        tokio::fs::write(dir.join("caloud.override.yml"), self.override_yaml(app_id, compose)?).await?;
        self.compose(app_id, &["pull", "--quiet"]).await.ok(); // 拉取失敗留給 up 回報
        self.compose(app_id, &["up", "-d", "--remove-orphans", "--wait", "--wait-timeout", "180"])
            .await?;
        Ok(project)
    }

    pub async fn stop(&self, app_id: &str) -> anyhow::Result<()> {
        self.compose(app_id, &["stop"]).await.map(|_| ())
    }

    /// 移除容器與網路；`purge_volumes` 為 true 時一併刪除資料
    pub async fn remove(&self, app_id: &str, purge_volumes: bool) -> anyhow::Result<()> {
        let mut args = vec!["down", "--remove-orphans"];
        if purge_volumes {
            args.push("--volumes");
        }
        self.compose(app_id, &args).await?;
        let _ = tokio::fs::remove_dir_all(self.apps_dir.join(app_id)).await;
        Ok(())
    }

    pub async fn status(&self, app_id: &str) -> anyhow::Result<Vec<ServiceStatus>> {
        let out = self.compose(app_id, &["ps", "--all", "--format", "json"]).await?;
        // 新版輸出 NDJSON，舊版輸出 JSON 陣列，兩者都接受
        if let Ok(v) = serde_json::from_str::<Vec<ServiceStatus>>(&out) {
            return Ok(v);
        }
        Ok(out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }

    pub async fn logs(&self, app_id: &str, service: Option<&str>, tail: u32) -> anyhow::Result<String> {
        let tail = tail.to_string();
        let mut args = vec!["logs", "--no-color", "--timestamps", "--tail", &tail];
        if let Some(s) = service {
            args.push(s);
        }
        self.compose(app_id, &args).await
    }

    /// FR-APP-07：每個 service 都接上 edge 網路，並以 `<project>-<service>` 作為 DNS alias，
    /// 反向代理即可用 `http://<project>-<service>:<port>` 連線，不需要對主機開 port。
    fn override_yaml(&self, app_id: &str, compose: &ValidatedCompose) -> anyhow::Result<String> {
        let project = caloud_core::project_name(app_id);
        let mut services = Mapping::new();
        for s in &compose.services {
            let mut edge = Mapping::new();
            edge.insert(
                "aliases".into(),
                Value::Sequence(vec![caloud_core::model_upstream_host(app_id, s).into()]),
            );
            let mut nets = Mapping::new();
            nets.insert("default".into(), Value::Mapping(Mapping::new()));
            nets.insert(self.edge_network.clone().into(), Value::Mapping(edge));
            let mut labels = Mapping::new();
            labels.insert("io.caloud.app".into(), app_id.into());
            labels.insert("io.caloud.project".into(), project.clone().into());
            let mut svc = Mapping::new();
            svc.insert("networks".into(), Value::Mapping(nets));
            svc.insert("labels".into(), Value::Mapping(labels));
            // 預設資源上限，避免單一 App 拖垮主機（之後可改為依使用者配額）
            svc.insert("mem_limit".into(), "1g".into());
            svc.insert("pids_limit".into(), 512.into());
            svc.insert("restart".into(), "unless-stopped".into());
            services.insert(s.clone().into(), Value::Mapping(svc));
        }
        let mut edge_def = Mapping::new();
        edge_def.insert("external".into(), true.into());
        edge_def.insert("name".into(), self.edge_network.clone().into());
        let mut networks = Mapping::new();
        networks.insert(self.edge_network.clone().into(), Value::Mapping(edge_def));
        let mut root = Mapping::new();
        root.insert("services".into(), Value::Mapping(services));
        root.insert("networks".into(), Value::Mapping(networks));
        Ok(serde_yaml::to_string(&Value::Mapping(root))?)
    }

    async fn compose(&self, app_id: &str, args: &[&str]) -> anyhow::Result<String> {
        let dir = self.apps_dir.join(app_id);
        let project = caloud_core::project_name(app_id);
        let mut cmd = Command::new(&self.docker_bin);
        cmd.arg("compose")
            .args(["-p", &project, "-f"])
            .arg(dir.join("compose.yml"))
            .arg("-f")
            .arg(dir.join("caloud.override.yml"))
            .args(args)
            .current_dir(&dir);
        run(&mut cmd).await
    }
}

async fn run(cmd: &mut Command) -> anyhow::Result<String> {
    let out = cmd.stdin(Stdio::null()).output().await?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
