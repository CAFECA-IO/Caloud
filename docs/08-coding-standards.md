# 08 開發規範

> 版本 0.1 · 狀態：Accepted · 相關：[03 開發計劃](03-development-plan.md)、[05 通用元件](05-components.md)

## 1. Rust

### 1.1 工具與版本

- Rust stable，MSRV 見 `Cargo.toml` 的 `rust-version`
- 依賴版本統一在 workspace `[workspace.dependencies]` 宣告，crate 內只寫 `xxx.workspace = true`
- 新增依賴需在 PR 說明理由；優先選擇維護活躍、無 `unsafe` 疑慮的 crate；只開需要的 feature

### 1.2 風格

| 規則 | 說明 |
| --- | --- |
| 格式 | `cargo fmt`（`rustfmt.toml`：`max_width = 140`） |
| Lint | `cargo clippy --all-targets -- -D warnings` 零警告 |
| 模組 | 一個檔案一個主題；crate 相依方向依 [02 §2](02-architecture.md#相依規則不可違反) |
| 命名 | 型別 `PascalCase`、函式 `snake_case`、常數 `SCREAMING_SNAKE_CASE`；稽核 action 為 `<資源>_<動詞>` |
| 註解 | 公開項目寫 `///` 文件註解（繁體中文）；實作需求時註明編號，例：`// FR-AUTH-02：不論成功與否都作廢` |
| `unsafe` | 禁止（`#![forbid(unsafe_code)]`） |
| `unwrap` / `expect` | 僅限測試與「程式啟動時、失敗即應中止」的情況，並附理由 |

### 1.3 錯誤處理

- 函式庫 crate（core／auth／orchestrator／proxy）：可預期錯誤用 `thiserror` 定義列舉；其他用 `anyhow::Result`
- `caloud-api` handler 一律回傳 `ApiResult<T>`，錯誤轉為 `ApiError`（[05 §B](05-components.md#b-後端共用模組)）
- 給使用者看的訊息依 [04 §5.5](04-design-language.md#55-錯誤訊息)，繁體中文
- 不吞錯誤：忽略錯誤時（`let _ =`、`.ok()`）必須有註解說明為何安全

### 1.4 資料庫

- 一律使用參數綁定（`.bind()`），**禁止**字串拼接 SQL
- 使用 runtime 查詢（`sqlx::query`／`query_as`），不依賴編譯期連線資料庫
- Schema 變更：新增 `migrations/NNNN_描述.sql`，已發布的 migration 不得修改
- 需要原子性的操作優先用單一陳述式（`INSERT … ON CONFLICT … RETURNING`），其次用 transaction

### 1.5 非同步與外部指令

- 只用 tokio；不在 async 中做阻塞 I/O
- 執行外部指令只能透過 `ComposeRunner`；參數以 `.arg()` 傳遞，不經 shell
- 背景作業用 `tokio::spawn`，作業內的錯誤必須寫入狀態或 log，不可遺失

### 1.6 Log

- 使用 `tracing`；欄位用結構化形式：`tracing::info!(app = %id, "deployed")`
- 層級：`error` 需人處理、`warn` 異常但可自癒、`info` 狀態變化、`debug` 除錯
- 不記錄機密（[07 §4](07-security.md#4-機密處理)）

## 2. 測試

| 層級 | 位置 | 要求 |
| --- | --- | --- |
| 單元 | 各 crate `#[cfg(test)] mod tests` | 每個 FR 至少一個測試；純函式（policy、hash、路由正規化）必須有 |
| 整合 | `crates/caloud-api/tests/` | 啟動真實 router，外部依賴以替身取代：模擬 CAFECA 錢包（設定、RPC、代稱）與 DoH 伺服器、`CALOUD_DOCKER_BIN` 指向替身腳本（M1-14） |
| 端到端 | 手動驗收清單 | 里程碑出場前於測試主機執行 |

- 測試名稱描述行為：`first_login_becomes_super_admin`
- 測試註解引用需求：`// FR-AUTH-03`
- 密碼學相關需有外部參考實作的測試向量（例：EIP-712 與 viem 交叉比對）
- 測試不可依賴網路或真實 Docker（整合測試以介面替身）

## 3. 前端

| 規則 | 說明 |
| --- | --- |
| 架構 | 原生 HTML／CSS／JS，無建置步驟、無框架；Design System 在 `web/ds/`，頁面只組合元件（[ADR 0006](adr/0006-design-system-files.md)） |
| 樣式 | 只用 [04](04-design-language.md) 的 Token 與 [05](05-components.md) 的 class；`index.html` 不得有 `<style>` 或 `style` 屬性 |
| 外部資源 | 只允許載入 CAFECA 錢包的 `cafeca-connect.js`；不使用其他 CDN |
| 安全 | 動態內容一律 `esc()`；不使用 inline 事件屬性（`onclick="…"`）、`eval`、`alert`／`confirm` |
| 共用函式 | 使用 [05 §A.4](05-components.md#a4-前端共用函式web-dsuijs全域-ui)，不重複實作 |
| 新元件 | 先寫入 05，再實作於 `components.css`，並加入 `/styleguide` |
| 調色 | 修改語意色後重新驗證對比（[04 §2.4](04-design-language.md#24-對比驗證)） |
| 文案 | 依 [04 §5](04-design-language.md#5-文案) |

## 4. Git 與 PR

### 4.1 分支

- `main`：可部署狀態，受保護，只能經 PR 合併
- 功能：`feat/<工作項目>-<簡述>`；修正：`fix/<簡述>`；文件：`docs/<簡述>`

### 4.2 Commit

[Conventional Commits](https://www.conventionalcommits.org/)，描述用繁體中文或英文皆可：

```
feat(proxy): 支援 WebSocket 轉發 (FR-DOM-06)
fix(auth): nonce 過期判斷差一秒
docs: 新增 ADR 0006
```

類型：`feat`、`fix`、`docs`、`refactor`、`test`、`chore`、`ci`。scope 為 crate 名稱去掉 `caloud-` 前綴，或 `web`、`deploy`。

### 4.3 PR

- 使用 `.github/pull_request_template.md`，勾選文件遵循檢查表
- 一個 PR 一個工作項目；文件變更與程式碼變更同一個 PR
- CI 全綠才能合併；安全相關變更依 [07 §5](07-security.md#5-安全相關變更的審查要求) 需兩位審查

## 5. 本機檢查指令

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/check-docs.sh
```
