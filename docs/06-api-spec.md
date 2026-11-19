# 06 API 規格

> 版本 0.1 · 狀態：Accepted · 相關：[01 需求](01-requirements.md)、[02 架構](02-architecture.md)、[07 安全](07-security.md)

## 1. 通則

| 項目 | 規格 |
| --- | --- |
| Base | `CALOUD_PUBLIC_ORIGIN`，路徑前綴 `/api` |
| 格式 | 請求與回應皆為 `application/json`（UTF-8）；例外：log 端點回 `text/plain` |
| 驗證 | session cookie `caloud_session`（見 FR-AUTH-05） |
| 時間 | Unix 秒（整數） |
| 帳戶 | 小寫 `0x` 開頭 40 位十六進位 |
| 命名 | JSON 欄位 snake_case；例外：CAFECA 錢包回應依其原格式（camelCase） |
| 新增端點 | 必須先列入本文件 §3，`scripts/check-docs.sh` 會比對 `caloud-api` 的路由 |

### 1.1 錯誤格式

```json
{ "error": "人看得懂的訊息" }
```

| HTTP | 意義 | 何時使用 |
| --- | --- | --- |
| 400 | Bad Request | 驗證失敗、格式錯誤、compose 不合規、登入驗證失敗 |
| 401 | Unauthorized | 未登入或 session 過期 |
| 403 | Forbidden | 角色不足、非資源擁有者、帳戶停用 |
| 404 | Not Found | 資源不存在 |
| 409 | Conflict | 重複建立（網域已存在）、部署進行中（M2） |
| 500 | Internal | 非預期錯誤，訊息固定為「伺服器錯誤」（NFR-SEC-03） |

### 1.2 權限標記

| 標記 | 條件 |
| --- | --- |
| 公開 | 不需登入 |
| 登入 | 任一有效 session（含 pending） |
| member+ | `Role::can_deploy()`：super_admin／admin／member |
| admin+ | `Role::is_admin()`：super_admin／admin |
| 擁有者 | 資源 `owner` 為自己，或為 admin+ |

## 2. 資料型別

### User

```json
{ "account": "0xabc…", "handle": "alice", "role": "super_admin", "created_at": 1762650000, "last_login": 1762650000 }
```

`role`：`super_admin` | `admin` | `member` | `pending` | `disabled`

### App

```json
{ "id": "uuid", "name": "blog", "owner": "0x…", "status": "running", "last_error": null, "created_at": 0, "updated_at": 0,
  "source": "git", "git_url": "https://github.com/CAFECA-IO/SwarmStorage", "git_ref": "main", "git_path": "deploy/livekit",
  "compose_file": "docker-compose.yml", "services": ["caddy", "livekit"], "privileged": true, "env_keys": ["LIVEKIT_DOMAIN", "TURN_DOMAIN"] }
```

`status`：`stopped` | `deploying` | `running` | `failed`。`source`：`compose`（上傳）| `git`（FR-APP-13）。`services` 為 null 時啟動全部（FR-APP-15）。`env_keys` 只列變數名稱，**不回傳值**（FR-APP-14）。`compose_yaml` 不在列表回應中出現，只在單筆查詢回傳；Git 來源 App 為最近一次取得的內容。

### Domain

```json
{ "hostname": "blog.example.com", "owner": "0x…", "verify_token": "caloud-…", "verified": true,
  "app_id": "uuid", "service": "web", "port": 80, "tls_status": "none", "tls_mode": "none", "tls_port": null, "created_at": 0 }
```

`tls_status`：`none` | `pending` | `issued` | `failed`，由憑證管理實際更新（FR-DOM-05：已驗證、非直通、綁定運作中 App 的網域才會申請）。`tls_mode`：`none`（HTTP 轉發）| `passthrough`（443 TLS 直通到 `tls_port`，FR-DOM-09）。

### ContainerStatus

```json
{ "Service": "web", "Name": "caloud-…-web-1", "State": "running", "Health": "healthy" }
```

（沿用 `docker compose ps --format json` 欄位名稱。）

## 3. 端點

### 3.1 身分驗證

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/auth/config` | 公開 | FR-AUTH-01 |
| POST | `/api/auth/nonce` | 公開 | FR-AUTH-02 |
| POST | `/api/auth/login` | 公開 | FR-AUTH-01～05 |
| POST | `/api/auth/logout` | 公開 | FR-AUTH-06 |
| GET | `/api/me` | 登入 | — |

**GET /api/auth/config** → `200 { "wallet": "https://cafeca.io", "domain": "<PUBLIC_ORIGIN>" }`

**POST /api/auth/nonce** → `200 { "nonce": "22 字元 base64url" }`

**POST /api/auth/login**
- 請求：CAFECA 錢包 `signIn()` 回傳的物件，原封不動
- 回應：`200 User`，並 `Set-Cookie: caloud_session=…; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800[; Secure]`
- 錯誤：`400`（nonce 無效／過期、網域不符、chainId 不符、過期、簽章無效、使用者拒絕）；`403`（帳戶停用）

**POST /api/auth/logout** → `200`，清除 cookie 並刪除 session

**GET /api/me** → `200 User`；未登入 `401`

### 3.2 使用者

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/users` | admin+ | FR-USER-01 |
| PUT | `/api/users/{account}` | admin+ | FR-USER-02、FR-USER-03 |

**PUT /api/users/{account}** 請求 `{ "role": "member" }` → `200 User`。
`403`：目標為 super_admin、要授予 super_admin、非 super_admin 授予／撤銷 admin、修改自己。設為 `disabled` 時刪除其所有 session。

### 3.3 網域

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/domains` | member+ | FR-DOM-01（admin+ 看全部，其餘看自己的） |
| POST | `/api/domains` | member+ | FR-DOM-01 |
| DELETE | `/api/domains/{hostname}` | 擁有者 | FR-DOM-01 |
| POST | `/api/domains/{hostname}/verify` | 擁有者 | FR-DOM-02 |
| PUT | `/api/domains/{hostname}/bind` | 擁有者（且為 App 擁有者） | FR-DOM-03 |

**POST /api/domains** `{ "hostname": "Blog.Example.com." }` → `201 Domain`（hostname 已正規化）；`400` 格式錯誤；`409` 已存在

**POST /api/domains/{hostname}/verify** → `200 Domain`；`400`「找不到 TXT 紀錄 _caloud-challenge.<host> = <token>」

**PUT /api/domains/{hostname}/bind** `{ "app_id": "uuid" | null, "service": "web", "port": 80, "tls_mode": "none" | "passthrough", "tls_port": 443 }` → `200 Domain`。`tls_mode: passthrough` 時 `tls_port` 必填（FR-DOM-09）。`app_id: null` 表示解除綁定。`400`：缺 service／port、service 不在 compose 中。

### 3.4 App

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/apps` | member+ | FR-APP-01（admin+ 看全部） |
| POST | `/api/apps` | member+ | FR-APP-01～03 |
| GET | `/api/apps/{id}` | 擁有者 | FR-APP-06 |
| PUT | `/api/apps/{id}` | 擁有者 | FR-APP-04 |
| DELETE | `/api/apps/{id}` | 擁有者 | FR-APP-05 |
| POST | `/api/apps/{id}/deploy` | 擁有者（member+） | FR-APP-03 |
| POST | `/api/apps/{id}/stop` | 擁有者 | FR-APP-05 |
| GET | `/api/apps/{id}/logs` | 擁有者 | FR-APP-06 |

**POST /api/apps** → `201 App`（`deploy` 預設 true，背景執行）；`400` compose 不合規（訊息指出 service 與欄位）、Git 取得失敗；`403` 非 Admin 使用 Git 來源或特權。

| 欄位 | 型別 | 說明 |
| --- | --- | --- |
| `name` | string? | 省略時以第一個 service 命名 |
| `source` | `"compose"` \| `"git"` | 預設 `compose` |
| `compose_yaml` | string | `source: compose` 時必填 |
| `git_url` | string | `source: git` 時必填；`https://` 公開儲存庫（admin+） |
| `git_ref` | string? | 分支、標籤或 commit，預設 `main` |
| `git_path` | string? | compose 所在目錄（相對儲存庫根目錄），預設根目錄 |
| `compose_file` | string? | 預設 `docker-compose.yml` |
| `services` | string[]? | 只啟動這些 service（FR-APP-15） |
| `env` | string? | `.env` 內容（`KEY=value` 每行一個，FR-APP-14） |
| `privileged` | bool? | 特權 App（admin+，FR-APP-16），預設 false |
| `deploy` | bool? | 預設 true |

**GET /api/apps/{id}** → `200 { "app": App, "compose_yaml": "…", "services": ["web","db"], "containers": [ContainerStatus] }`

**PUT /api/apps/{id}** 欄位同 POST，省略的欄位維持原值（`env` 省略表示不變）；Git 來源 App 會重新取得指定 ref → `200 App`

**DELETE /api/apps/{id}?purge_volumes=false** → `204`

**POST /api/apps/{id}/deploy** → `202`；**POST /api/apps/{id}/stop** → `204`

**GET /api/apps/{id}/logs?service=web&tail=200** → `200 text/plain`；`tail` 上限 5000

### 3.5 API 權杖

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/tokens` | member+ | FR-MCP-02（只列自己的） |
| POST | `/api/tokens` | member+ | FR-MCP-02 |
| DELETE | `/api/tokens/{id}` | 擁有者 | FR-MCP-02 |

**ApiToken**

```json
{ "id": "uuid", "name": "Claude Desktop", "prefix": "cld_AbCd", "created_at": 0, "expires_at": 0, "last_used_at": null }
```

**POST /api/tokens** `{ "name": "Claude Desktop", "expires_in_days": 90 }` → `201 { "token": "cld_…", "api_token": ApiToken }`。`token` 只在此回應出現一次。`name` 1–60 字；`expires_in_days` 1–365，預設 90。`403`：pending。

**DELETE /api/tokens/{id}** → `204`；非自己的權杖回 `404`。

### 3.6 後台（Admin API）

平臺後台（[ADR 0010](adr/0010-platform.md)）。全部需要 admin+，寫入動作皆寫稽核。

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/admin/domain-pools` | admin+ | FR-OPS-03 |
| POST | `/api/admin/domain-pools` | admin+ | FR-OPS-03 |
| PUT | `/api/admin/domain-pools/{root}` | admin+ | FR-OPS-03 |
| DELETE | `/api/admin/domain-pools/{root}` | admin+ | FR-OPS-03 |
| GET | `/api/admin/reserved-names` | admin+ | FR-OPS-03 |
| POST | `/api/admin/reserved-names` | admin+ | FR-OPS-03 |
| DELETE | `/api/admin/reserved-names/{name}` | admin+ | FR-OPS-03 |
| GET | `/api/admin/blocked-hosts` | admin+ | FR-OPS-03 |
| POST | `/api/admin/blocked-hosts` | admin+ | FR-OPS-03 |
| DELETE | `/api/admin/blocked-hosts/{hostname}` | admin+ | FR-OPS-03 |
| POST | `/api/admin/subdomain-ids` | admin+ | FR-ENV-07 |
| GET | `/api/admin/nodes` | admin+ | FR-OPS-04 |
| POST | `/api/admin/nodes/join-tokens` | admin+ | FR-OPS-04 |
| POST | `/api/admin/nodes/{id}/drain` | admin+ | FR-OPS-06 |
| POST | `/api/admin/nodes/{id}/activate` | admin+ | FR-OPS-06 |
| DELETE | `/api/admin/nodes/{id}` | admin+ | FR-OPS-06 |
| GET | `/api/admin/environments` | admin+ | FR-OPS-13 |
| POST | `/api/admin/environments` | admin+ | FR-OPS-13 |
| DELETE | `/api/admin/environments/{subdomain}` | admin+ | FR-OPS-13 |
| GET | `/api/admin/certificates` | admin+ | FR-OPS-10、FR-DOM-05 |
| GET | `/api/admin/github-app` | admin+ | FR-GH-05 |
| PUT | `/api/admin/github-app` | admin+ | FR-GH-05 |

**DomainPool**

```json
{ "root": "caloud.tw", "purposes": ["production", "staging", "verify"], "cloudflare_zone": "caloud.tw",
  "cloudflare_token_hint": "…a1b2",
  "id_lengths": { "production": 8, "staging": 12, "verify": 16 }, "created_at": 0, "updated_at": 0 }
```

- `root`：主機名稱格式，正規化為小寫。`purposes` 為 `production`、`staging`、`verify`、`system` 的子集，至少一個；`production`、`staging`、`verify` 各自只能屬於一個網域池（重複回 `409`）。
- `id_lengths` 每項 6–32，省略時為 8／12／16。`cloudflare_zone` 可為 null。
- `cloudflare_token`（只寫，FR-OPS-03、FR-OPS-14）：POST／PUT 可帶。省略表示不變；空字串表示清除。
  - 有值時必須同時有 `cloudflare_zone`。Caloud 會以該權杖呼叫 Cloudflare `GET /zones?name=<zone>`，查無 zone 或權杖無效回 `400`。
  - 驗證通過後以主金鑰加密保存；未設定 `CALOUD_MASTER_KEY` 回 `400`。
  - 回應只會出現 `cloudflare_token_hint`（末 4 碼，未設定時為 null）。
  - 設定權杖後，Caloud 會為 `root` 與 `*.root` 申請 wildcard 憑證（FR-OPS-10），進度見 `GET /api/admin/certificates`。
- **PUT** 本體同 POST 但不含 `root`；**DELETE** → `204`。

**Certificate**（`GET /api/admin/certificates` 回傳陣列，依 `name` 排序，ADR 0016）

```json
{ "name": "caloud.tw", "kind": "dns01", "names": ["caloud.tw", "*.caloud.tw"], "status": "issued",
  "not_after": 1800000000, "error": null, "next_attempt": null, "staging": false, "updated_at": 0 }
```

- `kind`：`dns01`（網域池）| `http01`（已驗證網域）。`status`：`pending` | `issued` | `failed`。
- `failed` 時 `error` 是最後一次的錯誤訊息，`next_attempt` 是下次重試時間（退避：1 小時起、最多 24 小時）。
- `staging` 表示目前使用 Let's Encrypt staging（`CALOUD_ACME_STAGING=true`），瀏覽器不信任這類憑證。

**ReservedName** `{ "name": "www", "created_at": 0 }`。**POST** `{ "name": "support" }`：1–32 字元，`a-z`、`0-9`、`-`，轉為小寫；重複回 `409`。3 字元以上的保留字連「包含」它的 ID 也排除，較短的只排除完全相同者。預設保留：`www`、`app`、`api`、`mcp`、`docs`、`status`、`admin`、`login`、`cafeca`、`caloud`。

**BlockedHost** `{ "hostname": "q7v2m4kx.caloud.tw", "reason": "檢舉：仿冒登入頁", "created_by": "0x…", "created_at": 0 }`。**POST** `{ "hostname", "reason" }`：`reason` 1–200 字。

**POST /api/admin/subdomain-ids** `{ "kind": "production" }` → `200 { "id": "jo4f8rk4", "hostname": "jo4f8rk4.caloud.tw" }`：依該用途的網域池產生一個目前可用的 ID（FR-ENV-07），**不保留**；沒有對應網域池回 `400`。

**Node**

```json
{ "id": "uuid", "name": "runner-tky-01", "owner": null, "status": "active",
  "specs": { "cpu_cores": 16, "arch": "x86_64", "memory_bytes": 68719476736, "disk_free_bytes": 1932735283200,
             "os": "Ubuntu 24.04", "gvisor": true, "buildkit": true, "isolation": true, "self_check": null, "agent_version": "0.1.0" },
  "allocatable": { "cpu_millis": 14400, "memory_bytes": 61847529062 },
  "allocated": { "cpu_millis": 0, "memory_bytes": 0 },
  "used": { "cpu_millis": 820, "memory_bytes": 2147483648 },
  "last_seen": 0, "created_at": 0 }
```

- `status`：`active` | `draining` | `offline`。`owner` 為 null 表示平臺節點（自有設備見 M7）。
- `allocatable` 為偵測規格保留 10% 給主機後的可配置量，由控制面計算（FR-OPS-08）；`allocated` 為調度已配置量；`used` 為最近一次心跳回報的主機用量。

**POST /api/admin/nodes/join-tokens** `{ "expires_in_minutes": 60 }` → `201 { "token": "caloud_join_…", "expires_at": 0 }`：一次性加入權杖，只在此回應出現；`expires_in_minutes` 1–60，預設 60。

**POST /api/admin/nodes/{id}/drain** → `200 Node`（`draining`，不再接新工作，FR-OPS-06）；**/activate** → `200 Node`（`active`，節點須在線，離線回 `409`）。

**DELETE /api/admin/nodes/{id}** → `204`；只有 `draining` 或 `offline` 的節點可移除，否則 `409`。移除後該節點權杖失效。

- `specs.isolation`：agent 自我檢查通過（FR-OPS-12，[ADR 0015](adr/0015-runner-sandbox.md)）；`specs.self_check` 為未通過的原因（通過時為 null）。平臺環境只調度到 `isolation: true` 的平臺節點。

**Environment**（FR-OPS-13）

```json
{ "subdomain": "k2m9xq7a4tz1ab3c", "hostname": "k2m9xq7a4tz1ab3c.caloud.tw", "kind": "verify",
  "node": "uuid", "subnet_index": 0, "status": "running", "error": null,
  "public_service": "web", "public_port": 80, "services": [ { "name": "web", "ip": "10.240.0.10" } ],
  "created_by": "0x…", "created_at": 0, "updated_at": 0 }
```

- `status`：`deploying` → `running` | `failed`；刪除時 `destroying`，agent 回報後紀錄移除。
- **POST** `{ "kind", "compose_yaml", "public_service", "public_port", "node_id"? }` → `201 Environment`（`deploying`）：
  - `kind`：`production` | `staging` | `verify`；子網域 ID 依該用途的網域池產生（FR-ENV-07）。
  - `compose_yaml` 以平臺政策（[07 §3.6](07-security.md#36-平臺-app-政策)）檢查；M4 只接受 image，含 `build` 回 `400`。
  - `public_service` 必須在 compose 中；`public_port` 1–65535。
  - 依可配置量選擇 `isolation: true` 的平臺節點（可指定 `node_id`）；沒有可用節點回 `409`；沒有對應網域池回 `400`。
  - 調度時每個服務保留 250 millicores、512 MiB，執行上限為 1 核、1 GB、512 個 process（ADR 0015）。
- **DELETE** → `202`；節點在線時送出 `destroy`，否則直接刪除紀錄。
- 環境為 `running` 時，Edge 將 `hostname` 經該節點的通道轉發到對外服務的固定 IP 與 port（M4-07）。

### 3.7 專案（平臺）

平臺專案（[ADR 0010](adr/0010-platform.md)）：以公開儲存庫網址建立，背景取得並解析（[ADR 0014](adr/0014-analyzer-crate.md)），用戶在確認頁確認後才進入驗證（M6）。member+ 可建立，擁有者或 admin+ 可管理；寫入動作皆寫稽核。

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/projects` | member+ | FR-GH-04（admin+ 看全部） |
| POST | `/api/projects` | member+ | FR-GH-04、FR-AN-06 |
| GET | `/api/projects/{id}` | 擁有者 | FR-AN-04 |
| PATCH | `/api/projects/{id}` | 擁有者 | FR-AN-06 |
| DELETE | `/api/projects/{id}` | 擁有者 | FR-AN-10 |
| POST | `/api/projects/{id}/analysis` | 擁有者 | FR-AN-10 |
| PUT | `/api/projects/{id}/analysis` | 擁有者 | FR-AN-03、FR-AN-04 |

**Project**

```json
{ "id": "uuid", "owner": "0x…", "name": "SwarmStorage", "repo_url": "https://github.com/CAFECA-IO/SwarmStorage",
  "git_ref": null, "branch": "main", "root": "", "status": "needs_review", "error": null,
  "confirmed_analysis": null, "created_at": 0, "updated_at": 0 }
```

- `status`：`analyzing` → `needs_review` | `error`；確認後 `confirmed`。重新解析時回到 `analyzing`，完成後若已有確認版本則回 `confirmed`，否則 `needs_review`。
- `git_ref` 為 null 表示儲存庫的預設分支，`branch` 為解析時取得的分支名稱。

**POST /api/projects** `{ "repo_url", "git_ref"?, "root"?, "name"? }` → `202 Project`（背景解析）。`repo_url` 須為 `https://` 且不含帳密；`git_ref`、`root` 規則同 §3.4 的 `git_ref`、`git_path`；`name` 省略時取儲存庫名稱（1–64 字）。超過 20 個專案回 `409`。

**GET /api/projects/{id}** → `200 { "project": Project, "analysis": Analysis | null, "confirmed": Analysis | null }`，`analysis` 為最新一筆：

```json
{ "id": "uuid", "git_ref": "HEAD", "commit": "98b705d…", "created_at": 0,
  "result": { "source": { "kind": "template", "template": "node/next" }, "root": "", "compose_dir": "",
              "compose": "services: …", "services": [ { "name": "web", "role": "app", "port": 3000, "public": true } ],
              "env": [ { "name": "PORT", "provider": "platform", "evidence": [], "build_time": false, "varies_by_env": false } ],
              "findings": [ { "key": "port", "message": "…", "confidence": "high", "evidence": [ { "file": "package.json", "line": 23 } ] } ],
              "errors": [], "needs_confirmation": false },
  "compose": null, "public_service": null, "acknowledged": [], "confirmed_by": null, "confirmed_at": null }
```

- `result` 為 `caloud_analyzer::Analysis`；`compose` 為用戶編輯版（null 表示使用 `result.compose`）。
- `env[].build_time`：建置時寫入前端的變數（FR-AN-09）；`env[].varies_by_env`：值隨環境不同（網址、origin 類），staging 與 production 需各自建置。舊紀錄缺少時視為 false。
- M5-08 新增的 finding `key`：`persistent_dir`（FR-AN-07）、`system_package`、`native_module`（FR-AN-08）、`build_env`（FR-AN-09）。

**PATCH /api/projects/{id}** `{ "name"?, "git_ref"?, "root"? }` → `200 Project`；`git_ref` 或 `root` 改變時重新解析（`git_ref` 給空字串表示預設分支）。

**DELETE /api/projects/{id}** → `204`；同時刪除解析紀錄與取得的原始碼。

**POST /api/projects/{id}/analysis** → `202 Project`：以目前設定重新取得並解析；解析中回 `409`。新結果另存一筆，已確認的版本不變（FR-AN-10）。

**PUT /api/projects/{id}/analysis** `{ "compose"?, "public_service", "acknowledged": [0, 3] }` → `200 Project`（`confirmed`），確認最新一筆解析：

- `acknowledged`：`result.findings` 中 `confidence: "low"` 的索引必須全部列出，否則 `400` 並列出缺少的項目（FR-AN-04）。
- `compose`：用戶編輯版，以平臺政策（[07 §3.6](07-security.md#36-平臺-app-政策)）檢查，`ports`、`env_file` 也不接受；不送時使用 `result.compose`，若為 null（解析有阻擋項）則必須送。
- `public_service`：必須是 compose 中的服務。
- 解析中或沒有解析結果回 `409`。

### 3.8 GitHub（ADR 0017）

| Method | Path | 權限 | 需求 |
| --- | --- | --- | --- |
| GET | `/api/github/install` | member+ | FR-GH-01 |
| GET | `/api/github/callback` | member+（session） | FR-GH-01 |
| GET | `/api/github/installations` | member+ | FR-GH-01 |
| GET | `/api/github/installations/{id}/repositories` | 安裝擁有者 | FR-GH-01 |
| POST | `/api/github/webhook` | GitHub（HMAC） | FR-GH-02 |

**GitHubApp**（`GET／PUT /api/admin/github-app`；未設定時 GET 回 `{ "configured": false }`）

```json
{ "configured": true, "app_id": 5826601, "slug": "caloud", "client_id": "Iv23li…",
  "client_secret_hint": "…a1b2", "private_key_hint": "…MII=", "webhook_secret_hint": "…00ca", "updated_at": 0 }
```

- **PUT** `{ "app_id", "client_id", "client_secret"?, "private_key"?, "webhook_secret"? }`：省略的機密維持原值；首次設定三者都必須提供。以私鑰簽 JWT 呼叫 GitHub `GET /app` 確認 App ID 相符並取得 slug，失敗回 `400`；未設定 `CALOUD_MASTER_KEY` 回 `400`。
- **GET /api/github/install** → `200 { "url": "https://github.com/apps/<slug>/installations/new?state=…" }`；state 一次性、綁定 session、10 分鐘有效。App 未設定回 `409`。
- **GET /api/github/callback** `?code&installation_id&state&setup_action`：驗證 state、交換 user token、確認安裝屬於該 GitHub 使用者後綁定，`302` 回 `/#projects`（失敗時 `/#projects?github_error=<訊息>`）。安裝已綁定到其他帳號為錯誤。
- **Installation** `{ "id": 1234, "account_login": "CAFECA-IO", "account_type": "Organization", "suspended": false, "created_at": 0 }`；`GET /api/github/installations` 列出自己的（admin+ 看全部）。
- **GET /api/github/installations/{id}/repositories** → `200 [ { "full_name": "CAFECA-IO/iSunFA", "private": true, "default_branch": "main" } ]`。
- **POST /api/github/webhook**：`X-Hub-Signature-256` 錯誤或缺少回 `401`；本體超過 5 MB 回 `413`；同一 `X-GitHub-Delivery` 重送回 `200` 但不重複處理。push、create（branch）、pull_request（opened／synchronize／reopened）使相符專案重新解析；installation deleted 刪除安裝、suspend／unsuspend 更新狀態。
- **POST /api/projects** 另可帶 `{ "installation_id", "repo_full_name" }` 以安裝匯入私有儲存庫（取代 `repo_url`；安裝必須屬於自己）。Project 回應增加 `installation_id`、`repo_full_name`。

**對外防護（M2-07）**：`/api/**` 寫入請求的 `Origin` 不等於 public origin 時回 `403`；超過速率限制回 `429`（附 `Retry-After`）：`/api/auth/nonce`、`/api/auth/login` 每 IP 每分鐘 20 次、webhook 300 次、其他寫入 120 次。

## 4. 非 API 路徑

| Path | 說明 |
| --- | --- |
| `GET /` | 管理介面（內嵌 `web/index.html`） |
| `GET /styleguide` | Design System 元件總覽（內嵌 `web/styleguide.html`，FR-UI-12） |
| `POST /mcp` | MCP 端點（§5） |
| `GET /agent/v1/connect` | Runner agent 的 WebSocket 連線（§6，[ADR 0011](adr/0011-runner-websocket.md)） |
| `GET /agent/v1/tunnel` | Runner 通道：Edge 經此轉發到節點上的環境（§6.1） |
| `GET /assets/{*path}` | Design System 靜態檔，只提供 `caloud-api::assets::FILES` 白名單：`ds/tokens.css`、`ds/components.css`、`ds/theme.css`、`ds/ui.js`、`ds/logo.svg`（品牌標誌，04 §4.1）；其餘回 404 |
| 代理 `:80` `/.well-known/acme-challenge/{token}` | ACME HTTP-01 回應（M2） |
| 代理 `:80` 其他路徑 | 依 Host 轉發至容器（FR-DOM-04、FR-DOM-07）；路由帶節點時經該節點通道（FR-OPS-09）；WebSocket／Upgrade 雙向轉接（FR-DOM-06） |
| 代理 `:443` | 依 TLS SNI：直通網域轉發整條 TCP 連線到 `tls_port`（FR-DOM-09）；有 Edge 憑證的網域終結 TLS 後依 Host 轉發（ADR 0016）；`CALOUD_PUBLIC_ORIGIN` 的 host 轉到管理介面本身（ADR 0017）；其他關閉 |

## 5. MCP 介面

依 [ADR 0008](adr/0008-mcp-interface.md)。

### 5.1 傳輸與驗證

| 項目 | 規格 |
| --- | --- |
| 端點 | `POST /mcp`，`Content-Type: application/json`，本文為單一 JSON-RPC 2.0 訊息（不支援批次） |
| 回應 | request → `200 application/json`；notification → `202` 無本文；`GET`／`DELETE /mcp` → `405` |
| 協定版本 | `2025-11-25`、`2025-06-18`、`2025-03-26`；`initialize` 時回應用戶端要求的版本（若支援），否則 `2025-11-25` |
| 驗證 | `Authorization: Bearer cld_…`（§3.5）；缺少或無效 → `401` 並帶 `WWW-Authenticate: Bearer`；不接受 cookie |
| Origin | 帶有 `Origin` 且不等於 `CALOUD_PUBLIC_ORIGIN` → `403` |
| 能力 | `initialize` 回傳 `capabilities: { tools: { listChanged: false } }`、`serverInfo: { name: "caloud", version }` 與使用說明 `instructions` |
| 方法 | `initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`；其他 → `-32601` |

### 5.2 錯誤

| 情況 | 回應 |
| --- | --- |
| JSON 無法解析 | JSON-RPC `-32700` |
| 不是合法的 JSON-RPC 請求 | `-32600` |
| 未知方法 | `-32601` |
| 未知工具、參數缺漏或型別錯誤 | `-32602` |
| 工具執行失敗（權限不足、驗證失敗、找不到資源…） | 正常結果，`isError: true`，`content[0].text` 為與 REST 相同的錯誤訊息 |

### 5.3 工具

成功時回傳 `content: [{ type: "text", text: <JSON 字串> }]` 與相同內容的 `structuredContent`。標記：R = `readOnlyHint`、D = `destructiveHint`、I = `idempotentHint`。

| 工具 | 參數 | 回傳 | 標記 | 對應 REST | 權限 |
| --- | --- | --- | --- | --- | --- |
| `whoami` | — | User | R | `GET /api/me` | member+ |
| `list_apps` | `status?` | `{ apps: [App] }` | R | `GET /api/apps` | member+ |
| `get_app` | `app_id` | 同 `GET /api/apps/{id}` | R | `GET /api/apps/{id}` | 擁有者 |
| `create_app` | `compose_yaml`、`name?`、`env?`、`services?`、`deploy?`（預設 true） | App | — | `POST /api/apps` | member+ |
| `create_git_app` | `git_url`、`git_ref?`、`git_path?`、`compose_file?`、`name?`、`env?`、`services?`、`privileged?`、`deploy?` | App | — | `POST /api/apps`（`source: git`） | admin+ |
| `update_app` | `app_id`、`compose_yaml?`、`git_ref?`、`name?`、`env?`、`services?`、`deploy?` | App | — | `PUT /api/apps/{id}` | 擁有者（Git 來源需 admin+） |
| `deploy_app` | `app_id` | `{ app_id, status: "deploying" }` | I | `POST /api/apps/{id}/deploy` | 擁有者 |
| `stop_app` | `app_id` | `{ app_id, status: "stopped" }` | I | `POST /api/apps/{id}/stop` | 擁有者 |
| `delete_app` | `app_id`、`confirm: true`、`purge_volumes?` | `{ deleted: app_id }` | D | `DELETE /api/apps/{id}` | 擁有者 |
| `get_app_logs` | `app_id`、`service?`、`tail?`（預設 200，上限 5000） | `{ logs }` | R | `GET /api/apps/{id}/logs` | 擁有者 |
| `list_domains` | — | `{ domains: [Domain] }` | R | `GET /api/domains` | member+ |
| `create_domain` | `hostname` | Domain ＋ `dns_records`（需設定的 A 與 TXT） | — | `POST /api/domains` | member+ |
| `verify_domain` | `hostname` | Domain | I | `POST /api/domains/{hostname}/verify` | 擁有者 |
| `bind_domain` | `hostname`、`app_id?`（null 解除）、`service?`、`port?`、`tls_passthrough_port?` | Domain | I | `PUT /api/domains/{hostname}/bind` | 擁有者 |
| `delete_domain` | `hostname`、`confirm: true` | `{ deleted: hostname }` | D | `DELETE /api/domains/{hostname}` | 擁有者 |
| `list_users` | — | `{ users: [User] }` | R | `GET /api/users` | admin+ |
| `set_user_role` | `account`、`role` | User | I | `PUT /api/users/{account}` | admin+ |

- 刪除類工具未傳 `confirm: true` → `isError: true`，訊息提示需確認（FR-MCP-05）。
- `tools/list` 對 member 不列出 admin 專用工具。
- 每個工具都有 `title`、`description`（繁體中文，說明用途與注意事項）與 `inputSchema`（JSON Schema，`additionalProperties: false`）。

## 6. Agent 協定

`GET /agent/v1/connect` 升級為 WebSocket；訊息為 JSON 文字訊框，以 `type` 區分。連線不帶 cookie，身分只看第一則 `register`（FR-OPS-04）。

**Agent → 控制面**

| type | 欄位 | 說明 |
| --- | --- | --- |
| `register` | `join_token` 或 `node_token`（擇一）、`name`、`specs`（同 Node.specs） | 必須是第一則，10 秒內未收到即關閉。首次以加入權杖註冊；之後以節點權杖重連 |
| `heartbeat` | `used`（`cpu_millis`、`memory_bytes`，主機目前用量） | 每 `heartbeat_secs` 秒一次；第一則在 `ack` 後立即送出 |
| `env_status` | `env_id`、`status`（`running`／`failed`／`destroyed`）、`error`?、`services`（`[{name, ip}]`） | 部署或刪除的結果 |

**控制面 → Agent**

| type | 欄位 | 說明 |
| --- | --- | --- |
| `ack` | `node_id`、`node_token`（只在以加入權杖註冊時出現）、`status`、`heartbeat_secs` | 註冊成功 |
| `status` | `status` | 維運者排空或恢復節點時通知 |
| `deploy` | `env`（`id`、`kind`、`compose`、`subnet_index`、`public_service`） | 部署或重新部署環境（FR-OPS-13）；agent 以平臺政策再檢查 compose |
| `destroy` | `env_id` | 停止並刪除環境與其 volume |
| `error` | `message` | 隨即關閉連線（權杖無效、已使用、已過期、格式錯誤、節點被移除）；agent 收到後不再重連 |

- 加入權杖 `caloud_join_…`：一次性、最長 1 小時、只存 keccak 雜湊；使用後立即作廢，不論成功與否。
- 節點權杖 `caloud_node_…`：只存雜湊；節點被移除即失效。agent 存於 `~/.caloud-agent/node.json`（權限 0600）。
- 以節點權杖重連時沿用原節點，更新規格；狀態若為 `offline` 改回 `active`（`draining` 維持）。
- 控制面每 15 秒把超過 60 秒沒有心跳的節點標為 `offline`。

### 6.1 通道（M4-07）

`GET /agent/v1/tunnel`，標頭 `Authorization: Bearer <節點權杖>`，升級為 WebSocket 後只用 binary frame，內容為 yamux（控制面為 client、agent 為 server）。權杖無效回 `401`，不升級。

| 步驟 | 方向 | 內容 |
| --- | --- | --- |
| 1 | 控制面 → agent | 開新 stream，送 `CONNECT <host>:<port>\n`（host 為環境的服務別名或 IP） |
| 2 | agent → 控制面 | `OK\n`，或 `ERR <原因>\n` 後關閉 stream |
| 3 | 雙向 | 原始位元組（HTTP、WebSocket、TLS 直通皆同） |

- agent 拒絕：雲端 metadata（`169.254.0.0/16`）、迴圈位址以外的控制面主機、格式錯誤的目標。
- 同一節點重連時以新通道取代舊通道；節點被移除時通道立即中斷，Edge 對其路由回 `502`。

### 6.2 遷移模式（草案，M7-12 實作時定稿）

`caloud-agent --migrate --join <遷移權杖>` 在舊主機執行（[ADR 0013](adr/0013-service-migration.md)）。沿用同一端點與訊框格式，註冊後改用以下訊息：

| 方向 | type | 用途 |
| --- | --- | --- |
| agent → 控制面 | `migrate_hello` | 遷移權杖、可用工具（`pg_dump`、`mysqldump`、`tar`）與版本 |
| 控制面 → agent | `migrate_task` | 要執行的工作：`snapshot`（資料庫連線字串、目錄清單）、`manifest`（只算雜湊）、`freeze`（執行用戶設定的凍結指令） |
| agent → 控制面 | `dump_chunk` | 快照分塊（另開 yamux stream 傳輸二進位資料） |
| agent → 控制面 | `manifest` | 每張表的筆數、內容雜湊、序列值；檔案的路徑、大小、SHA-256 |
| agent → 控制面 | `task_done` | 工作結果與錯誤 |

REST 路徑（建立遷移、演練、比對、驗收清單、切換、回退）於 M7-11 實作時寫入 §3。
