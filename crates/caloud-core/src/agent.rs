//! Runner agent 與控制面之間的協定型別（docs/06 §6，ADR 0011）。
use crate::placement::Resources;
use serde::{Deserialize, Serialize};

/// WebSocket 端點
pub const CONNECT_PATH: &str = "/agent/v1/connect";
/// 一次性加入權杖前綴
pub const JOIN_PREFIX: &str = "caloud_join_";
/// 長期節點權杖前綴
pub const NODE_PREFIX: &str = "caloud_node_";
/// 心跳間隔（秒）
pub const HEARTBEAT_SECS: u64 = 15;
/// 超過這麼久沒有心跳視為離線（秒）
pub const OFFLINE_AFTER_SECS: i64 = 60;
/// 連線後須在此時間內送出 register（秒）
pub const REGISTER_TIMEOUT_SECS: u64 = 10;

/// agent 偵測到的節點規格
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSpecs {
    pub cpu_cores: u32,
    pub arch: String,
    pub memory_bytes: u64,
    pub disk_free_bytes: u64,
    pub os: String,
    pub gvisor: bool,
    pub buildkit: bool,
    /// 自我檢查通過：gVisor、防火牆、磁碟、探測環境（FR-OPS-12）
    #[serde(default)]
    pub isolation: bool,
    /// 自我檢查未通過的原因
    #[serde(default)]
    pub self_check: Option<String>,
    pub agent_version: String,
}

/// 控制面送給 agent 的環境（FR-OPS-13，ADR 0015）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSpec {
    /// 子網域 ID
    pub id: String,
    /// `production` | `staging` | `verify`
    pub kind: String,
    /// 已通過平臺政策的 compose；有 `source` 時可含 `build`（ADR 0018）
    pub compose: String,
    /// 節點內的網段索引（ADR 0015）
    pub subnet_index: u16,
    pub public_service: String,
    /// 從原始碼建置（ADR 0018）；None 表示 compose 只用 image
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<EnvSource>,
    /// 秘密變數（ADR 0019）：`docker compose up` 時放進行程環境變數；Debug 輸出遮蔽值
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<EnvValue>,
}

/// 一個秘密變數；`user` 為用戶變數（注入從儲存庫建置的服務），否則為平臺產生（只供 compose 內插）
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvValue {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub user: bool,
}

impl std::fmt::Debug for EnvValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvValue")
            .field("name", &self.name)
            .field("value", &"***")
            .field("user", &self.user)
            .finish()
    }
}

/// 原始碼位置；`auth_header` 只在記憶體使用，Debug 輸出遮蔽（ADR 0018）
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSource {
    pub repo_url: String,
    /// 40 字元 commit
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// compose 所在目錄（儲存庫相對）；build context 以此為基準
    #[serde(default)]
    pub compose_dir: String,
    /// git 的 `http.extraHeader`（私有儲存庫）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
}

impl std::fmt::Debug for EnvSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvSource")
            .field("repo_url", &self.repo_url)
            .field("commit", &self.commit)
            .field("git_ref", &self.git_ref)
            .field("compose_dir", &self.compose_dir)
            .field("auth_header", &self.auth_header.as_ref().map(|_| "***"))
            .finish()
    }
}

/// 部署階段（ADR 0018）
pub const STAGES: [&str; 4] = ["fetch", "build", "start", "health"];

/// 環境中服務的固定 IP
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvService {
    pub name: String,
    pub ip: String,
}

/// 環境 ID：小寫英數，6–32 字元（也作為目錄名稱）
pub fn valid_env_id(id: &str) -> bool {
    (6..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// agent → 控制面
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMsg {
    Register {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        join_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_token: Option<String>,
        name: String,
        specs: NodeSpecs,
    },
    Heartbeat {
        used: Resources,
    },
    /// 部署進度與結果：`deploying`（附 stage）| `running` | `failed` | `destroyed`
    EnvStatus {
        env_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default)]
        services: Vec<EnvService>,
    },
    /// 部署輸出，逐行（ADR 0018）
    EnvLog {
        env_id: String,
        stage: String,
        line: String,
    },
}

/// 控制面 → agent
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Ack {
        node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_token: Option<String>,
        status: String,
        heartbeat_secs: u64,
    },
    Status {
        status: String,
    },
    Error {
        message: String,
    },
    Deploy {
        env: EnvSpec,
    },
    Destroy {
        env_id: String,
    },
}

/// 節點名稱：1–63 字元，`a-z0-9-`，不以 `-` 開頭結尾（與主機名稱相同）
pub fn valid_node_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-OPS-04
    #[test]
    fn messages_use_tagged_json() {
        let m: AgentMsg = serde_json::from_str(r#"{"type":"heartbeat","used":{"cpu_millis":5,"memory_bytes":7}}"#).unwrap();
        assert!(matches!(m, AgentMsg::Heartbeat { used } if used.cpu_millis == 5));
        let ack = ServerMsg::Ack {
            node_id: "n".into(),
            node_token: None,
            status: "active".into(),
            heartbeat_secs: 15,
        };
        let s = serde_json::to_string(&ack).unwrap();
        assert_eq!(s, r#"{"type":"ack","node_id":"n","status":"active","heartbeat_secs":15}"#);
        assert!(valid_node_name("runner-tky-01"));
        // ADR 0018：權杖不出現在 Debug；舊訊息沒有 source 仍可解析
        let src = EnvSource {
            repo_url: "https://github.com/a/b".into(),
            commit: "c".repeat(40),
            git_ref: None,
            compose_dir: String::new(),
            auth_header: Some("Authorization: Basic c2VjcmV0".into()),
        };
        assert!(!format!("{src:?}").contains("c2VjcmV0"));
        let d: ServerMsg = serde_json::from_str(
            r#"{"type":"deploy","env":{"id":"abcdefgh","kind":"verify","compose":"services: {}","subnet_index":1,"public_service":"web"}}"#,
        )
        .unwrap();
        assert!(matches!(d, ServerMsg::Deploy { env } if env.source.is_none()));
        assert!(!valid_node_name("-x"));
        assert!(!valid_node_name("Runner"));
    }
}
