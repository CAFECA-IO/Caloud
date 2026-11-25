# 01 需求規格

> 版本 0.1 · 狀態：Accepted · 相關：[02 架構](02-architecture.md)、[06 API](06-api-spec.md)、[07 安全](07-security.md)

## 1. 產品概述

Caloud 是自架的容器托管平台。管理者以 CAFECA 身分登入 Web 介面，上傳 docker compose 檔即時啟動容器，並把自己托管的網域轉向這些容器。

自 M4 起 Caloud 演進為多租戶平臺（[ADR 0010](adr/0010-platform.md)）：用戶以 CAFECA 登入並訂閱後，提供 GitHub 儲存庫即自動解析服務、產生專屬 compose，每次 commit 或新 branch 自動驗證，並可隨選啟動 staging 與 production。用戶環境在 `*.caloud.tw`（production 8 字元、staging 12 字元、驗證 16 字元子網域），`cafeca.io` 只供系統管理者（主控台 `caloud.cafeca.io` 與系統服務）。

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

- **範圍內**：單機 Docker 主機、HTTP/HTTPS 轉發、CAFECA 登入、Web 管理介面；M4 起：控制面＋多台 Runner 節點（含用戶自有設備）、GitHub 整合、自動驗證、映像建置、訂閱與配額；GPU 服務（平臺 GPU 節點以 gVisor nvproxy 執行，[ADR 0022](adr/0022-gpu.md)）
- **範圍外（目前）**：Kubernetes、非 GitHub 的 Git 服務、跨區域高可用、TCP/UDP 非 HTTP 轉發（TLS 直通與 App 自行對外的 UDP 除外）；需要 `docker.sock` 的服務只考慮未來在「信任的用戶自有設備」上執行

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
| FR-APP-13 | Admin 以上可從 **Git 儲存庫**建立 App：指定公開 HTTPS 儲存庫網址、分支或標籤、compose 所在目錄與檔名；部署時 Caloud 取得指定版本並以該目錄為專案目錄執行（可使用 `build`） | P0 | M2 |
| FR-APP-14 | App 可設定環境變數（`.env` 內容），部署時寫入專案目錄供 compose 變數替換與 `env_file` 使用；API 回應不回傳內容，只回傳變數名稱 | P0 | M2 |
| FR-APP-15 | App 可指定只啟動 compose 中的部分 service；未指定時啟動全部 | P0 | M2 |
| FR-APP-16 | Admin 以上可把 App 設為**特權 App**，套用[特權政策](07-security.md#35-特權-app-政策)：允許 `build`、`network_mode: host`、發布主機連接埠（含 UDP）、`env_file`、`tmpfs`；Caloud 保留的主機連接埠（80、443、`CALOUD_API_LISTEN` 的埠）會自動從 `ports` 移除並改由網域轉發 | P0 | M2 |

### 2.4 網域 DOM

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-DOM-01 | Member 以上可新增網域；主機名稱正規化（小寫、去結尾點）並驗證格式；全站唯一 | P0 | M1 |
| FR-DOM-02 | 新增後顯示需設定的 DNS 紀錄（A/AAAA 與 TXT `_caloud-challenge.<host>`）；使用者按「驗證」後系統查詢 TXT，相符才標記已驗證 | P0 | M1 |
| FR-DOM-03 | 可把網域綁定到自己有權管理的 App 的某個 service 與 port；service 必須存在於 compose 檔 | P0 | M1 |
| FR-DOM-04 | 網域驗證、綁定、App 狀態改變後，路由**不需重啟即時生效**（≤ 1 秒） | P0 | M1 |
| FR-DOM-05 | 已驗證、非直通且綁定運作中 App 的網域，自動申請並續簽 TLS 憑證（ACME HTTP-01，到期前 30 天）；有憑證的網域 HTTP 以 308 轉 HTTPS（[ADR 0016](adr/0016-edge-tls.md)） | P0 | M4 |
| FR-DOM-06 | 支援 WebSocket／HTTP Upgrade 轉發 | P0 | M2 |
| FR-DOM-07 | 未綁定或 App 未運作的網域回應 404 頁面；upstream 無回應回 502 | P0 | M1 |
| FR-DOM-08 | 同一 App 可綁多個網域（M4）；自訂萬用字元網域（DNS-01，需用戶的 DNS 委派）於 M7 | P2 | M4／M7 |
| FR-DOM-09 | 網域可設為 **TLS 直通**：Caloud 在 443 依 TLS ClientHello 的 SNI 判斷網域，把整條 TCP 連線原封不動轉給 App 的指定 port，由 App 自己處理 TLS 與憑證（含 TLS-ALPN-01）；同一網域的 HTTP（80）仍依綁定的 port 轉發，供 App 的 HTTP-01 驗證使用 | P0 | M2 |
| FR-DOM-10 | 綁定到 `network_mode: host` 的 service 時，Caloud 改以主機（`host.docker.internal`）的對應 port 轉發 | P0 | M2 |
| FR-DOM-11 | 平臺用戶可在專案加入自己的網域，指到 production 或某個 staging；以 TXT `_caloud-challenge.<網域>` 驗證擁有權，並提示 CNAME 指向該環境的 caloud.tw 子網域（根網域用 A 記錄） | P1 | M7 |
| FR-DOM-12 | 自訂網域驗證後由 Edge 以 HTTP-01 自動申請與續簽憑證；可改設 TLS 直通，由用戶服務自己處理憑證 | P1 | M7 |
| FR-DOM-13 | 自訂網域數量依方案；每日重新檢查 TXT，連續 7 天失效即停止路由並通知；同一網域只能綁一個專案 | P1 | M7 |

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
| FR-MCP-09 | 平臺 MCP 權杖限定專案與範圍（`read`、`verify`、`staging`、`production`、`config`），預設只有 `read`、`verify`；有效期預設 30 天、最長 365 天 | P0 | M7 |
| FR-MCP-10 | AI 可觸發驗證（指定 ref，或送出套用在某 commit 上的 patch，≤ 1 MB）、讀報告、問題與日誌；patch 驗證產生的發布只能部署到 staging | P0 | M7 |
| FR-MCP-11 | 權杖含 `staging` 範圍可部署與停止 staging；含 `production` 範圍可發布與回滾 production，仍須 `confirm: true`，且未通過驗證的發布一律拒絕 | P0 | M7 |
| FR-MCP-12 | MCP 設定頁提供 Claude Code、Cursor 等工具可直接貼上的設定 | P1 | M7 |

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
| FR-UI-14 | 專案頁分為「概況」（預設）、「設定」、「環境變數」、「紀錄」分頁：概況顯示線上版本、下一個版本與會不會自動部署、進行中的流程（耗時與預估）、需要處理的事項；開關一律使用 toggle switch（[ADR 0024](adr/0024-project-page.md)） | P0 | M7 |

### 2.8 平臺：帳號與訂閱 ACC

平臺維運者沿用 FR-AUTH-03（第一位登入者為 Super Admin）；其他登入者為訂閱用戶。

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-ACC-01 | 用戶可選方案並訂閱、續訂、取消；到期後停止新的驗證與部署，production 保留 7 天後休眠 | P0 | M8 |
| FR-ACC-02 | 用量頁顯示本期建置分鐘、執行中環境、儲存、專案數與 MCP 權杖數對照配額；達 80% 通知 | P1 | M8 |

### 2.9 平臺：GitHub GH

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-GH-01 | 透過 GitHub App 授權儲存庫，只申請 contents:read、metadata:read、checks:write、pull_requests:write、deployments:write；使用 1 小時 installation token，不存長期憑證；安裝時以一次性使用者授權確認安裝者，再綁定到 Caloud 帳號（[ADR 0017](adr/0017-github-app-public-console.md)） | P0 | M5 |
| FR-GH-02 | 接收 push、create（branch）、pull_request 事件（HMAC 驗簽、delivery 去重）並觸發驗證；驗證（M6）完成前先觸發重新解析 | P0 | M5 |
| FR-GH-03 | 驗證狀態與摘要回寫 GitHub Checks；PR 留言附報告連結：也驗證以專案分支為 base 的 PR（不注入秘密變數、不部署），每個 PR 一則留言並更新（[ADR 0021](adr/0021-checks-quota-release.md)） | P1 | M6 |
| FR-GH-06 | 部署狀態回寫 GitHub Deployments（environment `production`，進行中、成功、失敗與環境網址），只限以 GitHub App 匯入的專案（[ADR 0023](adr/0023-versions.md)） | P1 | M7 |
| FR-GH-04 | 公開儲存庫可直接以網址匯入（無事件，手動觸發） | P2 | M5 |
| FR-GH-05 | 維運者在後台設定 GitHub App 憑證（App ID、Client ID、client secret、私鑰、webhook secret），機密以主金鑰加密保存、只顯示末 4 碼；設定時以 GitHub API 確認私鑰與 App 相符 | P0 | M5 |

### 2.10 平臺：服務自動解析 AN

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-AN-01 | 依序以 `.caloud/compose.yml`、compose 檔、Dockerfile、語言標記檔（package.json、Cargo.toml、go.mod、pyproject.toml）、靜態網站解析服務、port、依賴的資料庫與環境變數；compose 只含附加服務（沒有任何 `build`）而根目錄有語言標記檔時，以範本產生 App 服務並與 compose 合併（[ADR 0014](adr/0014-analyzer-crate.md)） | P0 | M5 |
| FR-AN-02 | 產生專屬 compose，並通過 [07 §3](07-security.md#3-compose-政策) 的 compose 政策；主機 ports 移除改由平臺路由 | P0 | M5 |
| FR-AN-03 | 用戶可編輯產生結果，或在儲存庫放 `.caloud/compose.yml` 覆寫 | P0 | M5 |
| FR-AN-04 | 每個判斷標示信心度（高、中、低）與依據的檔案及行號；低信心項目在第一次驗證前需用戶確認；之後的解析若設定（有效 compose、compose 目錄、對外服務）相同且沒有新的低信心項目，自動沿用確認（[ADR 0024](adr/0024-project-page.md)） | P1 | M5 |
| FR-AN-05 | 平臺 App 適用 [07 §3.6](07-security.md#36-平臺-app-政策)：允許儲存庫內的 `build`（context、dockerfile 不超出儲存庫）；仍禁止 host 網路、主機路徑、`docker.sock`、privileged、`ports`；儲存庫 compose 中的 `env_file` 轉為平臺注入的環境變數 | P0 | M5 |
| FR-AN-06 | 專案可指定儲存庫子目錄作為根目錄（monorepo，例：`app/`） | P1 | M5 |
| FR-AN-07 | 偵測持久化目錄（寫入工作目錄下的路徑，含 `path.join(process.cwd(), …)` 等組合寫法；`*_DIR`／`*_PATH` 變數，可設定時自動填入掛載路徑），建議具名 volume；寫入但未掛 volume 列為 Medium | P1 | M5 |
| FR-AN-08 | 偵測系統套件與原生模組（ffmpeg、Chromium／Playwright、sharp、onnxruntime、node-gyp 編譯）：範本能加裝者自動加裝，否則列為 Medium 並建議自備 Dockerfile | P1 | M5 |
| FR-AN-09 | 偵測建置時變數（`NEXT_PUBLIC_*`、`VITE_*` 等），標示其值是否隨環境不同（網址、origin 類） | P1 | M5 |
| FR-AN-10 | 解析在背景執行，每次解析保存為一筆紀錄（ref、commit、結果、時間）；用戶確認的版本（含編輯過的 compose、對外服務、已確認的低信心項目）保留，不被之後的重新解析覆蓋；每位用戶最多 20 個專案 | P0 | M5 |
| FR-AN-11 | 偵測服務之間的位址：程式或環境變數預設值以 `localhost`／`127.0.0.1` 連到同一份 compose 中其他服務的 port（例：`http://127.0.0.1:5001` 對應 `ipfs_bootstrap` 的 5001），可由環境變數設定時在產生的 compose 自動填入服務名稱位址（`http://ipfs_bootstrap:5001`），列為中信心需確認；寫死無法設定時列為 High 並附依據與修改建議 | P1 | M5 |
| FR-AN-12 | 環境變數分類：呼叫形式（`os.getenv(`、`env::var(` 等）只接受字串字面量參數；程式中有預設值者為選填；compose `environment` 已提供者不需填寫；用戶可標為「不需要」；未設定提示只計算必填者（[ADR 0024](adr/0024-project-page.md)） | P1 | M7 |

### 2.11 平臺：環境 ENV

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-ENV-01 | Production：8 字元子網域、永久不變；發布以藍綠方式零停機切換 | P0 | M7 |
| FR-ENV-02 | Staging：12 字元子網域，可隨選部署任一已驗證的發布；閒置 2 小時休眠，下次請求喚醒 | P0 | M7 |
| FR-ENV-03 | 一鍵回滾到前一個發布 | P0 | M7 |
| FR-ENV-04 | 只有沒有 Blocker、High 的發布可上 production；強制發布需二次確認並寫稽核 | P0 | M7 |
| FR-ENV-05 | 秘密變數依環境分開設定，只寫不讀；日誌與報告自動遮蔽。用戶變數只注入從儲存庫建置的服務；平臺產生的變數（資料庫密碼）第一次部署時自動產生、不可修改；變更後提示重新部署才生效（[ADR 0019](adr/0019-project-vars.md)） | P0 | M7 |
| FR-ENV-06 | Production 資料 volume 每日備份、可還原 | P1 | M8 |
| FR-ENV-08 | 發布前任務（資料庫 migration、seed 等）：在切換流量前以一次性容器執行，失敗則中止發布並保留舊版；任務來自 `.caloud/verify.yml` 的 `release`、帶 profiles 的 migrate／seed 服務或 Prisma migrations，以新設定執行成功後才啟動新版（ADR 0021） | P0 | M7 |
| FR-ENV-09 | 常駐 worker（無對外 port 的長駐服務）與排程任務（cron 式，呼叫環境內的 HTTP 端點或執行指令）；排程結果與日誌可查 | P0 | M7 |
| FR-ENV-10 | 部署策略：預設藍綠；有以檔案為資料庫的 volume 或用戶指定時，改為「先停舊版再啟新版」，避免兩版同時寫入 | P1 | M7 |
| FR-ENV-11 | 建置時變數隨環境不同時，依環境分別建置；發布記錄每個環境的映像 digest，報告註明 | P1 | M7 |
| FR-ENV-12 | 專案確認後可一鍵部署到 production 環境（8 字元子網域）：在 Runner 以 gVisor 隔離的 BuildKit 建置、gVisor 執行、健康檢查通過後對外；顯示階段（取得原始碼、建置、啟動、健康檢查）與 log；以 GitHub 安裝匯入的專案 push 到該分支時自動重新部署。部署前須通過驗證（未驗證時先驗證再部署；admin 可強制並留稽核，ADR 0020）。M6／M7 完成前的簡化流程（[ADR 0018](adr/0018-minimal-deploy.md)） | P0 | M7 |
| FR-ENV-13 | GPU 服務：compose 以 `deploy.resources.reservations.devices`（`capabilities: [gpu]`）宣告，或由解析器依 CUDA 基底映像與 GPU 依賴產生（低信心、須確認）；在 `gpu_ready` 的平臺節點以 gVisor nvproxy（runtime `runsc-gpu`）執行，多個環境分時共用、不隔離顯示記憶體；執行期與驗證（含測試沙箱）可用，建置不給（[ADR 0022](adr/0022-gpu.md)） | P1 | M7 |
| FR-ENV-14 | 專案的版本可見性：分開「執行中」與「目標」版本（只在健康檢查通過後更新執行中版本）、以容器實際映像核對、注入 `CALOUD_COMMIT` 等平臺變數、Edge 回應帶 `X-Caloud-Commit`（可依專案關閉）、部署紀錄（[ADR 0023](adr/0023-versions.md)） | P0 | M7 |
| FR-ENV-15 | 部署指定版本與回滾：已通過驗證的 commit 可直接部署，曾成功上線的 commit 可「回到這個版本」（以當時的設定，不還原資料與秘密變數）；持久化目錄未掛 volume 時，可預覽 compose 修改後一鍵加上具名 volume（[ADR 0024](adr/0024-project-page.md)） | P0 | M7 |
| FR-ENV-16 | 專案資源上限：Admin 為專案設定記憶體與 CPU 的合計上限；服務在 compose 以 `mem_limit`／`cpus`（或 `deploy.resources.limits`）宣告用量，合計不超過上限即採用；未設定上限時每個服務最多 1 GB、1 核（[ADR 0025](adr/0025-project-limits.md)） | P1 | M7 |
| FR-ENV-07 | 子網域 ID 由 a-z、0-9 組成，以字母開頭且同時含字母與數字；長度即環境種類（預設 8／12／16，後台可調）；以 CSPRNG 產生，排除使用中、保留字（含包含保留字者）與封鎖清單 | P0 | M4 |

### 2.12 平臺：驗證與報告 CI、RPT

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-CI-01 | 每次 commit、新 branch 自動建立 16 字元驗證環境並測試 | P0 | M6 |
| FR-CI-02 | 驗證階段：取得原始碼、解析、建置、靜態檢查（秘密外洩、漏洞、lint）、啟動、專案測試、煙霧測試、日誌分析、彙整、銷毀（各階段與問題等級見 [ADR 0020](adr/0020-verification.md)；專案測試依語言偵測或 `.caloud/verify.yml`，在 gVisor 沙箱執行） | P0 | M6 |
| FR-CI-03 | 測試完成即銷毀容器、volume 與子網域，只保留報告；任何階段失敗仍執行彙整與銷毀 | P0 | M6 |
| FR-CI-04 | 同一 branch 有新 commit 時取消舊的執行 | P1 | M6 |
| FR-CI-05 | 驗證失敗時可選擇保留環境 30 分鐘以便除錯 | P2 | M6 |
| FR-CI-06 | 建置資源與執行配額分開：建置可用較大的記憶體與 CPU，上限依方案，計入建置分鐘；方案上線前由後台設定建置 CPU、記憶體與每位用戶每月建置分鐘，超過時拒絕驗證與部署（ADR 0021） | P0 | M6 |
| FR-CI-07 | 驗證環境也執行發布前任務（FR-ENV-08），失敗列為 Blocker | P0 | M6 |
| FR-RPT-01 | 問題分 Blocker、High、Medium、Low 四級，有 Blocker 或 High 即驗證失敗；依指紋去重；標示來源階段與位置 | P0 | M6 |
| FR-RPT-02 | 報告列出相對同 branch 上次驗證的新增、仍存在、已修正 | P1 | M6 |
| FR-RPT-03 | 報告保留期依方案；可匯出 JSON 與 Markdown | P1 | M6 |
| FR-RPT-04 | Blocker、High 問題通知用戶 | P1 | M6 |

### 2.13 平臺：用戶自有設備 DEV

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-DEV-01 | 用戶可把自己的 Linux 主機登記為私有 Runner（安裝 agent＋一次性加入權杖），規格自動偵測 | P1 | M7 |
| FR-DEV-02 | 每個環境可指定跑在平臺節點或自己的設備；自有設備只執行擁有者的專案，不計運算與儲存配額 | P1 | M7 |
| FR-DEV-03 | 自有設備在 NAT 或防火牆後也能對外服務：agent 主動連出，Edge 經同一連線的通道轉發 | P1 | M7 |

### 2.14 平臺：後台與節點 OPS

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-OPS-01 | 部署、權杖、秘密變更寫入稽核並標示來源（web、api、mcp、github） | P0 | M6 |
| FR-OPS-02 | 維運者可看全部節點負載、暫停用戶、強制停止環境 | P1 | M8 |
| FR-OPS-03 | 維運者在後台管理網域資源：網域池（根網域、用途、Cloudflare zone 與權杖、ID 長度）、保留字、封鎖清單；權杖設定時先經 Cloudflare 驗證，加密保存，只回傳末 4 碼 | P0 | M4 |
| FR-OPS-04 | Runner agent 啟動時自動偵測 CPU 核心數與架構、記憶體、可用磁碟、gVisor、BuildKit 與 NVIDIA GPU（ADR 0022），以一次性加入權杖註冊（1 小時有效、只存雜湊）；之後每 15 秒心跳；60 秒無心跳標為離線 | P0 | M4 |
| FR-OPS-05 | 方案、價格與配額在後台設定，不需改程式 | P1 | M8 |
| FR-OPS-06 | 節點可設為排空：不再接新工作，待驗證結束、環境搬移後才可移除；服務在線時即可加入或排空 | P0 | M4 |
| FR-OPS-07 | 整體已配置量 > 80% 或驗證排隊 > 5 分鐘時告警；離線節點上的 production 於其他平臺節點重建 | P1 | M7 |
| FR-OPS-08 | 調度依剩餘可配置量（偵測規格保留 10% 給主機）選擇節點；自有設備只接擁有者的工作 | P0 | M4 |
| FR-OPS-09 | Edge 依 Host 路由到環境所在節點；經 Runner 主動建立的通道轉發 | P0 | M4 |
| FR-OPS-10 | 網域池設有 Cloudflare 權杖時，`*.<根網域>` 與 `<根網域>`（如 `*.caloud.tw`、`caloud.tw`）以 ACME DNS-01 經 Cloudflare API 申請 wildcard 憑證；到期前 30 天自動續簽，失敗時退避重試（1 小時起、最多 24 小時）；`CALOUD_ACME_STAGING` 可改用 staging；後台可看各憑證狀態與錯誤（[ADR 0016](adr/0016-edge-tls.md)） | P0 | M4 |
| FR-OPS-11 | 用戶容器以 gVisor 執行；環境之間網路不通；封鎖雲端 metadata、平臺內網、Runner 主機本身與外連 25 port（[ADR 0015](adr/0015-runner-sandbox.md)） | P0 | M4 |
| FR-OPS-12 | Runner 啟動時自我檢查（gVisor runtime、防火牆、磁碟剩餘、以探測環境實際驗證隔離），結果顯示在節點規格；未通過的節點不接平臺環境 | P0 | M4 |
| FR-OPS-13 | 維運者可經 Admin API 在節點開出、列出、刪除環境（image 版 compose），以「子網域 ID＋網域池根網域」經 Runner 通道對外 | P0 | M4 |
| FR-OPS-15 | 控制面與 agent 以編譯時寫入的 commit 識別版本：`GET /api/version`、回應標頭 `X-Caloud-Build`、主控台顯示版本並在伺服器更新後提示重新整理；節點頁標示 agent 版本與控制面不同（[ADR 0023](adr/0023-versions.md)） | P0 | M7 |
| FR-OPS-14 | 祕密（Cloudflare 權杖，之後的祕密變數）以主金鑰 `CALOUD_MASTER_KEY` 加密保存（ChaCha20-Poly1305）；主金鑰只來自環境變數，不寫入資料庫、日誌或 API 回應；未設定時拒絕設定祕密 | P0 | M4 |

### 2.15 平臺：既有服務遷移 MIG

把運作中的服務（含網域、資料庫、檔案、帳號）搬到 Caloud（[ADR 0013](adr/0013-service-migration.md)）。範例：iSunFA 由舊主機遷入，網域 `isunfa.com`。

| 編號 | 需求 | 優先 | 里程碑 |
| --- | --- | --- | --- |
| FR-MIG-01 | 專案可建立「遷移」並綁定已驗證的自訂網域；驗證只需 TXT，不改 A 記錄，舊站照常服務 | P0 | M7 |
| FR-MIG-02 | 切換前取得網域憑證：用戶新增 `_acme-challenge.<網域>` CNAME 委派到 Caloud，以 DNS-01 申請，切換當下 TLS 已就緒 | P0 | M7 |
| FR-MIG-03 | 舊主機以一次性權杖執行 `caloud-agent --migrate`（只連出）；支援 PostgreSQL（`pg_dump`）、MySQL（`mysqldump`）、檔案目錄（tar 串流）；資料庫只需唯讀帳號 | P0 | M7 |
| FR-MIG-04 | 盤點：比對舊環境的環境變數名稱、資料庫 schema、檔案目錄與解析結果並列出缺漏；貼上的 `.env` 轉為秘密變數 | P0 | M7 |
| FR-MIG-05 | 演練：把快照還原到專用遷移環境（12 字元子網域），執行驗證與資料比對；可重複多次 | P0 | M7 |
| FR-MIG-06 | 資料比對：每張表的筆數、依主鍵排序的內容雜湊、序列值；檔案清單（路徑、大小、SHA-256）。來源端由 agent 計算、目標端由平臺計算；遺失或不符列為 Blocker | P0 | M7 |
| FR-MIG-07 | 帳號驗收：帳號相關資料表（使用者、登入憑證如 WebAuthn credential）逐筆比對；hosts 預覽讓用戶以真網域與 Passkey 登入遷移環境，逐項勾選驗收清單；可另以測試帳號執行自動腳本 | P0 | M7 |
| FR-MIG-08 | 切換：提醒預先降低 TTL；寫入凍結（舊站唯讀或維護頁，可由 agent 執行用戶設定的指令）；最後同步並再次比對，全部通過才能切換；偵測 DNS 生效；切換後監控 30 分鐘（5xx 比例、登入成功數） | P0 | M7 |
| FR-MIG-09 | 回退：舊站至少保留 7 天；回退即 DNS 指回舊站；可匯出切換後新增的資料（資料庫差異、新檔案）供回寫 | P1 | M7 |
| FR-MIG-10 | 遷移報告：時間線、比對結果、驗收清單與簽核者；MCP 可讀進度與報告，執行切換需 `production` 範圍與 `confirm: true` | P1 | M7 |

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
