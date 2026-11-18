//! 把使用者上傳的 docker compose 檔安全地跑起來。
//!
//! - [`policy`]：檢查 compose 檔，拒絕可逃逸到主機的設定
//! - [`git`]：安全地淺層取得公開儲存庫
//! - [`compose`]：產生 override（接上 edge 網路、加 label）並呼叫 `docker compose`
#![forbid(unsafe_code)]
pub mod compose;
pub mod git;
pub mod policy;
pub mod sandbox;

pub use compose::{ComposeRunner, DeploySpec, GitSource, ServiceStatus};
pub use policy::{prepare_platform, validate, validate_with, PlatformCompose, Policy, PolicyError, ValidatedCompose};
