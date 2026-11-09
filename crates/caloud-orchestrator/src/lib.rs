//! 把使用者上傳的 docker compose 檔安全地跑起來。
//!
//! - [`policy`]：檢查 compose 檔，拒絕可逃逸到主機的設定
//! - [`compose`]：產生 override（接上 edge 網路、加 label）並呼叫 `docker compose`
#![forbid(unsafe_code)]
pub mod compose;
pub mod policy;

pub use compose::{ComposeRunner, ServiceStatus};
pub use policy::{validate, PolicyError, ValidatedCompose};
