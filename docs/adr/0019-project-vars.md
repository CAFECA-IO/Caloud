# ADR 0019：秘密變數與部署時注入

- 狀態：Accepted
- 日期：2026-11-19
- 相關需求：FR-ENV-05、FR-APP-11、FR-OPS-14、FR-AN-09、FR-AN-11

## 背景

SwarmStorage 第一次部署（ADR 0018）暴露出兩個缺口：

- 環境變數（`SESSION_SECRET`、`IPFS_API_URL`…）沒有地方填值，只能寫進明文 compose，也不分環境；
- 解析器產生的 `${CALOUD_DB_PASSWORD}`（附加資料庫）與建置時變數的 build args `${NAME}`（FR-AN-09）在部署時沒有人提供值，會原樣傳入。

用戶決定：值依環境分開（本輪只有 production）；變更後提示、手動重新部署；用戶變數只注入從儲存庫建置的服務。

## 決策

### 1. 儲存

- 資料表 `project_vars(project, env, name, value_enc, hint, generated, updated_by, updated_at)`，主鍵 `(project, env, name)`。
- 值以主金鑰加密（`caloud_core::secret`，AAD `project_var:<project>:<env>:<name>`）；未設定主金鑰時拒絕寫入（`409`）。
- **只寫不讀**：API 只回傳名稱、是否已設定與提示。值長度 ≥ 12 時提示為末 4 碼，較短時不提示。
- 名稱須符合 `[A-Za-z_][A-Za-z0-9_]*`，`CALOUD_` 前綴保留給平臺；值最長 32 KB；每個環境最多 200 個。
- 稽核只記錄變數名稱，不記錄值。

### 2. 平臺產生的變數

- compose 用到 `${CALOUD_DB_PASSWORD}` 時，第一次部署自動以 CSPRNG 產生 32 字元，存為 `generated = 1`。
- 用戶不可修改或清除：資料庫 volume 以這個密碼初始化，改了會無法登入。

### 3. 注入

- `Deploy` 的 `EnvSpec.vars` 帶上解密後的值；只經已驗證的節點連線傳送，agent 只放在記憶體，Debug 輸出遮蔽。
- agent 執行 `docker compose up` 時，把值放進**行程環境變數**：
  - override 對每個從儲存庫建置的服務加上 `environment: [NAME, …]`（只有名稱），compose 從行程環境取值；
  - compose 中的 `${CALOUD_DB_PASSWORD}` 由 compose 依同一份環境變數內插；
  - Caloud 不把值寫進任何檔案（容器設定仍由 Docker 自己保存）。
- 用戶變數只注入從儲存庫建置的服務；現成映像（資料庫、IPFS…）只透過 compose 的 `${…}` 取得需要的值。
- 建置時變數：build args 中的 `${NAME}`／`$NAME` 由 agent 以變數值內插；未設定時傳空字串並在 log 記一行提醒。**建置時變數會寫進映像**，不適合放秘密（UI 標示「建置時」）。
- 值的優先序：秘密變數 > compose 中寫的值。

### 4. 遮蔽

- agent 送出的每行 log 與錯誤訊息，把每個長度 ≥ 4 的值換成 `***`。
- 控制面的回應、稽核與 log 都不含值。

### 5. 生效

- 專案的 `vars_rev` 在每次變更時加 1，部署時記錄到 `environments.vars_rev`；兩者不同時部署卡片提示「變數已變更，重新部署後生效」。
- 部署前列出需要但未設定的用戶變數（`missing_vars`），不阻擋部署。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| 把值寫進 override 檔或 env 檔 | 簡單、可重跑 | Runner 磁碟上多一份明文 |
| **行程環境變數＋只寫名稱的 override（採用）** | Caloud 不寫明文檔；compose 原生支援 | 之後手動執行 compose 需重新帶入 |
| 所有服務都注入 | 最簡單 | 現成映像拿到不需要的金鑰 |

## 影響

- 協定：`EnvSpec.vars`（選用，舊 agent 會忽略，等同未設定）。
- 資料表：`project_vars`；`projects.vars_rev`、`environments.vars_rev`。
- `sandbox::env_override` 新增注入清單參數。
