use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    SuperAdmin,
    Admin,
    Member,
    Pending,
    Disabled,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::SuperAdmin => "super_admin",
            Role::Admin => "admin",
            Role::Member => "member",
            Role::Pending => "pending",
            Role::Disabled => "disabled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "super_admin" => Role::SuperAdmin,
            "admin" => Role::Admin,
            "member" => Role::Member,
            "pending" => Role::Pending,
            "disabled" => Role::Disabled,
            _ => return None,
        })
    }
    /// 可以部署 App、管理自己的網域
    pub fn can_deploy(&self) -> bool {
        matches!(self, Role::SuperAdmin | Role::Admin | Role::Member)
    }
    /// 可以管理使用者與所有資源
    pub fn is_admin(&self) -> bool {
        matches!(self, Role::SuperAdmin | Role::Admin)
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub account: String,
    pub handle: Option<String>,
    pub role: String,
    pub created_at: i64,
    pub last_login: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct App {
    pub id: String,
    pub name: String,
    pub owner: String,
    #[serde(skip_serializing)]
    pub compose_yaml: String,
    pub status: String,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// `compose`（上傳）或 `git`（FR-APP-13）
    pub source: String,
    pub git_url: Option<String>,
    pub git_ref: Option<String>,
    pub git_path: Option<String>,
    pub compose_file: Option<String>,
    /// JSON 陣列；None = 全部 service（FR-APP-15）
    #[serde(serialize_with = "ser_json_list")]
    pub services: Option<String>,
    /// `.env` 內容；對外只輸出變數名稱（FR-APP-14）
    #[serde(rename = "env_keys", serialize_with = "ser_env_keys")]
    pub env: Option<String>,
    pub privileged: bool,
}

impl App {
    pub fn service_list(&self) -> Option<Vec<String>> {
        self.services.as_deref().and_then(|s| serde_json::from_str(s).ok())
    }
}

/// `.env` 內容中的變數名稱（忽略註解與空行）
pub fn env_keys(env: &str) -> Vec<String> {
    env.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, _)| k.trim().trim_start_matches("export ").trim().to_string())
        })
        .filter(|k| !k.is_empty())
        .collect()
}

fn ser_env_keys<S: serde::Serializer>(env: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    env_keys(env.as_deref().unwrap_or_default()).serialize(s)
}

fn ser_json_list<S: serde::Serializer>(v: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    let list: Option<Vec<String>> = v.as_deref().and_then(|x| serde_json::from_str(x).ok());
    list.serialize(s)
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Domain {
    pub hostname: String,
    pub owner: String,
    pub verify_token: String,
    pub verified: bool,
    pub app_id: Option<String>,
    pub service: Option<String>,
    pub port: Option<i64>,
    pub tls_status: String,
    pub created_at: i64,
    /// `none` 或 `passthrough`（FR-DOM-09）
    pub tls_mode: String,
    pub tls_port: Option<i64>,
    #[serde(skip_serializing)]
    pub target_host: Option<String>,
}

/// 反向代理使用的一條路由：hostname → upstream
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub hostname: String,
    /// HTTP 轉發目標，例：http://caloud-<app>-web:3000
    pub upstream: String,
    /// TLS 直通目標（FR-DOM-09），例：caloud-<app>-relay:443
    pub passthrough: Option<String>,
    /// 環境所在的 Runner 節點；有值時經該節點的通道轉發（FR-OPS-09）
    pub node: Option<String>,
}

/// host 網路 service 的轉發目標（FR-DOM-10）
pub const HOST_GATEWAY: &str = "host.docker.internal";

/// 個人 API 權杖（不含雜湊）
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ApiToken {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_used_at: Option<i64>,
}

/// 平臺專案（FR-GH-04、FR-AN-10，docs/06 §3.7）
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Project {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub repo_url: String,
    pub git_ref: Option<String>,
    pub branch: Option<String>,
    pub root: String,
    /// `analyzing` | `needs_review` | `confirmed` | `error`
    pub status: String,
    pub error: Option<String>,
    pub confirmed_analysis: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 一次解析的紀錄；`result` 為 `caloud_analyzer::Analysis` JSON
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AnalysisRow {
    pub id: String,
    pub project: String,
    pub git_ref: String,
    pub commit_sha: String,
    pub result: String,
    pub compose: Option<String>,
    pub public_service: Option<String>,
    pub acknowledged: String,
    pub confirmed_by: Option<String>,
    pub confirmed_at: Option<i64>,
    pub created_at: i64,
}
