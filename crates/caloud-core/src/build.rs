//! 建置識別（FR-OPS-15，ADR 0023）：編譯時以 build arg 寫入；本機開發為 `dev`。

/// Cargo 版本
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 短 commit（`CALOUD_COMMIT`）
pub const COMMIT: &str = match option_env!("CALOUD_COMMIT") {
    Some(c) if !c.is_empty() => c,
    _ => "dev",
};

/// 建置時間（`CALOUD_BUILT_AT`，RFC 3339）
pub const BUILT_AT: &str = match option_env!("CALOUD_BUILT_AT") {
    Some(c) if !c.is_empty() => c,
    _ => "dev",
};

/// 是否為正式建置（有 commit）
pub fn released() -> bool {
    COMMIT != "dev"
}
