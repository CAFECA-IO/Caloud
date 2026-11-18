# 02 系統架構

> 版本 0.1 · 狀態：Accepted · 相關：[01 需求](01-requirements.md)、[06 API](06-api-spec.md)、[07 安全](07-security.md)、[ADR](adr/)

## 1. 需求對應

| 需求 | 對應設計 |
| --- | --- |
| NFR-OPS-01 單一執行檔 | Cargo workspace，6 個 crate，單一執行檔 |
| FR-AUTH-01 CAFECA 登入 | `caloud-auth`：Sign in with CAFECA 伺服器端驗證（EIP-712 + ERC-1271） |
| FR-AUTH-03 第一位登入者即 super admin | `db::upsert_login` 單一 `INSERT … CASE WHEN COUNT(*)=0` 陳述式 |
| FR-DOM-01～03 托管網域 | 網域 CRUD、DNS TXT 擁有權驗證、綁定 App service:port |
| FR-APP-01～07 上傳 compose 即時啟動 | `caloud-orchestrator`：policy 檢查 → override → `docker compose up -d --wait` |
| FR-DOM-04 網域轉向容器 | `caloud-proxy`：Host → upstream 路由表，ArcSwap 熱更新 |

## 2. 系統架構

```
                     ┌──────────────────────── Caloud（單一 binary）───────────────────────┐
  管理者瀏覽器 ──────►│ caloud-api (axum :8080)                                             │
   │ CafecaConnect    │   /api/auth/*  ── caloud-auth ── 公開 RPC eth_call isValidSignature │
   ▼                  │   /api/apps/*  ── caloud-orchestrator ── docker compose CLI ──┐    │
  CAFECA 錢包         │   /api/domains/* ─┐                                           │    │
  (Passkey 簽署)      │                   ▼                                           │    │
                      │   SQLite (users/sessions/apps/domains/audit_log)              │    │
                      │                   │ reload                                    │    │
  網站訪客 ──────────►│ caloud-proxy (hyper :80 / :443) ── RouteTable(ArcSwap)        │    │
                      └──────────────────────────┬────────────────────────────────────┼────┘
                                                 │ caloud-edge docker network         │ docker.sock
                                                 ▼                                    ▼
                                  caloud-<app>-<service>:<port>  ◄──────  Docker Engine
```

### Crate 職責

| Crate | 職責 | 主要依賴 |
| --- | --- | --- |
| `caloud` | 進入點，組裝 state，同時跑 API 與 proxy | tokio |
| `caloud-core` | Config、DB 連線與 migration、模型、`project_name` 命名規則、祕密加密（`secret`，FR-OPS-14） | sqlx (sqlite), chacha20poly1305 |
| `caloud-auth` | `CafecaVerifier`、nonce/token 產生 | alloy-sol-types, reqwest |
| `caloud-orchestrator` | compose 安全政策、override 產生、compose 生命週期 | serde_yaml, tokio::process |
| `caloud-proxy` | 反向代理、路由表、ACME HTTP-01 回應、`:443` SNI 分流（直通或以 `CertStore` 終結 TLS）、`:80` 轉 HTTPS、WebSocket／Upgrade 轉接、Runner 通道（`TunnelRegistry`，FR-OPS-09） | hyper 1, arc-swap, rustls, tokio-rustls, tokio-yamux, tokio-util |
| `caloud-api` | 服務層（權限、驗證、稽核）、REST API、MCP 端點、session 與 API 權杖、Admin API、Agent WebSocket 端點、憑證管理（`certs`、Cloudflare client）、內嵌 Web UI 與 Design System 靜態檔 | axum 0.8（ws）, tokio-yamux, instant-acme, rcgen |
| `caloud-analyzer` | 服務自動解析：讀儲存庫產生平臺 compose 與解析報告（信心度與依據），不執行任何程式（[ADR 0014](adr/0014-analyzer-crate.md)） | toml, serde_yaml |
| `caloud-runner` | `caloud-agent` 執行檔：規格偵測、連線控制面、心跳、通道、自我檢查、環境部署（[ADR 0012](adr/0012-runner-crate.md)、[ADR 0015](adr/0015-runner-sandbox.md)） | sysinfo, tokio-tungstenite, tokio-yamux |

### 相依規則（不可違反）

```
caloud ──► caloud-api ──► caloud-auth
                     ├──► caloud-orchestrator ──► caloud-core
                     ├──► caloud-proxy ─────────► caloud-core
                     └──► caloud-core
caloud-runner（caloud-agent）──► caloud-orchestrator ──► caloud-core
caloud-analyzer ──► caloud-orchestrator ──► caloud-core
```

- `caloud-core` 不依賴任何其他 Caloud crate，也不依賴 web 框架
- `caloud-auth`、`caloud-orchestrator`、`caloud-proxy` 彼此不互相依賴
- 只有 `caloud-api` 處理 HTTP 請求／權限；只有 `caloud` 組裝與啟動
- `caloud-analyzer` 只依賴 `caloud-orchestrator`（重用平臺 compose 政策）與 `caloud-core`；由 `caloud-api` 呼叫（M5-05）
- `caloud-runner` 只依賴 `caloud-core` 與 `caloud-orchestrator`（平臺 compose 政策與環境沙箱 override，ADR 0015）；與控制面共用的協定型別（`caloud_core::agent`）與調度函式（`caloud_core::placement`）放在 core
- 新增 crate 或改變相依方向須寫 ADR

## 3. 登入流程（Sign in with CAFECA）

依 [CAFECA README 第 4 節](https://github.com/CAFECA-IO/CAFECA) 實作，不需要向 CAFECA 註冊 client ID。

1. 前端 `GET /api/auth/config` 取得錢包網址，載入 `<wallet>/sdk/cafeca-connect.js`。
2. 點擊登入 → `POST /api/auth/nonce`：產生 128-bit nonce，存入 `login_nonces`（5 分鐘）。
3. 錢包顯示 Caloud 網域，使用者以 Passkey 簽署 EIP-712 `SignIn`。
4. `POST /api/auth/login`（原封不動送出錢包回應）：
   1. `DELETE … RETURNING` 取出並作廢 nonce（一次性）
   2. 檢查 `message.domain == CALOUD_PUBLIC_ORIGIN`、nonce、`chainId`、`expiresAt`
   3. 計算 `hashTypedData({name:"CAFECA Sign-In",version:"1",chainId,verifyingContract:account}, SignIn, message)`
   4. `eth_call account.isValidSignature(hash, signature)` 必須回傳 `0x1626ba7e`
   5. 若同意 `handle`，向 `<wallet>/api/profile?q=` 查詢代稱（回應中的自稱不可信）
5. `upsert_login`：使用者主鍵 = 身分合約地址（換裝置、恢復後不變）。
6. 簽發 256-bit session token，DB 只存雜湊；cookie `HttpOnly; SameSite=Strict; Secure`（https 時）。

> 已有測試向量與 viem `hashTypedData` 交叉比對，確保 Rust 端雜湊與官方 SDK 一致。

### 第一位登入者 = super admin

```sql
INSERT INTO users (account, handle, role, created_at, last_login)
VALUES (?1, ?2, CASE WHEN (SELECT COUNT(*) FROM users) = 0 THEN 'super_admin' ELSE ?3 END, ?4, ?4)
ON CONFLICT(account) DO UPDATE SET last_login = excluded.last_login, … RETURNING *
```

SQLite 單一寫入者 + 單一陳述式 → 不會出現兩位 super admin。部署後應**立即**登入以取得管理權。
（FR-AUTH-08，M3：`CALOUD_BOOTSTRAP_ACCOUNT` 預先指定 super admin 地址，避免搶先登入的風險。）

### 角色與權限

權限矩陣的正式定義見 [07 安全規範](07-security.md#2-權限矩陣)，以下為摘要。

| 角色 | 部署 App / 托管網域 | 管理他人資源 | 管理使用者 |
| --- | --- | --- | --- |
| super_admin（唯一，不可變更） | ✓ | ✓ | ✓（含授予 admin） |
| admin | ✓ | ✓ | ✓（member / pending / disabled） |
| member | ✓（只限自己的） | | |
| pending | 只能看到「等待核准」 | | |
| disabled | 無法登入，session 立即失效 | | |

## 4. MCP 介面

```
MCP 用戶端 ──POST /mcp (Bearer cld_…)──► mcp.rs ──► service.rs ◄── REST handlers（apps/domains/users.rs）
 (Claude Desktop / Code)                    │            │ 權限、驗證、稽核
                                            ▼            ▼
                                     api_tokens 表   ComposeRunner / RouteTable / SQLite
```

- `mcp.rs` 只做協定：解析 JSON-RPC、列出工具、把參數轉給服務層、把 `ApiError` 轉成 `isError` 結果。
- 服務層函式接收 `(&AppState, &CurrentUser, 參數)`，REST 與 MCP 共用，權限邏輯只有一份。
- `CurrentUser` 由 session cookie（REST）或 API 權杖（MCP）取得，並帶有來源 `via`（`web`／`mcp:<權杖名稱>`）寫入稽核。

## 5. App（docker compose）生命週期

```
上傳 YAML ─► policy::validate ─► apps 表 (stopped) ─► spawn_deploy
                 │ 拒絕                                    │
                 ▼                                         ▼
              400 錯誤訊息                 status=deploying
                                           寫入 data/apps/<id>/compose.yml
                                           產生 caloud.override.yml
                                           docker compose -p caloud-<id12> pull / up -d --wait
                                                   │
                                     成功 running ─┴─ 失敗 failed + last_error
                                                   │
                                           RouteTable.reload()
```

- **一個 App = 一個 compose project**，名稱 `caloud-<uuid 前 12 碼>`，彼此網路隔離（各自 default network）。
- **override** 對每個 service：
  - 加入 `caloud-edge` 網路，alias 為 `caloud-<id12>-<service>` → 代理以 `http://<alias>:<port>` 連線，**不需對主機開 port**
  - label `io.caloud.app`、`restart: unless-stopped`、預設 `mem_limit: 1g`、`pids_limit: 512`
- **安全政策**（`policy.rs`，規則以 [07 安全規範](07-security.md#3-compose-政策) 為準）拒絕：`ports`、`privileged`、`cap_add`、`devices`、`security_opt`、host namespace、bind mount、`build`、`env_file`、`container_name`、`extends`、`include`、external volume/network、`secrets/configs.file`、`volumes.driver_opts`。
- 更新 compose（`PUT /api/apps/{id}`）即重新 `up -d`，compose 自行做差異更新。
- 刪除預設保留 volume，`?purge_volumes=true` 才一併刪除資料。

## 6. 網域與路由

1. `POST /api/domains {hostname}` → 產生 `verify_token`。
2. 使用者設定 DNS：`A/AAAA` 指向主機、`TXT _caloud-challenge.<hostname> = <token>`。
3. `POST /api/domains/{hostname}/verify` → 以 DNS-over-HTTPS 查 TXT，成功則 `verified=1`。
4. `PUT /api/domains/{hostname}/bind {app_id, service, port}` → 檢查 service 存在於 compose。
5. 路由表 = `verified=1 AND app.status='running'` 的網域；任何變動後 `RouteTable::reload()`，以 `ArcSwap` 原子替換，進行中的請求不受影響。
6. 代理加上 `X-Forwarded-For/Host/Proto`；找不到路由回 404，upstream 無回應回 502。

### TLS（M4-08，[ADR 0016](adr/0016-edge-tls.md)）

- `:443`：讀 ClientHello 的 SNI → 直通網域原封轉送；`CertStore` 有憑證（完全相符或上一層 wildcard）→ rustls 終結後交給同一套 HTTP 代理（`X-Forwarded-Proto: https`）；其他關閉。
- `:80`：Caloud 的 ACME token 優先 → 有憑證的 host 回 308 轉 HTTPS → 其他照舊轉發。
- `caloud-api::certs` 背景任務：網域池（有 Cloudflare 權杖）以 DNS-01 申請 `root`＋`*.root`；已驗證、非直通、綁定運作中 App 的網域以 HTTP-01 申請。到期前 30 天續簽，失敗退避重試，結果寫入 `certificates` 與 `domains.tls_status`。
- Cloudflare 權杖以 `caloud_core::secret`（主金鑰 `CALOUD_MASTER_KEY`）加密保存，只在簽發時解開。

## 7. 資料模型

定義於 `crates/caloud-core/migrations/`，migration 只能新增、不可修改已發布的檔案。

| 資料表 | 主鍵 | 說明 | 需求 |
| --- | --- | --- | --- |
| `users` | `account`（小寫 0x 地址） | 角色、代稱、登入時間 | FR-AUTH-03/04、FR-USER-* |
| `sessions` | `token_hash` | session token 雜湊、到期時間 | FR-AUTH-05/06 |
| `login_nonces` | `nonce` | 一次性登入 nonce | FR-AUTH-02 |
| `apps` | `id`（UUID） | compose 原文、狀態、最後錯誤；來源（上傳／Git：網址、ref、目錄、檔名）、環境變數、指定 service、特權 | FR-APP-* |
| `domains` | `hostname` | 驗證 token、是否已驗證、綁定的 app/service/port、轉發目標主機、TLS 模式（一般／直通）與直通 port、TLS 狀態 | FR-DOM-* |
| `audit_log` | 自增 `id` | 操作者、動作、對象、細節、時間 | FR-AUD-01 |
| `api_tokens` | `id`（UUID） | 擁有者、名稱、token 雜湊、前綴、建立／到期／最後使用時間 | FR-MCP-02、FR-MCP-03 |
| `domain_pools` | `root` | 根網域、用途（production／staging／verify／system）、Cloudflare zone 與 zone id、權杖密文與提示、ID 長度 | FR-OPS-03、FR-OPS-14 |
| `certificates` | `name` | 憑證：類型（dns01／http01）、名稱清單、狀態（pending／issued／failed）、到期、錯誤、下次重試 | FR-OPS-10、FR-DOM-05 |
| `reserved_names` | `name` | 子網域保留字 | FR-OPS-03、FR-ENV-07 |
| `blocked_hosts` | `hostname` | 封鎖的主機名稱、理由、操作者 | FR-OPS-03 |
| `nodes` | `id`（UUID） | 名稱、擁有者（空值＝平臺節點）、偵測規格、可配置量、已配置量、狀態（active／draining／offline）、節點權杖雜湊、最後心跳 | FR-OPS-04、FR-OPS-06 |
| `node_join_tokens` | `token_hash` | 一次性加入權杖、擁有者、到期、使用時間 | FR-OPS-04 |
| `projects` | `id`（UUID） | 平臺專案：擁有者、名稱、儲存庫網址、ref、預設分支、根目錄、狀態（analyzing／needs_review／confirmed／error）、錯誤、已確認的解析 | FR-GH-04、FR-AN-06、FR-AN-10 |
| `analyses` | `id`（UUID） | 專案、ref、commit、解析結果（`caloud_analyzer::Analysis` JSON）、用戶編輯的 compose、對外服務、已確認的低信心項目、確認者與時間 | FR-AN-03、FR-AN-04、FR-AN-10 |
| `environments` | `subdomain` | 環境：主機名稱、用途（production／staging／verify）、所在節點、節點內網段索引、compose、對外服務與 port、狀態（deploying／running／failed／destroying）、錯誤、服務 IP、建立者 | FR-OPS-13 |
| `migrations`（M7） | `id`（UUID） | 專案、自訂網域、狀態（盤點／演練／凍結／已切換／已回退）、遷移權杖雜湊、快照位置、時間線 | FR-MIG-01～10 |
| `migration_checks`（M7） | 自增 `id` | 遷移、類型（資料表／檔案／帳號／驗收清單）、對象、來源值、目標值、結果、簽核者 | FR-MIG-06、FR-MIG-07 |

```
users 1 ──< sessions
users 1 ──< api_tokens
users 1 ──< apps 1 ──< domains (app_id, ON DELETE SET NULL)
users 1 ──< domains
users 1 ──< nodes（owner，自有設備）
users 1 ──< projects 1 ──< analyses（ON DELETE CASCADE）
nodes 1 ──< environments（node, subnet_index 唯一）
```

### 狀態機：App.status

```
          create(deploy=false)
  (new) ───────────────────────► stopped ◄──────── stop
    │                               │ deploy           ▲
    │ create(deploy=true)           ▼                  │
    └──────────────────────────► deploying ──成功──► running
                                    │                  │ deploy / update
                                    └──失敗──► failed ──┘ (可再 deploy)
```

### 狀態機：Domain

```
created(verified=0) ──verify 成功──► verified(verified=1) ──bind──► bound（進入路由表，若 App running）
                                                       ◄─unbind──
```

## 8. API

完整規格見 [06 API 規格](06-api-spec.md)（MCP 見 §5）。所有端點掛在 `caloud-api::router`，回應皆為 JSON（log 端點為 text/plain）。

## 9. 部署拓樸

```
Internet ──:80/:443──► [caloud 容器] ──docker.sock──► Docker Engine（主機）
                          │  data volume: /data (caloud.db, apps/, certs/)
                          └── caloud-edge 網路 ──► caloud-<id>-<svc> 容器們
管理者 ──► https://<CALOUD_PUBLIC_ORIGIN>（M1 為 :8080，M2 起經 :443）
```

- Caloud 本體以 `deploy/docker-compose.yml` 部署；它是唯一允許使用 `ports` 與 docker.sock 的 compose 專案（不經 Caloud 管理）。
- 資料目錄：`/data/caloud.db`、`/data/apps/<app_id>/{compose.yml,caloud.override.yml}`、`/data/certs/<staging|production>/<名稱>/{cert.pem,key.pem}`、`/data/acme/<staging|production>/account.json`（M4-08）。
- `.env`（不進版控）：M4-08 起需要 `CALOUD_MASTER_KEY`（`openssl rand -base64 32`）、`CALOUD_ACME_EMAIL`，可選 `CALOUD_ACME_STAGING=true`。

### 平臺拓樸（M4 起，[ADR 0010](adr/0010-platform.md)）

```
用戶／AI／GitHub ──► Edge（*.caloud.tw、自訂網域、caloud.cafeca.io）
                       ├──► 控制面（API、MCP、調度、解析、SQLite、物件儲存）
                       └──► Runner 節點 × N（平臺主機＋用戶自有設備）
Runner 的 caloud-agent ──WebSocket（主動連出）──► 控制面 /agent/v1/connect
舊主機的 caloud-agent --migrate ──（主動連出，一次性）──► 控制面（快照、雜湊、最後同步，ADR 0013）
```

Runner 主機以 `scripts/runner-setup.sh install` 準備：安裝 gVisor、在 daemon.json 註冊 `runsc`、建立 `CALOUD-ENV-*` 防火牆 chain 與開機還原服務；`uninstall` 完整移除（[ADR 0015](adr/0015-runner-sandbox.md)）。環境容器以 gVisor 執行，每個環境一個 `cld<索引>` bridge 與固定網段，Edge 經通道連到對外服務的固定 IP。

| 網域 | 用途 |
| --- | --- |
| `*.caloud.tw` | 用戶環境（8／12／16 字元子網域） |
| `caloud.cafeca.io` | 主控台、API、MCP、webhook、Agent 連線 |
| 其他 `*.cafeca.io` | 系統服務，只供系統管理者 |

## 10. 架構決策

重大決策記錄於 [adr/](adr/)：

| ADR | 決策 |
| --- | --- |
| [0001](adr/0001-compose-cli.md) | 以 `docker compose` CLI 執行部署，而非直接呼叫 Docker API |
| [0002](adr/0002-builtin-proxy.md) | 內建 Rust 反向代理，不使用 Caddy／Traefik |
| [0003](adr/0003-sqlite.md) | 使用 SQLite 作為唯一資料庫 |
| [0004](adr/0004-first-login-super-admin.md) | 以單一 SQL 陳述式決定第一位 super admin |
| [0005](adr/0005-no-build-frontend.md) | 前端無建置步驟、編譯時內嵌（單一 HTML 部分已由 0006 取代） |
| [0006](adr/0006-design-system-files.md) | Design System 拆為 `web/ds/` 多檔，經 `/assets/` 提供 |
| [0007](adr/0007-ui-style-proposals.md) | 介面風格採用「企業儀表板」 |
| [0008](adr/0008-mcp-interface.md) | MCP 介面採 Streamable HTTP 與個人 API 權杖 |
| [0009](adr/0009-system-apps.md) | Git 來源的特權 App 與 443 TLS 直通 |
| [0010](adr/0010-platform.md) | 平臺化、網域分工（caloud.tw／cafeca.io）、SQLite 暫留 |
| [0011](adr/0011-runner-websocket.md) | Runner 以 WebSocket 主動連入，之後以 yamux 多工反向通道 |
| [0012](adr/0012-runner-crate.md) | 新增 `caloud-runner` crate 與 `caloud-agent` 執行檔 |
| [0013](adr/0013-service-migration.md) | 既有服務遷移：寫入凍結、agent 遷移模式、DNS-01 委派、hosts 預覽驗收 |
| [0014](adr/0014-analyzer-crate.md) | 新增 `caloud-analyzer` crate；範本以 `dockerfile_inline` 帶入；附加服務 compose 與範本合併 |
| [0015](adr/0015-runner-sandbox.md) | Runner 環境沙箱：gVisor、固定網段＋extra_hosts 名稱解析、`cld+` 防火牆、自我檢查 |
| [0016](adr/0016-edge-tls.md) | Edge TLS 終結：`:443` SNI 分流、wildcard（DNS-01）與已驗證網域（HTTP-01）憑證、主金鑰加密 Cloudflare 權杖 |
