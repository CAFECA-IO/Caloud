# ADR 0016：Edge TLS 終結與憑證管理

- 狀態：Accepted
- 日期：2026-11-18
- 相關需求：FR-OPS-03、FR-OPS-10、FR-OPS-14、FR-DOM-05、FR-DOM-08、FR-DOM-09

## 背景

在此之前，`:443` 只做 SNI 直通（ADR 0009），其他連線一律關閉。`:80` 只轉發 HTTP。

M4-10 開出的環境網址是 `<id>.caloud.tw`，需要由 Edge 終結 TLS（ADR 0010：`*.caloud.tw` 記錄設為「僅 DNS」）。同時，M2 規劃的「已驗證網域自動申請憑證」（FR-DOM-05）也還沒完成。

Cloudflare API 權杖可以改 DNS，屬於高風險祕密，不能以明文存進資料庫。

## 決策

### 1. `:443` 單一 listener

讀 TLS ClientHello 的 SNI 後依序判斷：

1. 網域設為直通 → 原封不動轉給 App（ADR 0009，不變）；
2. `CertStore` 有可用憑證 → 以 rustls 終結 TLS（ALPN：h2、http/1.1），交給與 `:80` 同一套 HTTP 代理，`x-forwarded-proto: https`；
3. 其他 → 關閉連線。

已讀出的 ClientHello 位元組以 rewind 包裝後交給 rustls，不必重新讀取。

### 2. 憑證存放與選擇

- 檔案：`<data_dir>/certs/<staging|production>/<名稱>/{cert.pem,key.pem}`。staging 與 production 的憑證分開放，切換 `CALOUD_ACME_STAGING` 時不會誤用。
- 啟動時載入記憶體（ArcSwap），簽發後立即替換，不中斷連線。
- 依 SNI 選擇：先找完全相符，再找上一層 wildcard（`a.caloud.tw` 可用 `*.caloud.tw`，`a.b.caloud.tw` 不行）。

### 3. 簽發（instant-acme，Let's Encrypt）

| 來源 | challenge | 名稱 |
| --- | --- | --- |
| 網域池有 Cloudflare 權杖 | DNS-01（經 Cloudflare API 寫 `_acme-challenge` TXT，用完即刪） | `root` 與 `*.root` 同一張 |
| 已驗證的網域：非直通，且綁定 running 的 App | HTTP-01（`:80` 的 Caloud token 優先） | 該網域 |

- 背景任務每 6 小時掃描一次；網域驗證、綁定、網域池變更時也會觸發。
- 到期前 30 天續簽。
- 一次只處理一張憑證。
- 失敗時退避重試：1 小時起，每次加倍，最多 24 小時。錯誤寫入 `certificates.error` 與稽核。
- DNS-01 寫入 TXT 後，以 DoH（`CALOUD_DOH_URL`）確認查得到，才通知 ACME 驗證。
- ACME 帳號金鑰存在 `<data_dir>/acme/<staging|production>/account.json`，檔案權限 600。
- 沒設定 `CALOUD_ACME_EMAIL` 時不簽發，只記錄警告。

### 4. `:80` 轉 HTTPS

處理順序：

1. ACME HTTP-01 token；
2. host 有可用憑證 → 308 轉到 `https://<host><path>`；
3. 其他照舊轉發。

直通網域沒有 Caloud 的憑證，所以 `:80` 照舊轉給 App，不影響 App 自己的 HTTP-01。

### 5. 主金鑰與祕密加密（FR-OPS-14）

- `CALOUD_MASTER_KEY`：32 bytes 的 base64，只從環境變數讀取，不寫入資料庫、日誌或 API 回應。
- 加密使用 ChaCha20-Poly1305，nonce 每次隨機。格式為 `v1:<base64(nonce‖密文)>`。
- AAD 為「用途:主鍵」，例如 `cloudflare_token:caloud.tw`。這樣密文搬到別的列也無法解開。
- 沒設定主金鑰時，Admin API 拒絕設定權杖。
- M7 的祕密變數（FR-APP-11）共用同一把主金鑰與格式。

### 6. Cloudflare 權杖

- 設定時，Caloud 先以該權杖呼叫 `GET /zones?name=<zone>` 取得 zone id，成功才加密保存。
- API 只回傳末 4 碼（`cloudflare_token_hint`）。
- `*.root` 的 A 記錄由維運者在 Cloudflare 建一次，設為「僅 DNS」。Caloud 不改 A／CNAME 記錄。

## 不在此決策

- 自訂網域的萬用字元憑證：需要用戶自己的 DNS 權杖或 CNAME 委派，於 M7（FR-MIG-02、M7-04）處理。
- 管理介面改走 `:443`（M2-03）。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| 權杖放在 `.env` | 最簡單 | 每個網域池無法各自設定權杖；換權杖要重啟 |
| Cloudflare 代理（橘雲）終結 TLS | 不必管憑證 | 違反 ADR 0010；WebSocket 與直通受限 |
| 每個子網域各申請一張憑證 | 不需要 DNS API | 碰上 Let's Encrypt 速率限制；開環境時要等簽發 |
| **wildcard（DNS-01）＋自訂網域 HTTP-01（採用）** | 開環境立即有 HTTPS；自訂網域不需要 DNS 權杖 | 需要妥善保管 Cloudflare 權杖 → 以主金鑰加密 |

## 影響

- `caloud-proxy` 新增 rustls、tokio-rustls；`caloud-api` 新增 instant-acme、rcgen；`caloud-core` 新增 chacha20poly1305。
- 新增 `certificates` 資料表；`domain_pools` 加上權杖密文、提示與 zone id 欄位。
- 正式機 `.env` 新增 `CALOUD_MASTER_KEY`、`CALOUD_ACME_EMAIL`、`CALOUD_ACME_STAGING`。主金鑰遺失時，已存的權杖無法解開，需要重新設定。
