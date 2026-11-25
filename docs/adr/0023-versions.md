# ADR 0023：建置識別與版本可見性

- 狀態：Accepted
- 日期：2026-11-25
- 相關需求：FR-OPS-15、FR-ENV-14、FR-GH-06

## 背景

更新後無法確認是否已在運行最新版：

- **控制面與 agent**：回報的版本都是 Cargo 的 `0.1.0`，看不出 commit。`/` 沒有快取標頭，瀏覽器可能用舊的 `index.html` 搭配新的 API；已開著的分頁也不知道伺服器已更新。
- **專案**：
  - 部署一開始就把 `environments.commit_sha` 改成新 commit，取得原始碼、建置或發布前任務失敗時，專案頁顯示的版本是錯的，而舊版其實還在執行；
  - Caloud 不知道容器實際使用的映像；
  - App 本身與外部都看不出線上版本；
  - 沒有部署紀錄。
- **GitHub**：GitHub 上看不到部署狀態。

用戶決定：

- 全部一起做，包含 GitHub Deployments；
- 專案的 `X-Caloud-Commit` 標頭預設開啟，可依專案關閉。

## 決策

### 1. 建置識別（FR-OPS-15）

- **編譯時寫入**：`caloud_core::build` 以 `option_env!` 讀取 `CALOUD_COMMIT`（短 commit）與 `CALOUD_BUILT_AT`（RFC 3339），未提供時為 `dev`。
  - Dockerfile 以 `ARG` 帶入；
  - 部署時用 `docker build --build-arg CALOUD_COMMIT=$(git rev-parse --short HEAD) --build-arg CALOUD_BUILT_AT=…`；
  - 映像加上 OCI label `org.opencontainers.image.revision`。
- **控制面**：
  - `GET /api/version`：不需登入，回傳 `{ version, commit, built_at }`；
  - 每個回應帶 `X-Caloud-Build: <commit>`；
  - MCP `serverInfo.version` 為 `<version>+<commit>`。
- **agent**：`NodeSpecs.agent_commit` 回報自己的 commit。節點頁顯示這個 commit；與控制面不同、且兩邊都不是 `dev` 時，標示「需要更新」。

### 2. 管理介面的快取與更新提示

- **`GET /`**：
  - 伺服器在 `index.html` 中，把 `/assets/…` 改為 `/assets/…?v=<commit>`，並加入 `<meta name="caloud-build" content="<commit>">`；
  - 回應 `Cache-Control: no-cache` 與 `ETag: "<commit>"`，`If-None-Match` 相符時回 `304`。
- **`/assets/…`**：`v` 等於目前 commit 且不是 `dev` 時，回應 `Cache-Control: public, max-age=31536000, immutable`；其他情況維持 `no-cache`。
- **更新提示**：前端記下載入時的 build，之後比對每個 API 回應的 `X-Caloud-Build`。不一致時顯示橫幅「Caloud 已更新到 <commit>，重新整理以載入新版」，附「重新整理」按鈕。側欄底部顯示目前版本。

### 3. 專案的版本（FR-ENV-14）

- **欄位語意**：
  - `environments.commit_sha` 改為「本次部署的目標」；
  - 新增 `running_commit` 與 `deployed_at`，只在健康檢查通過時寫入；
  - migration 時，以 `status='running' AND stage='done'` 的 `commit_sha` 回填 `running_commit`。
- **實際映像**：`env_status` 的 `services` 新增 `image`：agent 在 `up` 之後以 `docker compose ps` 取得容器實際的映像。
  - 控制面從平臺映像標籤 `caloud-env/<env>/<service>:<commit 前 12 碼>` 算出每個服務的 commit；
  - 與 `running_commit` 不一致時，專案頁顯示警告。
- **專案頁的 `deployment`**：新增以下欄位，UI 分「執行中」「正在部署」「部署失敗（仍執行舊版／服務未正常運作）」三種狀況顯示：
  - `running_commit`、`deployed_at`、`target_commit`；
  - `services: [{ name, image, commit }]`；
  - `branch_commit`：最新解析的 commit；
  - `branch_verify`：它最近一次驗證的狀態。
- **App 可讀的變數**：對從儲存庫建置的服務，平臺同時以執行期環境變數與 build arg 提供下列變數。這些不是秘密變數，不做遮蔽：

  | 變數 | 值 |
  | --- | --- |
  | `CALOUD_COMMIT` | 完整 commit |
  | `CALOUD_COMMIT_SHORT` | 前 7 碼 |
  | `CALOUD_BRANCH` | 分支；PR 驗證為 `pr/<n>` |
  | `CALOUD_ENV` | `production`、`staging` 或 `verify` |
  | `CALOUD_DEPLOYED_AT` | RFC 3339 時間 |

  build arg 只在 Dockerfile 宣告 `ARG` 時才生效，未宣告不影響建置。
- **Edge 標頭**：
  - 專案環境的回應帶 `X-Caloud-Commit: <running_commit 前 7 碼>`；
  - `projects.commit_header`（預設 1）控制，`PATCH /api/projects/{id}` 的 `commit_header` 可以關閉；
  - 只加在 Edge 轉發的回應；不改寫 App 自己的快取標頭。
- **部署紀錄**：新增資料表 `deployments`，欄位為 ID、專案、環境、commit、觸發、狀態、失敗階段、錯誤、GitHub deployment ID、建立者與時間。
  - 觸發：`manual`、`forced`、`push`（驗證通過後自動部署）；
  - 狀態：`in_progress` → `success` 或 `failure`；
  - `GET /api/projects/{id}/deployments` 回傳最近 20 筆。
- **順帶修正**：`in_progress` 漏了 `release` 階段，發布前任務期間可以重複部署。

### 4. GitHub Deployments（FR-GH-06）

- GitHub App 另需 **Deployments（Read & write）** 權限。只限以 GitHub App 匯入的專案；API 失敗只記 log，不影響部署。
- **部署開始時**：
  - `POST /repos/{repo}/deployments`，內容為 `{ ref: <commit>, environment: "production", auto_merge: false, required_contexts: [], production_environment: true, description: "Caloud" }`；
  - 接著送出狀態 `in_progress`，附 `log_url`（專案頁）與 `environment_url`；
  - 部署紀錄保存 deployment ID。
- **結束時**：成功送出 `success`；失敗送出 `failure`，`description` 寫明失敗的階段，以及舊版是否仍在執行（140 字內）。GitHub 會自動把同一環境較早的部署標為 inactive。
- 狀態的送出在背景依序執行：結束時 deployment 若尚未建立，最多等 30 秒。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| 版本號手動遞增 | 不需建置參數 | 容易忘記，看不出 commit |
| 前端定期輪詢 `/api/version` | 不需標頭 | 多餘請求；API 回應已經帶有足夠資訊 |
| **commit 寫入執行檔，回應帶標頭（採用）** | 每個回應都能比對，不需額外請求 | 部署時要帶 build arg |
| Edge 改寫 App 的 HTML 快取標頭 | 解決 App 的舊頁面問題 | 破壞 App 自己的快取策略 |

## 影響

- **資料表**：
  - `environments`：新增 `running_commit`、`deployed_at`；
  - `projects`：新增 `commit_header`；
  - 新增 `deployments`。
- **協定**：
  - `NodeSpecs.agent_commit`；
  - `EnvService.image`；
  - `EnvSpec.platform_vars`（`[{name, value}]`，平臺提供的非秘密變數）。
- **GitHub App 權限**：新增 Deployments。
