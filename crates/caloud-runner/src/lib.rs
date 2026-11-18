//! Runner agent（ADR 0012、ADR 0015）：規格偵測、連線控制面、心跳、通道、自我檢查、環境部署。
#![forbid(unsafe_code)]
pub mod client;
pub mod envs;
pub mod selfcheck;
pub mod specs;
pub mod state;
pub mod tunnel;
