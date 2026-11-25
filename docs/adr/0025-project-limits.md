# ADR 0025：專案資源上限

- 狀態：Accepted
- 日期：2026-11-26
- 相關需求：FR-ENV-16

## 背景

- **目前的上限**：平臺專案的每個服務固定 1 GB 記憶體、1 核 CPU、512 個程序。這些值由 runner 的 `Limits::default()` 寫進 compose override，用戶在 compose 寫的 `mem_limit` 會被蓋掉。
- **排程估算**：選節點時，每個服務以 512 MB、0.25 核估算佔用。
- **觸發這次修改**：Unimatrix 的 `inference` 已用到 924 MB／1 GB，整個專案需要 16 GB。Runner 節點 cafeca-dev-004 有 125 GB 記憶體、20 核，但它是共用工作站。

用戶決定：

- Admin 為專案設定合計上限；
- 服務在 compose 自行宣告用量，只要合計不超過上限就採用。

## 決策

### 1. 專案上限（Admin）

- `projects` 新增 `memory_limit_mb` 與 `cpu_limit`，可為 NULL。
- 設為 NULL 時維持現狀：每個服務最多 1 GB、1 核。
- 有值時，表示專案所有服務的**合計**上限。
- 只有 admin+ 能設定：`PUT /api/projects/{id}/limits`，寫入稽核 `project_limits`。
- 範圍：記憶體 1024–131072 MB；CPU 1–64 核。

### 2. 服務的用量（compose）

- **記憶體**：讀 `mem_limit` 或 `deploy.resources.limits.memory`。
  - 單位接受 `b`、`k`、`m`、`g`（不分大小寫，可加 `b`），也可以有小數，例如 `1.5g`；
  - 沒有單位時視為 bytes；
  - 下限 64 MB。
- **CPU**：讀 `cpus` 或 `deploy.resources.limits.cpus`，下限 0.1。
- 兩處都有寫時取較小值。
- 沒有宣告的服務以 1 GB、1 核計入合計。

**檢查規則**：

- 沒有專案上限：每個服務的宣告不可超過 1 GB、1 核。
- 有專案上限：合計不可超過上限。
- 超過時的錯誤訊息：「`inference` 等服務合計 17 GB，超過專案上限 16 GB」或「每個服務最多 1 GB；需要更多請聯絡 Admin」。

**檢查時機**：

- 確認設定（`PUT analysis`）；
- 加上 volume；
- 部署與驗證：組成部署內容（`target_for`）時檢查。上限被調低後再部署，會回 `409`。

### 3. 套用到容器

- `EnvSpec.limits: [{ service, memory_mb, cpu_millis }]`，加上 `serde(default)`，舊 agent 收到時忽略。
- Runner 在 override 寫入每個服務的 `mem_limit` 與 `cpus`。
- 原 compose 有 `deploy.resources.limits` 時，override 以 `!reset` 移除，避免和 `mem_limit` 衝突。
- `pids_limit` 維持 512。

### 4. 排程

環境的預留量是各服務預留量的合計：

- 有宣告的服務：以宣告值計算；
- 沒有宣告的服務：維持 512 MB／0.25 核。

驗證環境使用同樣的上限，因此驗證期間節點上會多佔用一份。

### 5. 介面

「設定」分頁新增「資源」卡，顯示：

- 每個服務的記憶體與 CPU，以及是否由 compose 宣告；
- 專案合計與上限。

Admin 可以在這張卡編輯上限；member 只能檢視，卡上提示「需要更多請聯絡 Admin」。

## 後果

- 現有專案的行為不變。
- 放寬上限需要 Admin 設定，用戶也必須在 compose 宣告，兩者缺一不可。
- 節點容量仍以靜態規格估算；共用工作站上其他服務（例如 Ollama）的用量不計入，Admin 設定時需要自行留意。
- 方案與計價（M8）可以沿用這兩個欄位。
