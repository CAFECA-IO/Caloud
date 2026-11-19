# ADR 0017：GitHub App 整合與主控台對外

- 狀態：Accepted
- 日期：2026-11-19
- 相關需求：FR-GH-01、FR-GH-02、FR-GH-05、FR-DOM-05、NFR-SEC-01

## 背景

在此之前，主控台只綁在 127.0.0.1:8080，要經 SSH 通道才能使用；專案匯入也只支援公開儲存庫（FR-GH-04）。

要接上 GitHub App（M5-01），有兩件事必須成立：
- GitHub 的 webhook 與安裝回呼需要一個對外的 HTTPS 網址；
- 這個網址就是 ADR 0010 規劃的主控台網域 `caloud.cafeca.io`。

主控台一旦對外，原本規劃的基本防護（M2-07）也要同時到位。

CAFECA-IO 組織已建立 GitHub App（App ID 5826601）。

## 決策

### 1. 主控台網域（M2-03）

- `CALOUD_PUBLIC_ORIGIN` 的 host 是**系統路由**：Edge 把它轉到同一個容器內的 API（`127.0.0.1:<API port>`），不需要網域資料表，也不必經 TXT 驗證，因為依據就是維運者的設定。
- 憑證由憑證管理（ADR 0016）以 HTTP-01 申請。
- `:8080` 仍只綁 127.0.0.1，作為維運用的後門。
- CAFECA 簽章綁定 origin，所以改成 `https://caloud.cafeca.io` 後，經 SSH 通道的 localhost 登入就失效。

### 2. GitHub App 憑證（FR-GH-05）

- 存在單列資料表 `github_app`。App ID、slug、Client ID 為明文；client secret、私鑰、webhook secret 以主金鑰加密（`caloud_core::secret`，AAD 為 `github_app:<欄位>`）。
- Admin API 只寫不讀，回應只附末 4 碼。
- 設定時以私鑰簽 JWT 呼叫 `GET /app`，確認私鑰與 App ID 相符，同時取得 slug。

### 3. 安裝與擁有者確認（FR-GH-01）

GitHub App 勾選「安裝時要求使用者授權」，流程如下：

1. 主控台呼叫 `GET /api/github/install`，取得安裝網址 `https://github.com/apps/<slug>/installations/new?state=<state>`。state 為一次性、綁定 session、10 分鐘有效。
2. GitHub 回呼 `GET /api/github/callback?code&installation_id&state`（`setup_action=install|update`）。
3. Caloud 驗證 state，以 Client ID／secret 交換 user token，呼叫 `GET /user/installations`，確認 `installation_id` 屬於這個 GitHub 使用者。
4. 寫入 `github_installations` 並綁定到 Caloud 帳號；同一個安裝已綁定到其他人時回 409。

user token 只在記憶體使用一次，不保存。這樣可以防止他人以別人的 `installation_id` 冒領安裝。

### 4. installation token

- 以 App 私鑰簽 RS256 JWT（`iat` 為現在減 60 秒，`exp` 為現在加 9 分鐘），呼叫 `POST /app/installations/{id}/access_tokens`。
- 權杖只快取在記憶體，到過期前 5 分鐘為止；不寫入資料庫、log 或檔案。
- 取得私有儲存庫時，git 以 `-c http.extraHeader=Authorization: Basic <base64(x-access-token:<token>)>` 帶入權杖。網址不含帳密，權杖不寫入 `.git/config`。其餘沿用 `git::fetch_repo` 的強化設定。

### 5. webhook（FR-GH-02）

- 驗證：`POST /api/github/webhook` 以 webhook secret 驗證 `X-Hub-Signature-256`（HMAC-SHA256，常數時間比較）；簽章錯誤或缺少時回 401。
- 本體上限 5 MB；`X-GitHub-Delivery` 寫入 `github_deliveries` 去重，保留 7 天。
- 處理的事件：
  - `installation`：deleted 時刪除安裝；suspend／unsuspend 時更新狀態。
  - `installation_repositories`：記錄即可。可匯入的清單一律即時向 GitHub 查詢。
  - `push`、`create`（branch）、`pull_request`（opened／synchronize／reopened）：找出同一安裝、同一儲存庫，且分支相符（`git_ref` 為 null 時比對預設分支）的專案，重新解析並寫入稽核。驗證流程在 M6 加入後改為觸發驗證。

### 6. 對外防護（M2-07，NFR-SEC-01）

- **Origin**：
  - `/api/**` 的寫入請求（POST／PUT／PATCH／DELETE）帶有 `Origin` 時必須等於 public origin，否則回 403。
  - 沒有 `Origin` 的請求不擋：瀏覽器的跨站寫入一定會帶 Origin，而且 session cookie 為 `SameSite=Strict`，跨站請求本來就不會帶 cookie。
  - webhook、agent 端點與只帶 Bearer 的 MCP 不在此限。
- **速率限制**（記憶體中、依來源 IP、固定 60 秒視窗）：
  - `/api/auth/nonce`、`/api/auth/login`：每分鐘 20 次；
  - `/api/github/webhook`：每分鐘 300 次；
  - 其他寫入：每分鐘 120 次。
  - 超過回 429，並附 `Retry-After`。
- **來源 IP**：只有請求從本機（Edge）進來時，才信任 `X-Forwarded-For` 的最後一個值。
- **安全標頭**：所有回應加上 `X-Content-Type-Options: nosniff`、`Referrer-Policy: strict-origin-when-cross-origin`、`X-Frame-Options: DENY`。public origin 為 https 時另加 `Strict-Transport-Security: max-age=31536000`。HTML 頁面加上 `Content-Security-Policy`：
  - `default-src 'self'`；
  - `script-src 'self' 'unsafe-inline' <錢包>`；
  - `connect-src 'self' <錢包> <RPC>`；
  - `frame-src <錢包>`；
  - `img-src 'self' data:`；
  - `style-src 'self' 'unsafe-inline'`。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| 只用 `installation_id`（Setup URL）綁定 | 不需 OAuth | 任何人可冒領他人的安裝 |
| 長期保存 user token 代替 installation token | 可以用使用者權限操作 | 違反 FR-GH-01「不存長期憑證」 |
| 權杖放在 clone 網址 | 實作最簡單 | 權杖會寫入 `.git/config` 與錯誤訊息 |
| 主控台另開一個 listener 對外 | 不經 Edge | 憑證與 TLS 要另外處理，與 Edge 重複 |

## 影響

- 新增資料表 `github_app`、`github_installations`、`github_deliveries`；`projects` 新增 `installation_id`、`repo_full_name`。
- 正式機 `.env` 的 `CALOUD_PUBLIC_ORIGIN` 改為 `https://caloud.cafeca.io`。
- 新增設定 `CALOUD_GITHUB_API`（預設 `https://api.github.com`）、`CALOUD_GITHUB_WEB`（預設 `https://github.com`），測試時指向替身。
