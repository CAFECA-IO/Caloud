# 03 開發計劃

> 版本 0.1 · 狀態：Accepted · 相關：[01 需求](01-requirements.md)、[08 開發規範](08-coding-standards.md)

## 1. 里程碑

| 里程碑 | 目標 | 出場條件 |
| --- | --- | --- |
| **M1 骨架** | 端到端走通：登入 → 上傳 compose → 綁定網域 → HTTP 轉發 | M1 的 P0 需求全部完成並通過驗收；可在測試主機以 HTTP 示範 |
| **M2 可上線** | HTTPS、穩定性、安全強化 | M2 的 P0 完成；在正式網域以 HTTPS 運行 7 天無重大事件 |
| **M3 營運** | 日常營運所需功能 | M3 的 P0／P1 完成 |
| **M4 平臺基礎** | 控制面＋Runner 節點、`*.caloud.tw` 子網域與 wildcard 憑證、後台網域資源（[ADR 0010](adr/0010-platform.md)） | 維運者可經 API 在 Runner 開出 8／12／16 字元環境，環境之間網路不通 |
| **M5 GitHub 與解析** | GitHub App、服務自動解析、匯入流程 | 用 10 個真實儲存庫匯入，8 個以上不需修改就能啟動 |
| **M6 驗證與報告** | 自動驗證、分級報告、GitHub Checks | 連續 100 次驗證無殘留資源；故意放入的錯誤都被正確分級 |
| **M7 環境、網域、遷移與 MCP** | staging／production、回滾、秘密、發布前任務、worker 與排程、自訂網域、自有設備、既有服務遷移、範圍化 MCP | Claude Code 只用 MCP 把一個壞掉的專案修到驗證通過並部署 staging；iSunFA 由舊主機遷入，帳號與資料比對全數通過 |
| **M8 訂閱與上線** | 方案、計量、配額、後台畫面、備份 | 第一批外部用戶可自助訂閱並上線 |

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
| M1-15 | 測試主機端到端驗收（真實 CAFECA 錢包 + Docker）：2026-11-09 於 57.182.212.18 完成 Docker 部署、MCP 部署／轉發流程與真實 CAFECA 登入（第一位成為 Super Admin）；待以真實網域完成 DNS 驗證 | M1 出場條件 | 🚧 |

### M2 可上線

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M2-01 | rustls SNI 憑證選擇、`:443` 監聽 | FR-DOM-05 | → M4-08 |
| M2-02 | instant-acme 申請／續簽、HTTP-01、`:80` 轉 HTTPS | FR-DOM-05 | → M4-08 |
| M2-03 | 管理介面改經 `:443` 的 `PUBLIC_ORIGIN` 提供 | FR-DOM-05、NFR-SEC-01 | ⬜ |
| M2-04 | WebSocket／Upgrade 轉發 | FR-DOM-06 | → M4-07 |
| M2-05 | 每 App 部署互斥鎖 | FR-APP-08 | ⬜ |
| M2-06 | 啟動對帳與定期對帳 | FR-APP-09 | ⬜ |
| M2-07 | 寫入 API 檢查 `Origin` header、速率限制 | NFR-SEC-01 | ⬜ |
| M2-08 | 代理效能基準測試 | NFR-PERF-01 | ⬜ |
| M2-09 | Git 來源 App、環境變數、指定 service | FR-APP-13、FR-APP-14、FR-APP-15 | ✅ |
| M2-10 | 特權 App 政策與保留連接埠移除（`!override`） | FR-APP-16 | ✅ |
| M2-11 | 443 SNI 路由與 TLS 直通；host 網路 service 轉發 | FR-DOM-09、FR-DOM-10 | ✅ |
| M2-12 | 網頁與 MCP 支援上述設定 | FR-APP-13～16、FR-DOM-09 | ✅ |
| M2-13 | 以 Caloud 部署 LiveKit（livekit.cafeca.io、turn.cafeca.io）與 iroh-relay（relay.cafeca.io） | FR-APP-13、FR-DOM-09 | 🚧 |

### M3 營運

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M3-01 | `CALOUD_BOOTSTRAP_ACCOUNT` | FR-AUTH-08 | ⬜ |
| M3-02 | log 即時串流（SSE） | FR-APP-10 | ⬜ |
| M3-03 | 環境變數與 secrets（加密） | FR-APP-11 | → M7-03 |
| M3-04 | 使用者配額 | FR-APP-12 | ⬜ |
| M3-05 | 稽核紀錄查詢介面 | FR-AUD-02 | ⬜ |
| M3-06 | 多網域、萬用字元網域 | FR-DOM-08 | → M4-08 |

狀態「→ M4-08」等表示併入平臺里程碑的該項目一起完成。

### M4 平臺基礎

設計依據：Caloud Platform 系統設計 §5、§9、§13；決策見 [ADR 0010](adr/0010-platform.md)、[ADR 0011](adr/0011-runner-websocket.md)、[ADR 0012](adr/0012-runner-crate.md)。

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M4-01 | 平臺需求、計劃、架構、API、安全文件與 ADR 0010～0012 | NFR-QA-01 | ✅ |
| M4-02 | 子網域 ID 產生與檢查（長度、字元、保留字、封鎖、未使用） | FR-ENV-07 | ✅ |
| M4-03 | 後台網域資源：資料表、服務層、Admin API | FR-OPS-03 | ✅ |
| M4-04 | `caloud-agent`：規格偵測與可配置量 | FR-OPS-04 | ✅ |
| M4-05 | 節點加入權杖、WebSocket 註冊與心跳、離線判定、排空 | FR-OPS-04、FR-OPS-06 | ✅ |
| M4-06 | 依容量選擇節點 | FR-OPS-08 | ✅ |
| M4-07 | Edge 經 Runner 通道（yamux）轉發；WebSocket／Upgrade 轉發 | FR-OPS-09、FR-DOM-06 | ⬜ |
| M4-08 | `*.caloud.tw` wildcard 憑證（Cloudflare DNS-01）、rustls SNI、`:80` 轉 HTTPS | FR-OPS-10、FR-DOM-05、FR-DOM-08 | ⬜ |
| M4-09 | 用戶容器以 gVisor 執行、環境網路隔離、封鎖 metadata | FR-OPS-11 | ⬜ |

### M5 GitHub 與解析

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M5-01 | GitHub App 安裝、installation token、webhook 驗簽 | FR-GH-01、FR-GH-02 | ⬜ |
| M5-02 | 公開儲存庫網址匯入 | FR-GH-04 | ⬜ |
| M5-03 | 解析器：compose、Dockerfile、Node、Rust、Go、Python 範本、附加資料庫、環境變數 | FR-AN-01、FR-AN-02 | ⬜ |
| M5-04 | 信心度與依據；`.caloud/compose.yml` 覆寫；編輯產生結果 | FR-AN-03、FR-AN-04 | ⬜ |
| M5-05 | 匯入與解析確認畫面 | FR-AN-04 | ⬜ |
| M5-06 | 平臺 App compose 政策（repo 內 build、`env_file` 轉換） | FR-AN-05 | ⬜ |
| M5-07 | 子目錄（monorepo）根目錄 | FR-AN-06 | ⬜ |
| M5-08 | 偵測持久化目錄、系統套件與原生模組、建置時變數 | FR-AN-07、FR-AN-08、FR-AN-09 | ⬜ |

### M6 驗證與報告

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M6-01 | 驗證狀態機與 10 個階段、逾時、取消舊執行 | FR-CI-01、FR-CI-02、FR-CI-04 | ⬜ |
| M6-02 | 銷毀與資源回收、失敗保留 30 分鐘 | FR-CI-03、FR-CI-05 | ⬜ |
| M6-03 | 問題分級、指紋去重、與前次比較 | FR-RPT-01、FR-RPT-02 | ⬜ |
| M6-04 | 報告儲存、匯出、通知、驗證執行與報告畫面 | FR-RPT-03、FR-RPT-04 | ⬜ |
| M6-05 | GitHub Checks 與 PR 留言 | FR-GH-03 | ⬜ |
| M6-06 | 稽核來源標示 | FR-OPS-01 | ⬜ |
| M6-07 | 建置資源與執行配額分開 | FR-CI-06 | ⬜ |
| M6-08 | 驗證環境執行發布前任務 | FR-CI-07 | ⬜ |

### M7 環境、網域與 MCP

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M7-01 | 發布、production 藍綠切換、回滾、發布關卡 | FR-ENV-01、FR-ENV-03、FR-ENV-04 | ⬜ |
| M7-02 | Staging 隨選部署、休眠與喚醒 | FR-ENV-02 | ⬜ |
| M7-03 | 秘密變數（加密、依環境、遮蔽） | FR-ENV-05、FR-APP-11 | ⬜ |
| M7-04 | 自訂網域：驗證、憑證、直通、每日重驗 | FR-DOM-11～13 | ⬜ |
| M7-05 | 用戶自有設備：加入、執行位置、通道 | FR-DEV-01～03 | ⬜ |
| M7-06 | 容量告警、離線節點重建 | FR-OPS-07 | ⬜ |
| M7-07 | 範圍化 MCP 權杖、patch 驗證、部署工具、設定頁 | FR-MCP-09～12 | ⬜ |
| M7-08 | 發布前任務（migration、seed）於切換前執行 | FR-ENV-08 | ⬜ |
| M7-09 | 常駐 worker 與排程任務 | FR-ENV-09 | ⬜ |
| M7-10 | 部署策略（藍綠／先停後啟）與依環境建置 | FR-ENV-10、FR-ENV-11 | ⬜ |
| M7-11 | 遷移：建立遷移、綁定網域、DNS-01 委派預發憑證 | FR-MIG-01、FR-MIG-02 | ⬜ |
| M7-12 | `caloud-agent --migrate`：資料庫與檔案快照、來源端雜湊 | FR-MIG-03 | ⬜ |
| M7-13 | 盤點、演練環境、資料比對 | FR-MIG-04～06 | ⬜ |
| M7-14 | 帳號驗收：逐筆比對、hosts 預覽、驗收清單 | FR-MIG-07 | ⬜ |
| M7-15 | 寫入凍結、最後同步、切換、監控、回退、遷移報告與畫面 | FR-MIG-08～10 | ⬜ |

### M8 訂閱與上線

| # | 工作項目 | 需求 | 狀態 |
| --- | --- | --- | --- |
| M8-01 | 方案、訂閱、到期處理 | FR-ACC-01 | ⬜ |
| M8-02 | 計量與用量頁 | FR-ACC-02 | ⬜ |
| M8-03 | 後台：方案與價格、網域資源、Runner 節點、用戶管理畫面 | FR-OPS-02、FR-OPS-03、FR-OPS-05 | ⬜ |
| M8-04 | Production volume 備份與還原 | FR-ENV-06 | ⬜ |

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
| ACME 速率限制 | 中 | 開發用 staging；網域須先驗證才申請；平臺子網域只用一張 wildcard 憑證 | M2／M4 |
| 執行任意用戶程式碼 | 高 | 控制面與 Runner 分機、gVisor、環境網路隔離（FR-OPS-11） | M4 |
| caloud.tw 未進 Public Suffix List 前用戶可互設 cookie | 中 | 及早送審；主控台在 cafeca.io 不受影響 | M4 |
| 遷移後資料不一致或遺失 | 高 | 寫入凍結後最後同步；逐表雜湊與檔案清單比對全數通過才可切換；舊站保留 7 天可回退（FR-MIG-06～09） | M7 |
| 遷移過程傳輸個資 | 高 | 只連出的 agent、TLS 傳輸、平臺端加密保存、完成後依保留期刪除快照 | M7 |
