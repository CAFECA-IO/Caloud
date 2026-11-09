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
}

/// 反向代理使用的一條路由：hostname → upstream
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub hostname: String,
    pub upstream: String, // 例：http://caloud-<app>-web-1:3000
}

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
