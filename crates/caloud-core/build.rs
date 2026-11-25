// sqlx::migrate! 在編譯時嵌入 migrations；新增檔案時要重新編譯
fn main() {
    println!("cargo:rerun-if-changed=migrations");
    // ADR 0023：建置識別
    println!("cargo:rerun-if-env-changed=CALOUD_COMMIT");
    println!("cargo:rerun-if-env-changed=CALOUD_BUILT_AT");
}
