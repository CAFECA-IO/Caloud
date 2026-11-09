# 01 需求規格

> 版本 0.1 · 狀態：Accepted · 相關：[02 架構](02-architecture.md)、[06 API](06-api-spec.md)、[07 安全](07-security.md)

## 1. 產品概述

Caloud 是自架的容器托管平台。管理者以 CAFECA 身分登入 Web 介面，上傳 docker compose 檔即時啟動容器，並把自己托管的網域轉向這些容器。

### 1.1 使用者角色

| 角色 | 說明 |
| --- | --- |
| Super Admin | 部署後第一位登入者，全站唯一，擁有所有權限 |
| Admin | 由 Super Admin 授予，可管理所有資源與一般使用者 |
| Member | 可建立與管理自己的 App 和網域 |
| Pending | 已登入但尚未核准，無法操作任何資源 |
| Disabled | 已停用，無法登入 |
| 訪客 | 透過已綁定網域存取容器服務的一般網路使用者（不登入 Caloud） |

### 1.2 範圍

- **範圍內**：單機 Docker 主機、HTTP/HTTPS 轉發、CAFECA 登入、Web 管理介面
- **範圍外（目前）**：多節點叢集、TCP/UDP 非 HTTP 轉發、映像建置（build）、計費

## 2. 功能需求（FR）

優先級：**P0** 必要（該里程碑不可缺）、**P1** 重要、**P2** 加分。里程碑定義見 [03 開發計劃](03-development-plan.md)。

### 2.1 身分驗證 AUTH

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-AUTH-01 | 使用者可在登入頁以「Sign in with CAFECA」（popup 模式）登入；後端依 CAFECA 規格驗證：網域、nonce、chainId、有效期，EIP-712 `SignIn` 雜湊，經公開 RPC 呼叫 `isValidSignature` 須回傳 `0x1626ba7e` | P0 | M1 |
| FR-AUTH-02 | 登入 nonce 由後端產生（≥128 bit），有效 5 分鐘，**只能使用一次**，不論驗證成功與否皆作廢 | P0 | M1 |
| FR-AUTH-03 | 系統中**沒有任何使用者時**，第一位成功登入者自動成為 Super Admin；並發登入時保證只有一人取得 | P0 | M1 |
| FR-AUTH-04 | 第一位以外的新使用者以設定值 `CALOUD_DEFAULT_ROLE` 為角色（預設 `pending`） | P0 | M1 |
| FR-AUTH-05 | 登入成功後建立 session：有效 7 天；cookie 為 `HttpOnly`、`SameSite=Strict`，HTTPS 時加 `Secure`；資料庫只存 token 雜湊 | P0 | M1 |
| FR-AUTH-06 | 使用者可登出，該 session 立即失效 | P0 | M1 |
| FR-AUTH-07 | 使用者同意提供代稱時，由錢包 API 查詢確認後顯示；不可信任回應中自稱的代稱 | P1 | M1 |
| FR-AUTH-08 | 可用 `CALOUD_BOOTSTRAP_ACCOUNT` 預先指定 Super Admin 帳戶，設定後 FR-AUTH-03 不生效 | P1 | M3 |

**驗收條件（FR-AUTH-03）**

- Given 空資料庫，When A 登入，Then A 的角色為 `super_admin`
- Given A 已是 super_admin，When B 登入，Then B 的角色為 `CALOUD_DEFAULT_ROLE`
- Given A 再次登入，Then A 仍為 `super_admin`，代稱更新

### 2.2 使用者管理 USER

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-USER-01 | Admin 以上可檢視所有使用者（帳戶、代稱、角色、最後登入） | P0 | M1 |
| FR-USER-02 | Admin 以上可變更使用者角色，規則：Super Admin 不可被變更也不可被授予；只有 Super Admin 能授予或撤銷 Admin；不可變更自己 | P0 | M1 |
| FR-USER-03 | 使用者被設為 `disabled` 時，其所有 session 立即失效 | P0 | M1 |
| FR-USER-04 | Pending 使用者登入後只看到「等待核准」畫面 | P0 | M1 |

### 2.3 App（docker compose）APP

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-APP-01 | Member 以上可上傳 compose 檔（選檔或貼上 YAML）並命名，建立 App | P0 | M1 |
| FR-APP-02 | 建立與更新前依 [07 安全規範](07-security.md#3-compose-政策) 檢查 compose 檔，不合規時回傳**指出 service 與欄位**的錯誤訊息 | P0 | M1 |
| FR-APP-03 | 建立後預設**立即在背景部署**；狀態依序為 `stopped → deploying → running / failed`，失敗時保存錯誤訊息 | P0 | M1 |
| FR-APP-04 | 可編輯 compose 檔並重新部署 | P0 | M1 |
| FR-APP-05 | 可停止、重新部署、刪除 App；刪除預設保留資料 volume，需明確選擇才一併刪除 | P0 | M1 |
| FR-APP-06 | 可檢視每個容器的狀態與健康度，以及最近 N 行 log（預設 200，上限 5000） | P0 | M1 |
| FR-APP-07 | 每個 App 為獨立 compose project，彼此網路隔離；容器不對主機開 port，只經由 Edge 網路被代理 | P0 | M1 |
| FR-APP-08 | 同一 App 同時只能有一個部署作業，後到的請求排隊或回 409 | P0 | M2 |
| FR-APP-09 | Caloud 啟動時與 Docker 實際狀態對帳，修正 App 狀態與路由 | P1 | M2 |
| FR-APP-10 | log 即時串流（SSE） | P1 | M3 |
| FR-APP-11 | App 環境變數與 secrets 管理，加密儲存 | P1 | M3 |
| FR-APP-12 | 每位使用者的 App 數量與資源配額 | P2 | M3 |

### 2.4 網域 DOM

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-DOM-01 | Member 以上可新增網域；主機名稱正規化（小寫、去結尾點）並驗證格式；全站唯一 | P0 | M1 |
| FR-DOM-02 | 新增後顯示需設定的 DNS 紀錄（A/AAAA 與 TXT `_caloud-challenge.<host>`）；使用者按「驗證」後系統查詢 TXT，相符才標記已驗證 | P0 | M1 |
| FR-DOM-03 | 可把網域綁定到自己有權管理的 App 的某個 service 與 port；service 必須存在於 compose 檔 | P0 | M1 |
| FR-DOM-04 | 網域驗證、綁定、App 狀態改變後，路由**不需重啟即時生效**（≤ 1 秒） | P0 | M1 |
| FR-DOM-05 | 已驗證網域自動申請並續簽 TLS 憑證（ACME HTTP-01），HTTP 自動轉 HTTPS | P0 | M2 |
| FR-DOM-06 | 支援 WebSocket／HTTP Upgrade 轉發 | P0 | M2 |
| FR-DOM-07 | 未綁定或 App 未運作的網域回應 404 頁面；upstream 無回應回 502 | P0 | M1 |
| FR-DOM-08 | 同一 App 可綁多個網域；支援萬用字元網域（DNS-01） | P2 | M3 |

### 2.5 MCP 與 API 權杖 MCP

讓 AI 助手（Claude Desktop、Claude Code 等 MCP 用戶端）以使用者的身分操作 Caloud（[ADR 0008](adr/0008-mcp-interface.md)）。

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-MCP-01 | 提供 MCP 伺服器，傳輸為 Streamable HTTP，端點 `POST /mcp`（JSON-RPC 2.0），支援 `initialize`、`ping`、`tools/list`、`tools/call` | P0 | M1 |
| FR-MCP-02 | Member 以上可在介面建立具名 API 權杖（可設有效天數），權杖**只在建立時顯示一次**；可檢視自己的權杖（名稱、建立、最後使用、到期）並撤銷 | P0 | M1 |
| FR-MCP-03 | `/mcp` 只接受 `Authorization: Bearer <權杖>`，不接受 session cookie；權杖代表建立者本人，權限與其目前角色相同；角色變為 pending／disabled 後權杖立即失效 | P0 | M1 |
| FR-MCP-04 | 提供工具：查詢身分；App 的列表、詳細、建立、更新、部署、停止、刪除、log；網域的列表、新增、驗證、綁定、刪除；Admin 以上另有使用者列表與角色變更。行為與權限和 REST API 相同 | P0 | M1 |
| FR-MCP-05 | 每個工具標示 `readOnlyHint`／`destructiveHint`／`idempotentHint`；刪除類工具需傳入 `confirm: true` 才執行 | P0 | M1 |
| FR-MCP-06 | 工具錯誤以 `isError: true` 與人看得懂的訊息回傳（同 REST 錯誤訊息），協定錯誤以 JSON-RPC error 回傳 | P0 | M1 |
| FR-MCP-07 | 經由 MCP 的操作寫入稽核紀錄，細節標示 `via=mcp` 與權杖名稱；權杖建立與撤銷亦寫入稽核 | P0 | M1 |
| FR-MCP-08 | 介面提供「API 權杖」頁，建立後顯示權杖與 MCP 用戶端設定範例（可一鍵複製） | P0 | M1 |

### 2.6 稽核 AUD

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-AUD-01 | 登入、角色變更、App 建立／更新／刪除、網域建立／綁定／刪除皆寫入稽核紀錄（操作者、動作、對象、時間） | P0 | M1 |
| FR-AUD-02 | Admin 可在介面查詢稽核紀錄 | P1 | M3 |

### 2.7 Web 介面 UI

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-UI-01 | 單頁介面包含：登入、總覽、Apps、網域、使用者（僅 Admin 以上可見），以網址 hash 切換頁面 | P0 | M1 |
| FR-UI-02 | 部署中的 App 自動輪詢狀態（3 秒）直到結束 | P0 | M1 |
| FR-UI-03 | 破壞性操作（刪除 App、刪除網域）需二次確認 | P0 | M1 |
| FR-UI-04 | 介面遵循 [04 設計語言](04-design-language.md) 與 [05 通用元件](05-components.md)，支援淺色／深色，使用者可手動切換並記在本機 | P0 | M1 |
| FR-UI-05 | 每個操作有即時回饋：按鈕處理中狀態、成功 Toast、失敗就地顯示；列表首次載入顯示 Skeleton | P0 | M1 |
| FR-UI-06 | DNS 紀錄、驗證值等需要貼到其他地方的值提供一鍵複製 | P0 | M1 |
| FR-UI-07 | 網域設定以步驟引導：輸入網域 → 設定 DNS → 驗證 → 綁定 App，只顯示目前步驟的輸入 | P0 | M1 |
| FR-UI-08 | App 與網域列表可依名稱搜尋；App 可依狀態篩選 | P1 | M1 |
| FR-UI-09 | 登入後首頁為總覽：運作中 App、轉發中網域、待處理數、使用者數，以及「需要處理」清單（部署失敗、待驗證、未綁定、待核准），每項可直接前往處理 | P0 | M1 |
| FR-UI-10 | App 詳細資料（容器、綁定網域、compose 編輯、log）在側邊 Drawer 中檢視，不離開列表 | P0 | M1 |
| FR-UI-11 | 版面為側欄導覽＋內容；寬度 < 768px 時側欄改為頂部列；側欄顯示待核准人數 | P0 | M1 |
| FR-UI-12 | 提供 `/styleguide` 元件總覽頁，列出所有元件與 Token | P1 | M1 |
| FR-UI-13 | 採用單一正式介面風格「企業儀表板」，不保留其他風格與切換機制（[ADR 0007](adr/0007-ui-style-proposals.md)） | P0 | M1 |

## 3. 非功能需求（NFR）

| 編號 | 類別 | 需求 |
| --- | --- | --- |
| NFR-SEC-01 | 安全 | 遵循 [07 安全規範](07-security.md) 全部規則 |
| NFR-SEC-02 | 安全 | 所有寫入 API 需登入且通過角色檢查；資源類 API 需為擁有者或 Admin |
| NFR-SEC-03 | 安全 | 錯誤回應不洩漏內部細節（500 只回「伺服器錯誤」，細節寫 log） |
| NFR-PERF-01 | 效能 | 代理額外延遲 p99 < 5 ms（同主機 upstream、1 KB 回應） |
| NFR-PERF-02 | 效能 | 路由查詢無鎖（ArcSwap），重新載入不阻塞進行中的請求 |
| NFR-REL-01 | 可靠 | Caloud 重啟不影響已運行容器（容器 `restart: unless-stopped`） |
| NFR-OPS-01 | 維運 | 單一執行檔／單一容器部署；全部設定經環境變數（前綴 `CALOUD_`） |
| NFR-OPS-02 | 維運 | 結構化 log（tracing），層級可由 `RUST_LOG` 調整 |
| NFR-A11Y-01 | 無障礙 | 文字對比 ≥ WCAG AA（4.5:1）；所有操作可用鍵盤完成；表單欄位有標籤 |
| NFR-I18N-01 | 語系 | 介面文案為繁體中文（zh-TW），依 [04 設計語言](04-design-language.md#5-文案) 用語 |
| NFR-COMPAT-01 | 相容 | 支援最新兩版 Chrome、Safari、Firefox、Edge；寬度 ≥ 360px 可用 |
| NFR-COMPAT-02 | 相容 | Docker Engine ≥ 24、Compose plugin ≥ 2.20（需 `--wait`） |
| NFR-QA-01 | 品質 | `cargo fmt`、`cargo clippy -D warnings`、`cargo test`、`scripts/check-docs.sh` 於 CI 全數通過 |
