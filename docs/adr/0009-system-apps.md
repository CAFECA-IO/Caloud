# ADR 0009：Git 來源的特權 App 與 TLS 直通

- 狀態：Accepted
- 日期：2026-11-09
- 相關需求：FR-APP-13～16、FR-DOM-09、FR-DOM-10

## 背景

要以 Caloud 部署 CAFECA 的基礎服務：

| 服務 | 來源 | 需要 |
| --- | --- | --- |
| LiveKit（`livekit.cafeca.io`、`turn.cafeca.io`） | `CAFECA-IO/SwarmStorage` 的 `deploy/livekit` | `build`、`network_mode: host`（UDP 媒體 7882、TURN 3478／5349、TCP 7881）、自帶 Caddy 申請憑證、`.env` |
| iroh-relay（`relay.cafeca.io`） | `CAFECA-IO/hills.cafeca.io` 的 `deploy`（只要 `relay` service） | `build`、自行以 Let's Encrypt（TLS-ALPN-01）申請憑證並在 443 終結 TLS、UDP 7842 |

原本的設計（[0001](0001-compose-cli.md)、[07 §3](../07-security.md#3-compose-政策)）只接受上傳 compose、只用 image、不開主機 port、只轉發 HTTP，無法部署這類服務；Caloud 自己的 HTTPS（M2-01～03）也尚未完成。

## 決策

1. **Git 來源**：App 可指定公開 HTTPS 儲存庫、ref、目錄與 compose 檔名。部署時 Caloud 以 `git` 取得該版本到 `/data/apps/<id>/src`，以該目錄為 `--project-directory` 執行 `docker compose up -d --build --wait [services…]`，相對路徑的 `build.context` 與 `env_file` 都在儲存庫內解析。
2. **環境變數**：App 的 `.env` 內容存在資料庫，部署時寫入專案目錄；API 只回傳變數名稱。
3. **特權 App**：只有 Admin 以上可建立或設定。特權政策放寬 `build`、`network_mode: host`、`ports`、`env_file`、`tmpfs`，但仍禁止 `privileged`、`cap_add`、`devices`、主機路徑掛載、docker.sock 等可直接取得主機權限的設定。Caloud 保留的主機連接埠以 compose 的 `!override` 從 `ports` 移除，改由網域轉發。
4. **TLS 直通**：Caloud 在 443 讀取 ClientHello 的 SNI，網域設為直通時把整條 TCP 連線轉給 App，App 自己處理 TLS（包含 ACME TLS-ALPN-01）。非直通網域在 Caloud 的 HTTPS 完成（M2-01）前一律關閉連線。
5. **host 網路 service**：Caloud 容器加入 `host.docker.internal:host-gateway`，綁定到 host 網路 service 時以主機 port 轉發。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **Git 來源＋特權 App＋TLS 直通**（採用） | 不需修改服務自己的部署檔；每個服務自己管理憑證；Caloud 統一管理 80／443 與生命週期 | 特權 App 等同主機上的高權限，需限制給 Admin |
| 由 Caloud 終結 TLS 再轉發 | 憑證集中管理 | TURN/TLS 與 iroh QUIC 需要憑證檔；要等 M2 並修改服務設定 |
| 不經 Caloud、直接在主機跑各專案 compose | 最快 | 80／443 衝突需另架前端；失去 Caloud 的管理、稽核與 MCP |
| 預先建置映像推到 registry | 不需在主機建置 | 需要 registry 與 CI，改動上游專案流程 |

## 影響

- Caloud 映像需包含 `git`；`deploy/docker-compose.yml` 加入 `extra_hosts`，並由 Caloud 監聽 443
- 新增 migration 0003（App：來源、環境變數、指定 service、特權；網域：TLS 模式與 port）
- [07 安全規範 §3.5](../07-security.md#35-特權-app-政策) 定義特權政策
- 建置在主機上進行，會佔用 CPU 與磁碟；建置失敗的訊息寫入 App 的錯誤欄位
