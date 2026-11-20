# ADR 0021：GitHub Checks、建置配額與發布前任務

- 狀態：Accepted
- 日期：2026-11-20
- 相關需求：FR-GH-03、FR-CI-06、FR-CI-07、FR-ENV-08

## 背景

M6 核心（ADR 0020）上線後還缺三件事：

- 驗證結果只在 Caloud 裡，GitHub 與 PR 上看不到；
- 建置資源寫死為 4 核、8 GB，沒有記錄建置分鐘；
- migration、seed 在驗證與部署時都不會執行，production 的資料庫要手動 migrate。

用戶決定：

- Checks 涵蓋專案分支與以它為 base 的 PR，PR 驗證會留言、不部署；
- 建置資源與每月建置分鐘由後台設定，M8 再改成依方案；
- 發布前任務以偵測或 `.caloud/verify.yml` 定義，驗證與 production 部署都執行（一併完成 M7-08）。

## 決策

### 1. GitHub Checks 與 PR（FR-GH-03）

- GitHub App 另需 **Checks（Read & write）** 與 **Pull requests（Read & write）** 權限；用戶在 App 設定加上後，各安裝需接受新權限。後台 GitHub App 頁提示。
- 只限以 GitHub 安裝匯入的專案。GitHub API 失敗只記錄 log，不影響驗證。
- 驗證開始時建立 check run：名稱 `Caloud 驗證`、`in_progress`、`details_url` 指向報告頁、`external_id` 為驗證 ID。
- 結束時更新：
  - `conclusion`：通過 `success`、未通過 `failure`、取消 `cancelled`、錯誤 `neutral`；
  - `output`：計數、前 20 個問題與報告連結。
- **PR 驗證**：
  - 觸發：`pull_request` 的 opened、synchronize、reopened，且 base 為專案分支；
  - 內容：以確認的設定驗證 head commit；
  - 分支標記為 `pr/<n>`，同一個 PR 有新的執行時取消舊的；
  - 取得原始碼時，以 `refs/pull/<n>/head` 為後備；
  - 不部署，也不算進部署關卡。
- **PR 驗證不注入任何用戶變數**：PR（含 fork）可能是不受信任的程式碼；只產生資料庫密碼。
- 每個 PR 只留一則留言（`github_pr_comments`），之後的結果更新同一則。
- 原本 `pull_request` 事件會以 head 分支觸發重新解析，這個行為移除。

### 2. 建置資源與建置分鐘（FR-CI-06）

- `build_settings`（單列）：`cpus` 1～16（預設 4）、`memory_mb` 1024～32768（預設 8192）、`minutes_monthly`（0 表示不限）。API 為 `GET`／`PUT /api/admin/build-settings`（admin+，寫入稽核）。
- 建置資源以 `EnvSpec.build` 帶給 agent，用在 BuildKit 建置，以及測試、lint、漏洞掃描沙箱；執行期的資源上限不變。
- 用量：
  - agent 在 `env_status`（`running` 或 `failed`）與 `verify_done` 回報 `build_secs`，即上述沙箱的累計秒數；
  - 寫入 `build_usage`，記在專案擁有者名下；
  - 每月從 UTC 月初起算。
- 本月用量達上限時，`verify` 與 `deploy` 回 `409`；進行中的執行不中斷。

### 3. 發布前任務（FR-CI-07、FR-ENV-08）

- `Analysis.verify.release: [{ service, command? }]`，來源依序：
  1. `.caloud/verify.yml` 的 `release`；
  2. compose 中帶 `profiles`、名稱或 command 含 `migrat` 或 `seed` 的服務；
  3. Prisma 且有 `prisma/migrations/` 時，在 App 服務執行 `npx prisma migrate deploy`。
- 執行方式：`docker compose run --rm -T <service> [sh -c <command>]`。值以行程環境變數帶入（同 ADR 0019），依賴的服務由 compose 啟動，逾時 10 分鐘。
- Runner 保留並建置 release 服務；其他帶 `profiles` 的服務仍移除。
- **驗證**：在 `start` 之後、`smoke` 之前執行（屬於 `start` 階段）；失敗為 Blocker（`release/failed`），並略過 smoke 與 logs。
- **部署（M7-08）**：
  - 新階段 `release`，位於 `build` 與 `start` 之間；
  - 新的 compose 與 override 先寫到 `compose.next.yml`、`caloud.override.next.yml`，以新設定執行任務；
  - 成功才換成正式檔並 `up`；
  - 失敗時回報 `failed`（`stage: release`），舊版繼續執行。

```yaml
# .caloud/verify.yml
release:
  - service: migrate
  - service: web
    command: npm run seed
```

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| PR 驗證也注入秘密變數 | 結果與 production 一致 | fork 的程式碼可以讀到金鑰 |
| 發布前任務在 `up` 之後執行 | 簡單 | 新版先上線、資料庫尚未 migrate；失敗時舊版已停止 |
| **以 next 設定先執行任務，成功才 `up`（採用）** | 失敗時舊版不受影響 | 任務啟動的依賴服務可能先被更新 |

## 影響

- 協定：`env.build`、`env.release`；`env_status` 與 `verify_done` 的 `build_secs`；部署階段新增 `release`。
- 資料表：`build_settings`、`build_usage`、`github_pr_comments`；`verifications` 新增 `check_run_id`、`pr_number`、`build_secs`。
- GitHub App 權限：新增 Checks、Pull requests。
