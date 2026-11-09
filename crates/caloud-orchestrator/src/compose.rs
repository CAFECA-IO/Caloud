use crate::policy::ValidatedCompose;
use serde::{Deserialize, Serialize};
use serde_yaml::value::{Tag, TaggedValue};
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// 以 `docker compose` CLI 管理每個 App（一個 App = 一個 compose project，ADR 0001）。
///
/// 目錄配置：`<data_dir>/apps/<app_id>/`
/// - 上傳的 compose：`compose.yml`、`.env`、`caloud.override.yml`
/// - Git 來源（ADR 0009）：`src/`（儲存庫）、`src/<path>/.env`、`caloud.override.yml`
#[derive(Clone)]
pub struct ComposeRunner {
    pub apps_dir: PathBuf,
    /// 被代理的容器加入的共用網路（Caloud 自己也在這個網路上）
    pub edge_network: String,
    /// docker CLI 路徑（`CALOUD_DOCKER_BIN`）
    pub docker_bin: String,
    /// git CLI 路徑（`CALOUD_GIT_BIN`）
    pub git_bin: String,
}

/// Git 來源（FR-APP-13）
#[derive(Debug, Clone)]
pub struct GitSource {
    pub url: String,
    pub git_ref: String,
    /// compose 所在目錄（相對儲存庫根目錄，"" 表示根目錄）
    pub path: String,
    pub compose_file: String,
}

/// 一次部署需要的設定
#[derive(Debug, Clone, Copy)]
pub struct DeploySpec<'a> {
    pub compose: &'a ValidatedCompose,
    pub git: Option<&'a GitSource>,
    /// `.env` 內容（FR-APP-14）
    pub env: Option<&'a str>,
    /// 只啟動這些 service（FR-APP-15）
    pub services: Option<&'a [String]>,
    /// 特權 App（FR-APP-16）
    pub privileged: bool,
    /// Caloud 保留的主機連接埠，會從特權 App 的 `ports` 移除
    pub reserved_ports: &'a [u16],
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
    pub fn new(apps_dir: PathBuf, edge_network: String, docker_bin: String, git_bin: String) -> Self {
        Self {
            apps_dir,
            edge_network,
            docker_bin,
            git_bin,
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

    fn app_dir(&self, app_id: &str) -> PathBuf {
        self.apps_dir.join(app_id)
    }

    /// compose 專案目錄：上傳的 compose 為 App 目錄；Git 來源為儲存庫中的指定目錄
    fn project_dir(&self, app_id: &str, git: Option<&GitSource>) -> PathBuf {
        match git {
            Some(g) => self.app_dir(app_id).join("src").join(&g.path),
            None => self.app_dir(app_id),
        }
    }

    fn compose_path(&self, app_id: &str, git: Option<&GitSource>) -> PathBuf {
        match git {
            Some(g) => self.project_dir(app_id, git).join(&g.compose_file),
            None => self.app_dir(app_id).join("compose.yml"),
        }
    }

    /// FR-APP-13：重新取得儲存庫的指定 ref（淺層），回傳 compose 檔內容
    pub async fn fetch_git(&self, app_id: &str, git: &GitSource) -> anyhow::Result<String> {
        let src = self.app_dir(app_id).join("src");
        if tokio::fs::try_exists(&src).await? {
            tokio::fs::remove_dir_all(&src).await?;
        }
        tokio::fs::create_dir_all(&src).await?;
        let g = |args: &[&str]| {
            let mut c = Command::new(&self.git_bin);
            // 不提示輸入帳密，避免卡住（ADR 0009：只支援公開儲存庫）
            c.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(&src).args(args);
            c
        };
        run(&mut g(&["init", "-q"])).await?;
        run(&mut g(&["fetch", "-q", "--depth", "1", &git.url, &git.git_ref]))
            .await
            .map_err(|e| anyhow::anyhow!("無法取得 {} 的 {}：{e}", git.url, git.git_ref))?;
        run(&mut g(&["checkout", "-q", "FETCH_HEAD"])).await?;
        let path = self.compose_path(app_id, Some(git));
        tokio::fs::read_to_string(&path).await.map_err(|_| {
            anyhow::anyhow!(
                "儲存庫中找不到 {}，請確認目錄與檔名。",
                Path::new(&git.path).join(&git.compose_file).display()
            )
        })
    }

    /// 寫入 compose、`.env` 與 override，然後 `up -d --wait`（Git 來源加上 `--build`）。重複呼叫即為重新部署。
    pub async fn deploy(&self, app_id: &str, spec: DeploySpec<'_>) -> anyhow::Result<String> {
        let project = caloud_core::project_name(app_id);
        let dir = self.app_dir(app_id);
        tokio::fs::create_dir_all(&dir).await?;
        // Git 來源由呼叫端先以 fetch_git 取得並檢查（避免部署未經檢查的內容）
        if spec.git.is_none() {
            tokio::fs::write(dir.join("compose.yml"), serde_yaml::to_string(&spec.compose.doc)?).await?;
        }
        let env_path = self.project_dir(app_id, spec.git).join(".env");
        match spec.env {
            Some(env) => tokio::fs::write(&env_path, env).await?,
            None if spec.git.is_none() => {
                // 上傳的 compose 不保留舊的 .env；忽略不存在的情況
                let _ = tokio::fs::remove_file(&env_path).await;
            }
            None => {}
        }
        tokio::fs::write(dir.join("caloud.override.yml"), self.override_yaml(app_id, &spec)?).await?;

        let services: Vec<&str> = spec.services.unwrap_or_default().iter().map(String::as_str).collect();
        let mut pull = vec!["pull", "--quiet", "--ignore-buildable"];
        pull.extend(&services);
        // 拉取失敗留給 up 回報
        let _ = self.compose(app_id, spec.git, &pull).await;
        let mut up = vec!["up", "-d", "--remove-orphans", "--wait", "--wait-timeout", "300"];
        if spec.git.is_some() {
            up.push("--build");
        }
        up.extend(&services);
        self.compose(app_id, spec.git, &up).await?;
        Ok(project)
    }

    pub async fn stop(&self, app_id: &str, git: Option<&GitSource>) -> anyhow::Result<()> {
        self.compose(app_id, git, &["stop"]).await.map(|_| ())
    }

    /// 移除容器與網路；`purge_volumes` 為 true 時一併刪除資料
    pub async fn remove(&self, app_id: &str, git: Option<&GitSource>, purge_volumes: bool) -> anyhow::Result<()> {
        let mut args = vec!["down", "--remove-orphans"];
        if purge_volumes {
            args.push("--volumes");
        }
        self.compose(app_id, git, &args).await?;
        // App 目錄可能不存在（從未部署）
        let _ = tokio::fs::remove_dir_all(self.app_dir(app_id)).await;
        Ok(())
    }

    pub async fn status(&self, app_id: &str, git: Option<&GitSource>) -> anyhow::Result<Vec<ServiceStatus>> {
        let out = self.compose(app_id, git, &["ps", "--all", "--format", "json"]).await?;
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

    pub async fn logs(&self, app_id: &str, git: Option<&GitSource>, service: Option<&str>, tail: u32) -> anyhow::Result<String> {
        let tail = tail.to_string();
        let mut args = vec!["logs", "--no-color", "--timestamps", "--tail", &tail];
        if let Some(s) = service {
            args.push(s);
        }
        self.compose(app_id, git, &args).await
    }

    /// FR-APP-07、FR-APP-16：Caloud 加入的設定（docs/07-security.md §3.4、§3.5）
    pub fn override_yaml(&self, app_id: &str, spec: &DeploySpec<'_>) -> anyhow::Result<String> {
        let project = caloud_core::project_name(app_id);
        let mut services = Mapping::new();
        for name in &spec.compose.services {
            let orig = spec.compose.doc["services"][name.as_str()]
                .as_mapping()
                .cloned()
                .unwrap_or_default();
            let host_network = orig.get("network_mode").and_then(Value::as_str) == Some("host");
            let mut svc = Mapping::new();

            // host 網路的 service 不能加入其他網路；其餘接上 edge 網路並以 <project>-<service> 為 alias
            if !host_network {
                let mut edge = Mapping::new();
                edge.insert(
                    "aliases".into(),
                    Value::Sequence(vec![caloud_core::model_upstream_host(app_id, name).into()]),
                );
                let mut nets = Mapping::new();
                nets.insert("default".into(), Value::Mapping(Mapping::new()));
                nets.insert(self.edge_network.clone().into(), Value::Mapping(edge));
                svc.insert("networks".into(), Value::Mapping(nets));
            }
            let mut labels = Mapping::new();
            labels.insert("io.caloud.app".into(), app_id.into());
            labels.insert("io.caloud.project".into(), project.clone().into());
            svc.insert("labels".into(), Value::Mapping(labels));
            // 已設定的 restart 一律保留（例如一次性的初始化 service 為 "no"）
            if !orig.contains_key("restart") {
                svc.insert("restart".into(), "unless-stopped".into());
            }
            if !spec.privileged {
                // 預設資源上限，避免單一 App 拖垮主機（之後可改為依使用者配額）
                svc.insert("mem_limit".into(), "1g".into());
                svc.insert("pids_limit".into(), 512.into());
            }
            // 特權 App：移除 Caloud 保留的主機連接埠，改由網域轉發
            if let Some(ports) = orig.get("ports").and_then(Value::as_sequence) {
                let kept: Vec<Value> = ports
                    .iter()
                    .filter(|p| !host_port(p).is_some_and(|h| spec.reserved_ports.contains(&h)))
                    .cloned()
                    .collect();
                if kept.len() != ports.len() {
                    svc.insert(
                        "ports".into(),
                        Value::Tagged(Box::new(TaggedValue {
                            tag: Tag::new("override"),
                            value: Value::Sequence(kept),
                        })),
                    );
                }
            }
            services.insert(name.clone().into(), Value::Mapping(svc));
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

    /// 有 compose 檔時帶入檔案；沒有時（從未部署或已刪除）只以專案名稱操作既有容器
    async fn compose(&self, app_id: &str, git: Option<&GitSource>, args: &[&str]) -> anyhow::Result<String> {
        let dir = self.app_dir(app_id);
        let project = caloud_core::project_name(app_id);
        let compose_path = self.compose_path(app_id, git);
        let mut cmd = Command::new(&self.docker_bin);
        cmd.arg("compose").args(["-p", &project]);
        if tokio::fs::try_exists(&compose_path).await.unwrap_or(false) {
            cmd.arg("--project-directory")
                .arg(self.project_dir(app_id, git))
                .arg("-f")
                .arg(&compose_path)
                .arg("-f")
                .arg(dir.join("caloud.override.yml"));
        }
        cmd.args(args);
        if tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            cmd.current_dir(&dir);
        }
        run(&mut cmd).await
    }
}

/// ports 項目的主機埠；無法判斷（只有容器埠、變數、範圍）時回傳 None
fn host_port(p: &Value) -> Option<u16> {
    match p {
        Value::String(s) => {
            let spec = s.split('/').next().unwrap_or_default();
            let parts: Vec<&str> = spec.split(':').collect();
            let host = match parts.len() {
                2 => parts[0],
                3 => parts[1],
                _ => return None,
            };
            host.parse().ok()
        }
        Value::Mapping(m) => match m.get("published") {
            Some(Value::Number(n)) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

async fn run(cmd: &mut Command) -> anyhow::Result<String> {
    let out = cmd.stdin(Stdio::null()).output().await?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{validate_with, Policy};

    fn runner() -> ComposeRunner {
        ComposeRunner::new("/tmp".into(), "edge".into(), "docker".into(), "git".into())
    }

    // FR-APP-16：特權 App 移除保留連接埠、保留 restart、host 網路不加入 edge、不設資源上限
    #[test]
    fn privileged_override_strips_reserved_ports() {
        let y = "services:\n  relay:\n    build: .\n    ports: ['80:80', '443:443', '7842:7842/udp']\n  init:\n    build: .\n    restart: 'no'\n  lk:\n    build: .\n    network_mode: host\n";
        let c = validate_with(
            y,
            Policy {
                privileged: true,
                repo_depth: Some(1),
            },
        )
        .unwrap();
        let spec = DeploySpec {
            compose: &c,
            git: None,
            env: None,
            services: None,
            privileged: true,
            reserved_ports: &[80, 443, 8080],
        };
        let out = runner().override_yaml("12345678-aaaa-bbbb-cccc-000000000000", &spec).unwrap();
        let v: Value = serde_yaml::from_str(&out).unwrap();
        let relay = &v["services"]["relay"];
        let Value::Tagged(t) = &relay["ports"] else {
            panic!("ports 應為 !override：{out}")
        };
        assert_eq!(t.tag, Tag::new("override"));
        assert_eq!(t.value, Value::Sequence(vec!["7842:7842/udp".into()]));
        assert!(out.contains("!override"));
        assert!(v["services"]["init"].get("restart").is_none(), "已設定的 restart 不覆寫");
        assert!(v["services"]["lk"].get("networks").is_none(), "host 網路不加入 edge");
        assert!(relay.get("mem_limit").is_none());
    }

    // FR-APP-07：一般 App 維持原本的隔離與上限
    #[test]
    fn regular_override_keeps_limits() {
        let c = crate::validate("services: {web: {image: nginx}}").unwrap();
        let spec = DeploySpec {
            compose: &c,
            git: None,
            env: None,
            services: None,
            privileged: false,
            reserved_ports: &[80],
        };
        let v: Value = serde_yaml::from_str(&runner().override_yaml("12345678-aaaa-bbbb-cccc-000000000000", &spec).unwrap()).unwrap();
        assert_eq!(v["services"]["web"]["mem_limit"], "1g");
        assert_eq!(v["services"]["web"]["restart"], "unless-stopped");
        assert!(v["services"]["web"]["networks"].get("edge").is_some());
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(host_port(&"80:80".into()), Some(80));
        assert_eq!(host_port(&"127.0.0.1:8080:80".into()), Some(8080));
        assert_eq!(host_port(&"7842:7842/udp".into()), Some(7842));
        assert_eq!(host_port(&"3000".into()), None);
        assert_eq!(host_port(&"${PORT:-7843}:7843/udp".into()), None);
    }
}
