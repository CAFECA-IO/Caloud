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
{ "id": "uuid", "name": "blog", "owner": "0x…", "status": "running", "last_error": null, "created_at": 0, "updated_at": 0 }
```

`status`：`stopped` | `deploying` | `running` | `failed`。`compose_yaml` 不在列表回應中出現，只在單筆查詢回傳。

### Domain

```json
{ "hostname": "blog.example.com", "owner": "0x…", "verify_token": "caloud-…", "verified": true,
  "app_id": "uuid", "service": "web", "port": 80, "tls_status": "none", "created_at": 0 }
```

`tls_status`：`none` | `pending` | `issued` | `failed`

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

**PUT /api/domains/{hostname}/bind** `{ "app_id": "uuid" | null, "service": "web", "port": 80 }` → `200 Domain`。`app_id: null` 表示解除綁定。`400`：缺 service／port、service 不在 compose 中。

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

**POST /api/apps** `{ "name": "blog" | null, "compose_yaml": "services: …", "deploy": true }` → `201 App`（`deploy` 預設 true，背景執行）；`400` compose 不合規（訊息指出 service 與欄位）

**GET /api/apps/{id}** → `200 { "app": App, "compose_yaml": "…", "services": ["web","db"], "containers": [ContainerStatus] }`

**PUT /api/apps/{id}** 請求同 POST → `200 App`

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

## 4. 非 API 路徑

| Path | 說明 |
| --- | --- |
| `GET /` | 管理介面（內嵌 `web/index.html`） |
| `GET /styleguide` | Design System 元件總覽（內嵌 `web/styleguide.html`，FR-UI-12） |
| `POST /mcp` | MCP 端點（§5） |
| `GET /assets/{*path}` | Design System 靜態檔，只提供 `caloud-api::assets::FILES` 白名單：`ds/tokens.css`、`ds/components.css`、`ds/theme.css`、`ds/ui.js`；其餘回 404 |
| 代理 `:80` `/.well-known/acme-challenge/{token}` | ACME HTTP-01 回應（M2） |
| 代理 `:80`／`:443` 其他路徑 | 依 Host 轉發至容器（FR-DOM-04、FR-DOM-07） |

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
| `create_app` | `compose_yaml`、`name?`、`deploy?`（預設 true） | App | — | `POST /api/apps` | member+ |
| `update_app` | `app_id`、`compose_yaml`、`name?`、`deploy?` | App | — | `PUT /api/apps/{id}` | 擁有者 |
| `deploy_app` | `app_id` | `{ app_id, status: "deploying" }` | I | `POST /api/apps/{id}/deploy` | 擁有者 |
| `stop_app` | `app_id` | `{ app_id, status: "stopped" }` | I | `POST /api/apps/{id}/stop` | 擁有者 |
| `delete_app` | `app_id`、`confirm: true`、`purge_volumes?` | `{ deleted: app_id }` | D | `DELETE /api/apps/{id}` | 擁有者 |
| `get_app_logs` | `app_id`、`service?`、`tail?`（預設 200，上限 5000） | `{ logs }` | R | `GET /api/apps/{id}/logs` | 擁有者 |
| `list_domains` | — | `{ domains: [Domain] }` | R | `GET /api/domains` | member+ |
| `create_domain` | `hostname` | Domain ＋ `dns_records`（需設定的 A 與 TXT） | — | `POST /api/domains` | member+ |
| `verify_domain` | `hostname` | Domain | I | `POST /api/domains/{hostname}/verify` | 擁有者 |
| `bind_domain` | `hostname`、`app_id?`（null 解除）、`service?`、`port?` | Domain | I | `PUT /api/domains/{hostname}/bind` | 擁有者 |
| `delete_domain` | `hostname`、`confirm: true` | `{ deleted: hostname }` | D | `DELETE /api/domains/{hostname}` | 擁有者 |
| `list_users` | — | `{ users: [User] }` | R | `GET /api/users` | admin+ |
| `set_user_role` | `account`、`role` | User | I | `PUT /api/users/{account}` | admin+ |

- 刪除類工具未傳 `confirm: true` → `isError: true`，訊息提示需確認（FR-MCP-05）。
- `tools/list` 對 member 不列出 admin 專用工具。
- 每個工具都有 `title`、`description`（繁體中文，說明用途與注意事項）與 `inputSchema`（JSON Schema，`additionalProperties: false`）。
