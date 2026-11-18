# ADR 0014：新增 `caloud-analyzer` crate（服務自動解析）

- 狀態：Accepted
- 日期：2026-11-14
- 相關需求：FR-AN-01～FR-AN-06

## 背景

平臺專案從 GitHub 儲存庫匯入後，要自動解析出服務、port、附加資料庫與環境變數，產生一份通過 [07 §3.6](../07-security.md#36-平臺-app-政策) 的 compose 與解析報告（設計稿 §6）。解析的輸入是不受信任的儲存庫內容，規則多、需要大量以檔案為輸入的測試，與部署（`caloud-orchestrator`）及 API（`caloud-api`）的職責不同。

以 5 個真實儲存庫實測時發現：iSunFA 與 SwarmStorage 的 compose 只有附加服務（postgres、ipfs），App 本身沒有寫在 compose 裡。照「找到第一個來源就停」的順序，App 會被漏掉。

## 決策

- 新增 crate `caloud-analyzer`：純同步函式庫，入口 `analyze(repo, &Options) -> Analysis`。依賴 `caloud-orchestrator`（重用 `prepare_platform`）與 `toml`。
- 只讀檔、不執行儲存庫內任何程式、不連網；讀檔限制見 [07 §1](../07-security.md#1-威脅模型)。
- 範本產生的 Dockerfile 以 compose 的 `build.dockerfile_inline` 帶入，不寫入儲存庫，也不另存檔案。
- 附加服務所需的密碼以 `${CALOUD_DB_PASSWORD}` 等變數引用，由平臺產生並以秘密變數注入；compose 內不出現秘密值。
- **合併規則**：儲存庫 compose 中沒有任何 `build` 服務、但根目錄有語言標記檔時，以範本產生 App 服務並與 compose 合併；此判斷標示為中信心。
- `Analysis` 可序列化，之後原樣存入 analyses 表（M5-05）。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **獨立 crate**（採用） | 職責單一、測試獨立、之後可在 Runner 端執行 | workspace 多一個 crate |
| 放在 `caloud-orchestrator` | 少一個 crate | 部署與解析混在一起；orchestrator 會依賴 toml 等解析用套件 |
| 產生的 Dockerfile 寫入建置目錄 | 不依賴 compose 新功能 | 需要另外管理檔案；與儲存庫內容混淆 |

## 影響

- [02 架構](../02-architecture.md) 的相依圖新增 `caloud-analyzer ──► caloud-orchestrator`。
- `dockerfile_inline` 需要 Docker Compose 2.17 以上；Runner 的 BuildKit 環境（M4-09）須符合。
- [01 需求](../01-requirements.md) FR-AN-01 補上合併規則。

## 增補：持久化、系統套件與建置時變數（2026-11-18，M5-08）

相關需求：FR-AN-07、FR-AN-08、FR-AN-09。偵測規則集中在 `caloud-analyzer` 的 `runtime` 模組。

- **來源分工**：
  - 範本來源：解析器直接修改產出的 Dockerfile 與 compose；
  - compose／Dockerfile 來源：只產生判斷，不修改用戶的 Dockerfile 或 compose。
- **系統套件與原生模組**（FR-AN-08）：
  - 範本依依賴加裝。Node 與 Python 在安裝依賴前執行一次 `apt-get install --no-install-recommends`；Playwright 以框架指令安裝 Chromium；Go 的 cgo 依賴改用 `CGO_ENABLED=1` 與 debian runtime；Rust 依賴 `openssl-sys` 時加裝 `libssl-dev`／`libssl3`。
  - 儲存庫根目錄的 `Aptfile` 列出的套件一併加裝。
  - 用戶自己的 Dockerfile 原文沒有安裝所需套件時，列為中信心。
  - 無法以範本處理者，列為中信心並建議自備 Dockerfile。
- **持久化目錄**（FR-AN-07）：
  - 偵測來源：程式碼寫入工作目錄下的相對路徑、`*_DIR`／`*_PATH` 等變數的預設值、SQLite 檔案、儲存庫中保留的空目錄。
  - 範本以具名 volume `<服務>-<目錄>` 掛到 `/app/<目錄>`。
  - compose 來源中 build 服務沒有掛載對應路徑時，列為中信心。
  - 不掛 `/tmp`、絕對路徑與建置產物目錄。
- **建置時變數**（FR-AN-09）：
  - 前綴 `NEXT_PUBLIC_`、`VITE_`、`REACT_APP_`、`NUXT_PUBLIC_`、`PUBLIC_`、`EXPO_PUBLIC_`、`GATSBY_` 視為建置時變數，`EnvVar.build_time = true`。
  - 範本以 Dockerfile `ARG` 加上 compose `build.args: NAME: ${NAME}` 傳入。
  - 名稱含網址、origin 類字詞者標示 `varies_by_env`，提示 staging 與 production 需各自建置（M7 依環境建置）。

