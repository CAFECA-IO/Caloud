# ADR 0024：專案頁改版與設定沿用

- 狀態：Accepted
- 日期：2026-11-25
- 相關需求：FR-AN-04、FR-AN-12、FR-ENV-14、FR-ENV-15、FR-UI-14

## 背景

以 Unimatrix 的使用情況分析專案頁，發現以下問題。

**push 之後自動部署被略過。**

- 每次 push 都會產生新的解析。只要解析 ID 不同，專案頁就顯示「最新解析與已確認的版本不同」，即使設定完全相同。
- 使用者照提示重新確認後，`confirmed_analysis` 換成新的解析。
- 驗證通過時，自動部署要求 `confirmed_analysis == verification.analysis`，部署關卡也比對解析 ID。驗證期間重新確認過，兩邊就對不上，自動部署被略過。
- 結果：`1e009f2` 驗證通過，卻從未上線。

**頁面以設定為主。**

- 已上線的專案，頁面仍是「匯入步驟＋判斷表＋compose」。
- 線上版本、下一個版本、是否會自動部署，都要從多張卡片拼湊。

**環境變數誤判。**

- compose `environment` 已提供的變數，仍顯示「未設定」。
- `os.environ.get(name)` 中的參數 `name` 被當成變數名稱。
- 程式中有預設值的變數，也列為「需要你提供」。

**資料遺失風險被埋沒。**

- 「寫入 `.data/` 但沒有掛 volume」只是判斷表中的一列 Medium，要使用者自己改 compose。

**無法部署指定版本，也無法回到上一版。**

用戶決定：

- 開關型設定一律使用 toggle switch（「我已確認」這類勾選仍用 Checkbox）；
- 預設分頁是「概況」；
- 「加上 volume」先顯示 compose 的修改再確認，並可選擇確認後立即重新部署（預設關閉）；
- 變數必填或選填由解析器自動判斷；
- 紀錄提供「回到這個版本」。

## 決策

### 1. 設定指紋與自動沿用確認

**設定指紋**：`settings_key(analysis)` 為以下三項的雜湊。部署與驗證只依這三項建置與啟動，所以只要它們相同，就是同一份設定。

- 有效 compose：用戶編輯過的 compose；沒有時用解析產生的 compose。
- `compose_dir`。
- 對外服務。

**自動沿用確認**：解析成功後，以下條件全部成立時，把上一份確認沿用到新的解析，並把 `projects.confirmed_analysis` 指向新的解析。稽核記為 `project_confirm`、`via=auto`。

- 專案已有確認；
- 新解析產生的 compose 與 `compose_dir`，都與確認時那份解析相同；
- 對外服務仍在新的 compose 中；
- 新的低信心項目都已在上次確認時勾選過（以訊息內容比對）。

沿用的內容：編輯過的 compose、對外服務、已勾選的低信心項目（依訊息重新對應到新的索引）。確認者與時間保留上一次的值，`confirmed_via` 設為 `auto`。

條件不成立時，維持上一份確認。專案頁在「設定」分頁列出 compose 差異，並在「需要處理」提示確認新設定。

**比對設定，不比對 ID**：

- **部署關卡**：目標 commit 有一次非 PR 的通過驗證，且該驗證使用的解析與目標的 `settings_key` 相同。
- **驗證通過後的自動部署**：比對驗證的解析與目前確認的解析的 `settings_key`。
- **略過時記錄原因**：設定不同時不部署，並在 `verifications.deploy_note` 記錄原因（例：「驗證期間設定有變動」），專案頁與紀錄顯示。

### 2. 部署指定版本與回滾（FR-ENV-15）

`POST /api/projects/{id}/deploy` 可帶 `{ "commit": "<40 碼>" }`，依序決定使用的設定：

1. 這個 commit 有非 PR 的通過驗證：用最近一次通過驗證所用的解析。
2. 這個 commit 曾成功部署：用那次部署記錄的解析（`deployments.analysis`）。
3. 都沒有：`409`。

帶 `commit` 時不再經過部署關卡（第 1 種已通過，第 2 種曾上線）。部署紀錄的 `trigger` 為 `manual`，回到舊版時稽核另記 `rollback`。

- 回滾只回到程式碼與 compose，不還原 volume 的資料，也不回退秘密變數。
- 每次部署都在 `deployments.analysis` 記錄使用的解析，作為之後回滾的依據。

### 3. 環境變數分類（FR-AN-12）

**解析器**：

- `os.environ.get(`、`os.getenv(`、`env::var(` 等呼叫形式，只接受字串字面量參數，不接受識別字（修正 `name` 誤判）。`process.env.X` 不變。
- 有預設值時標為選填（`EnvVar.optional = true`）。預設值的判斷：
  - JavaScript：`||`、`??`；
  - Python：`os.environ.get("X", …)`、`os.getenv("X", …)`；
  - Rust：`.unwrap_or(`、`.unwrap_or_else(`、`.unwrap_or_default()`、`.ok()`；
  - Go：`os.LookupEnv(`。
  
  同一個變數只要有一處沒有預設值，就是必填。

**API**（`GET /api/projects/{id}/vars`）：

- 確認的 compose 中任一服務的 `environment` 已提供值（不是 `${NAME}` 引用自己），`source` 為 `compose`，並附上 `value`（compose 內容本來就可見，不是秘密）。
- 用戶可把變數標為「不需要」（`project_var_ignores`），標記後不計入未設定。
- `missing_vars` 只計算以下條件全部成立的變數：必填、未設定、不是 compose 提供、沒有標為不需要。

### 4. 加上 volume

解析器在可以確定容器內路徑時（單一 build 服務、Dockerfile 有絕對路徑 `WORKDIR`），在 `persistent_dir` 判斷附上 `volume: { service, target }`。

**`GET /api/projects/{id}/volume-fix?service=&target=`** 以確認的有效 compose 產生修改：

- volume 名稱為 `<service>-data`，已存在時依序加上 `-2`、`-3`…；
- 頂層 `volumes` 加上這個 volume；
- 服務的 `volumes` 加上 `<volume>:<target>`；
- 回傳 `{ volume, before, after }`，UI 以逐行差異顯示。

**`POST /api/projects/{id}/volume-fix { service, target, deploy }`**：

- 以平臺政策檢查修改後的 compose，寫入確認的解析的 `compose`（成為編輯過的 compose），稽核 `project_volume_fix`；
- `deploy: true` 時接著部署（經部署關卡；新的設定未驗證時先驗證）；
- 不搬移容器內現有的資料，UI 在確認前提醒。
- 這個修改改變了設定指紋：進行中的驗證通過後不自動部署，並記錄原因。

### 5. 驗證耗時與預估

- `verifications.stage_times`：各階段 `{ "<stage>": [開始, 結束] }`，收到階段回報時寫入。
- 專案頁的 `verification.avg_secs`：最近 3 次已結束（passed 或 failed）、非 PR 的驗證的平均耗時；沒有時為 null。
- UI 顯示「已進行 6:40 · 約還要 14 分鐘」。

### 6. 專案頁（FR-UI-14）

**路由**：

| 路由 | 分頁 |
| --- | --- |
| `#projects/<id>` | 概況（預設） |
| `#projects/<id>/settings` | 設定 |
| `#projects/<id>/vars` | 環境變數 |
| `#projects/<id>/history` | 紀錄 |
| `#projects/<id>/verifications/<vid>` | 驗證報告（不變） |

**頁首**：名稱、儲存庫與分支、「開啟網站」、「更多」選單（強制部署、重新解析、刪除專案）、分頁。「環境變數」分頁標示未設定的必填數量。

**各分頁**：

- **概況**：
  - 線上版本、下一個版本（會不會自動部署與原因）、環境（網址、節點、GPU）三張卡；
  - 進行中的驗證或部署以流程條顯示階段、耗時與預估；
  - 「需要處理」：設定需要確認、加上 volume、通過但未部署的版本、必填變數未設定、部署失敗；
  - 最近活動；
  - log 只在失敗或進行中時展開。
- **設定**：
  - 確認狀態：已確認，或設定有變動（列出差異並可「確認新設定」）；
  - 服務與對外服務；
  - 判斷與依據（低信心項目勾選「我已確認」，persistent 項目附「加上 volume」）；
  - compose（預設收合，可編輯）；
  - 專案設定：分支、根目錄、版本標頭 switch；
  - 刪除專案。
- **環境變數**：分成四組，每個變數可標為「不需要」或還原：
  - 需要你提供；
  - 選填；
  - 已提供：秘密變數、compose、平臺；
  - 已標為不需要。
- **紀錄**：
  - 每個 commit 一列，驗證與部署並列；
  - 已通過但不是線上版本的 commit 顯示「部署這個版本」；
  - 曾成功上線的舊版本顯示「回到這個版本」，按下後先確認。

**移除**：匯入步驟列、「已確認，按『部署』上線」的說明、頁尾的「更新確認」。

**登入頁**：「第一位登入的人會成為 Super Admin」只在系統還沒有使用者時顯示（`/api/auth/config` 的 `first_user`）。

**元件**（05）：

- `c-switch`：開關；
- `c-diff`：逐行差異；
- `c-flow`：流程條；
- `c-menu`：「更多」選單。

## 後果

- push 不再要求重新確認相同的設定；手動確認與自動部署不再互相干擾。
- 設定真的改變時（例如 compose 改了），仍必須人工確認，這是刻意保留的關卡。
- 回滾只涵蓋程式碼與 compose；資料與秘密變數不回退，UI 明示這點。
- 「不需要」標記只影響提示，不影響部署注入的變數。
