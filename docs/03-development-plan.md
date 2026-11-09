# 03 開發計劃

> 版本 0.1 · 狀態：Accepted · 相關：[01 需求](01-requirements.md)、[08 開發規範](08-coding-standards.md)

## 1. 里程碑

| 里程碑 | 目標 | 出場條件 |
| --- | --- | --- |
| **M1 骨架** | 端到端走通：登入 → 上傳 compose → 綁定網域 → HTTP 轉發 | M1 的 P0 需求全部完成並通過驗收；可在測試主機以 HTTP 示範 |
| **M2 可上線** | HTTPS、穩定性、安全強化 | M2 的 P0 完成；在正式網域以 HTTPS 運行 7 天無重大事件 |
| **M3 營運** | 日常營運所需功能 | M3 的 P0／P1 完成 |
| **M4 進階** | 多節點等延伸功能 | 依需求另行規劃 |

## 2. 工作項目

狀態：✅ 完成 · 🚧 進行中 · ⬜ 未開始。每個項目引用 [01 需求](01-requirements.md) 編號；新增項目前需先有需求編號。

### M1 骨架

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M1-01 | Workspace、設定載入、SQLite 與 migration | NFR-OPS-01 | ✅ |
| M1-02 | CAFECA 驗證器（EIP-712 雜湊、ERC-1271 eth_call、代稱查詢） | FR-AUTH-01、FR-AUTH-07 | ✅ |
| M1-03 | nonce 發放與一次性作廢 | FR-AUTH-02 | ✅ |
| M1-04 | 第一位 super admin、預設角色 | FR-AUTH-03、FR-AUTH-04 | ✅ |
| M1-05 | session、登出、`CurrentUser` 擷取器 | FR-AUTH-05、FR-AUTH-06 | ✅ |
| M1-06 | 使用者列表與角色變更 | FR-USER-01～04 | ✅ |
| M1-07 | compose 安全政策 | FR-APP-02 | ✅ |
| M1-08 | override 產生、部署／停止／刪除／狀態／log | FR-APP-01、FR-APP-03～07 | ✅ |
| M1-09 | 網域 CRUD、DNS TXT 驗證、綁定 | FR-DOM-01～03 | ✅ |
| M1-10 | HTTP 反向代理與熱更新路由表、404/502 | FR-DOM-04、FR-DOM-07 | ✅ |
| M1-11 | 稽核紀錄寫入 | FR-AUD-01 | ✅ |
| M1-12 | Web UI 依設計語言與元件規格實作 | FR-UI-01～04 | ✅ |
| M1-16 | Design System：三層 Token、元件 CSS、共用函式與圖示 | FR-UI-04、FR-UI-05 | ✅ |
| M1-17 | 三個風格提案（企業儀表板／開發者終端／CAFECA 品牌）與對比驗證 | FR-UI-04、NFR-A11Y-01 | ✅ |
| M1-18 | 管理介面改版：AppShell、總覽、搜尋篩選、App Drawer | FR-UI-01、FR-UI-08～11 | ✅ |
| M1-19 | 網域設定 Stepper 與一鍵複製 | FR-UI-06、FR-UI-07 | ✅ |
| M1-20 | `/styleguide` 元件總覽 | FR-UI-12 | ✅ |
| M1-21 | 採用企業儀表板為正式風格，移除其他提案與預覽機制 | FR-UI-13 | ✅ |
| M1-22 | 業務邏輯抽出為服務層，REST 與 MCP 共用 | FR-MCP-04、NFR-SEC-02 | ✅ |
| M1-23 | API 權杖：資料表、建立／列表／撤銷 API、Bearer 驗證 | FR-MCP-02、FR-MCP-03 | ✅ |
| M1-24 | MCP 伺服器：`POST /mcp`、工具定義與呼叫、錯誤對應、稽核 | FR-MCP-01、FR-MCP-04～07 | ✅ |
| M1-25 | 「API 權杖」頁與 MCP 用戶端設定範例 | FR-MCP-08 | ✅ |
| M1-13 | 文件體系、`check-docs.sh`、CI | NFR-QA-01 | ✅ |
| M1-14 | API 整合測試（模擬 RPC、模擬 docker） | NFR-QA-01 | ✅ |
| M1-15 | 測試主機端到端驗收（真實 CAFECA 錢包 + Docker） | M1 出場條件 | ⬜ |

### M2 可上線

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M2-01 | rustls SNI 憑證選擇、`:443` 監聽 | FR-DOM-05 | ⬜ |
| M2-02 | instant-acme 申請／續簽、HTTP-01、`:80` 轉 HTTPS | FR-DOM-05 | ⬜ |
| M2-03 | 管理介面改經 `:443` 的 `PUBLIC_ORIGIN` 提供 | FR-DOM-05、NFR-SEC-01 | ⬜ |
| M2-04 | WebSocket／Upgrade 轉發 | FR-DOM-06 | ⬜ |
| M2-05 | 每 App 部署互斥鎖 | FR-APP-08 | ⬜ |
| M2-06 | 啟動對帳與定期對帳 | FR-APP-09 | ⬜ |
| M2-07 | 寫入 API 檢查 `Origin` header、速率限制 | NFR-SEC-01 | ⬜ |
| M2-08 | 代理效能基準測試 | NFR-PERF-01 | ⬜ |

### M3 營運

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M3-01 | `CALOUD_BOOTSTRAP_ACCOUNT` | FR-AUTH-08 | ⬜ |
| M3-02 | log 即時串流（SSE） | FR-APP-10 | ⬜ |
| M3-03 | 環境變數與 secrets（加密） | FR-APP-11 | ⬜ |
| M3-04 | 使用者配額 | FR-APP-12 | ⬜ |
| M3-05 | 稽核紀錄查詢介面 | FR-AUD-02 | ⬜ |
| M3-06 | 多網域、萬用字元網域 | FR-DOM-08 | ⬜ |

## 3. 完成定義（Definition of Done）

一個工作項目只有在以下全部成立時才能標記 ✅：

- [ ] 對應的需求編號已存在於 [01](01-requirements.md)，驗收條件已滿足
- [ ] 架構、API、元件、安全文件已同步更新（若有影響）
- [ ] 實作符合 [08 開發規範](08-coding-standards.md)
- [ ] 有測試覆蓋，測試名稱或註解引用需求編號
- [ ] `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、`scripts/check-docs.sh` 全部通過
- [ ] UI 變更：淺色／深色、寬度 360px、鍵盤操作皆已檢查
- [ ] 本文件的狀態欄已更新

## 4. 開發流程

1. **挑選工作項目**：從本文件取一個 ⬜ 項目，改為 🚧。
2. **閱讀規格**：讀對應需求、架構章節、API／元件規格。規格不清楚時先修文件，不憑猜測實作。
3. **開分支**：`feat/<工作項目編號>-<簡述>`，例：`feat/M2-01-tls-sni`。
4. **實作與測試**：依 [08 開發規範](08-coding-standards.md)。
5. **自我檢查**：執行 DoD 中的所有指令。
6. **PR**：使用 PR 範本，列出工作項目與需求編號，勾選文件遵循檢查表。
7. **審查**：審查者以文件為準檢查實作；與文件不符者退回。
8. **合併後**：更新本文件狀態為 ✅。

## 5. 風險追蹤

| 風險 | 影響 | 對策 | 負責里程碑 |
| --- | --- | --- | --- |
| 部署後被他人搶先登入成為 super admin | 高 | 部署後立即登入；M3 提供 `CALOUD_BOOTSTRAP_ACCOUNT` | M3 |
| docker.sock 等同 root | 高 | compose 政策、rootless Docker 建議、僅核准受信任 member | M1／持續 |
| CAFECA 簽章格式改變（ERC-7739） | 中 | 一律經 `isValidSignature` 驗證，不自行解析簽章 | M1 |
| 公開 RPC 不穩定 | 中 | `CALOUD_CAFECA_RPC` 可指定自架節點 | M1 |
| ACME 速率限制 | 中 | 開發用 staging；網域須先驗證才申請 | M2 |
