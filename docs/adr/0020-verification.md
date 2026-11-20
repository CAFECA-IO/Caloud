# ADR 0020：驗證流程與報告

- 狀態：Accepted
- 日期：2026-11-20
- 相關需求：FR-CI-01～05、FR-RPT-01～04、FR-ENV-12、FR-OPS-01

## 背景

最小部署流程（ADR 0018）在確認後直接部署 production，沒有任何檢查。M6 在部署前加上驗證：在臨時的驗證環境中取得原始碼、建置、檢查、測試與啟動，產生分級報告，結束後銷毀。

用戶決定：

- 本輪做 M6-01～04、M6-06；GitHub Checks（M6-05）、建置配額（M6-07）、發布前任務（M6-08）留到下一輪。
- **驗證通過才能部署**；admin 可強制部署並留下稽核。
- **專案測試**依語言偵測指令，可用 `.caloud/verify.yml` 覆寫，在 gVisor 沙箱中執行。

## 決策

### 1. 驗證執行

- 觸發：
  - push 與新分支：GitHub webhook 重新解析後；
  - 手動：專案頁「驗證」；
  - 部署：目標 commit 尚未通過驗證時，「部署」改為先驗證（`deploy_after`），通過後自動部署。
- 同一專案同一分支有新的執行時，取消進行中的舊執行（FR-CI-04）。
- 驗證環境：kind `verify`、16 字元子網域；沿用 M4-10 的節點與網段配置（需要建置環境）；不加入 Edge 路由；結束後刪除紀錄。
- 逾時：建置 20 分鐘、測試與 lint 各 15 分鐘、啟動 10 分鐘；整體超過 45 分鐘或節點離線時，控制面判為 error 並清理。
- 要驗證的 commit 與 compose 與部署相同（`deploy::target`）：確認的 compose；最新解析的 compose 與確認版本相同時用最新的 commit。

### 2. 十個階段（FR-CI-02）

| 階段 | 執行 | 內容與問題等級 |
| --- | --- | --- |
| `fetch` | agent | 取得原始碼；失敗 Blocker |
| `analyze` | 控制面 | 最新解析與確認版本不同時 High（「設定有變動，請重新確認」） |
| `build` | agent | gVisor BuildKit；失敗 Blocker |
| `static` | agent | 秘密外洩（內建規則，High，值遮蔽）；相依套件漏洞（osv-scanner，CVSS ≥ 9 High、≥ 7 Medium、其餘 Low）；lint（有指令時，失敗 Medium） |
| `start` | agent | `compose up --wait`；失敗 Blocker |
| `test` | agent | 專案測試；失敗 High；沒有測試 Low |
| `smoke` | agent | 從 Runner 主機對對外服務固定 IP 發 `GET`（`/` 與 `verify.yml` 的路徑）；無回應 Blocker、5xx High |
| `logs` | agent | 容器重啟或以非 0 結束 High；`panic`、`FATAL`、`Unhandled`、`Traceback`、`ECONNREFUSED` 等關鍵字 Medium |
| `report` | 控制面 | 彙整、去重、判定、與前次比較 |
| `destroy` | agent | `down -v`、刪除映像與建置快取 |

- 前面的階段失敗（Blocker）時，之後需要執行環境的階段標為 `skipped`；`report` 與 `destroy` 一定執行（FR-CI-03）。
- 測試失敗不會略過 `smoke` 與 `logs`。
- 失敗時可選擇保留環境 30 分鐘（FR-CI-05）：agent 不銷毀，控制面到期後送 `destroy`。

### 3. 沙箱

- **測試與 lint**：`docker run --rm --runtime runsc --network caloud-build --cpus 4 --memory 8g --pids-limit 4096`，掛載可寫的原始碼副本與 resolv.conf，執行 `sh -c "<setup> && <command>"`。網路與建置相同（可連外網，封鎖內網、metadata、主機）；不注入任何變數。
- **漏洞掃描**：`ghcr.io/google/osv-scanner:v2.3.0` 以 digest 固定，同樣在 gVisor 與 `caloud-build` 網路中執行，原始碼唯讀掛載。
- **秘密外洩**：agent 內建規則（私鑰、AWS、GitHub、GitLab、Slack、Stripe、Google、npm 權杖格式），跳過相依套件、建置產物與 `.env.example` 類檔案；回報只含規則、檔案、行號與遮蔽後的片段。

### 4. 報告

- 問題：`severity`（blocker／high／medium／low）、`stage`、`rule`、`title`、`detail`、`file`?、`line`?。
- 指紋：`sha256(stage|rule|file|正規化 title)` 前 16 碼；同一次執行內去重（FR-RPT-01）。
- 判定：有 Blocker 或 High 即失敗。
- 與前次比較（FR-RPT-02）：對照同分支上一次完成（passed／failed）的執行：新增、仍存在、已修正。
- 匯出 JSON 與 Markdown（FR-RPT-03）；每個專案保留最近 50 次（方案化留給 M8）。
- 通知（FR-RPT-04）：本輪為站內通知：總覽「需要處理」與專案列表顯示最近一次驗證失敗。

### 5. 部署關卡（FR-ENV-12）

- `POST /api/projects/{id}/deploy`：目標 commit 有通過的驗證 → 部署；否則啟動驗證（`deploy_after`）並回 `202 { "verification": … }`。
- admin 帶 `{"force": true}` 略過驗證；稽核記錄 `forced`。
- push：已部署且設定未變動時，驗證通過後自動重新部署。

### 6. 變數

- 驗證環境使用 production 的用戶變數；平臺產生的變數（資料庫密碼）每次執行重新產生，不寫入資料庫。

### 7. `.caloud/verify.yml`（選用）

```yaml
test: { image: node:22-bookworm, setup: npm ci, command: npm test }
lint: { command: npm run lint }   # image、setup 省略時沿用 test
smoke: [/, /api/health]
```

### 8. 稽核來源（FR-OPS-01）

部署、驗證、秘密變數、權杖的稽核一律寫出 `via=web|api|mcp|github`。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| 控制面逐階段下指令 | 控制面掌握流程 | 訊息往返多，斷線時狀態難以收斂 |
| **agent 執行整個流程並逐步回報（採用）** | 一則訊息、agent 端可取消與清理 | 控制面需處理逾時與節點離線 |
| 驗證直接用 production 環境 | 省資源 | 汙染正式資料，違反 FR-CI-03 |

## 影響

- 協定：`verify`、`cancel_verify`；`verify_stage`、`verify_issue`、`verify_log`、`verify_done`。舊 agent 不回應 `verify`，控制面逾時後判為 error。
- 資料表：`verifications`、`verification_issues`、`verification_logs`。
- `Analysis.verify`（測試、lint、smoke）。
- Runner 預先拉取 osv-scanner 映像（`runner-setup.sh`）。
