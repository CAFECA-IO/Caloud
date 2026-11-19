# ADR 0018：最小部署流程與 Runner 建置沙箱

- 狀態：Accepted
- 日期：2026-11-19
- 相關需求：FR-ENV-12、FR-OPS-04、FR-OPS-06、FR-OPS-11、FR-GH-01

## 背景

專案匯入並確認後（M5），流程就停住了。完整的驗證（M6）與發布（M7）需要好幾輪才能完成，在那之前用戶看不到服務實際跑起來。

這一輪先做最小可用流程：確認 → 建置 → 以 gVisor 啟動 → 健康檢查 → `https://<8 碼>.caloud.tw`。Runner 主機為 cafeca-dev-004，改為長期使用。

docs/07 §3.6 已規定：建置以 rootless BuildKit 執行，不使用主機 Docker 建置；建置可連外網，但封鎖雲端 metadata 與平臺內網。

2026-11-19 在 cafeca-dev-004 實測（Docker 29.4、gVisor 20261116.0、BuildKit v0.35.0）：

- 常駐的 rootless buildkitd 放在 runsc 中執行時，綁定掛載的 unix socket 從主機端連不到（gVisor 不對外暴露沙箱內建立的 socket）。
- 改成每次建置啟動一個 `buildctl-daemonless.sh` 容器（runsc、`--oci-worker-no-process-sandbox`），結果如下：
  - RUN 步驟的 `uname` 顯示 `4.19.0-gvisor`；
  - `apk add` 可連外網；
  - 閘道（主機 SSH）、`169.254.169.254`、區網都連不到；
  - 輸出 `type=docker` tarball 後 `docker load` 成功；
  - 小型映像約 20 秒。
- Docker 內建 DNS 在 gVisor 下不能用（同 ADR 0015），需要掛載 resolv.conf。

## 決策

### 1. 流程（FR-ENV-12）

1. 專案確認後，用戶按「部署」（`POST /api/projects/{id}/deploy`）。若專案是透過 GitHub 安裝匯入，push 到同一分支且新的解析與確認版本相同時，也會自動重新部署。
2. 控制面為專案配置一個 production 環境：8 碼子網域，之後固定不變。節點與網段沿用 M4-10 的選擇與配置。
3. 控制面送出 `Deploy`。EnvSpec 多帶 `source`：儲存庫網址、commit、根目錄、compose 目錄；私有儲存庫另附 git 授權標頭（1 小時的 installation token）。
4. agent 依序執行四個階段：
   - `fetch`：取得原始碼；
   - `build`：建置；
   - `start`：啟動（`docker compose up --wait`）；
   - `health`：健康檢查，由控制面執行。

   每個階段以 `EnvStatus.stage` 回報，輸出以 `EnvLog` 逐行回報。
5. 健康檢查：控制面經通道對對外服務發 `GET /`，60 秒內收到任何 HTTP 回應就算通過，接著路由生效。
6. 重新部署採「先停舊版、再啟新版」（保留 volume），會有短暫中斷；藍綠切換留給 M7-01。每個環境只保留最近 2 個映像。

### 2. 建置沙箱

- **每次建置一個臨時容器**，不常駐 buildkitd：
  ```
  docker run --rm --runtime runsc --network caloud-build
    --cpus 4 --memory 8g --pids-limit 4096
    -v <resolv.conf>:/etc/resolv.conf:ro -v <context>:/ctx:ro -v <out>:/out
    -v caloud-bkcache-<env>:/home/user/.local/share/buildkit
    -e BUILDKITD_FLAGS=--oci-worker-no-process-sandbox
    --entrypoint buildctl-daemonless.sh moby/buildkit@<固定 digest>
    build --frontend dockerfile.v0 --local context=/ctx --local dockerfile=<dir>
    --output type=docker,name=<tag>,dest=/out/image.tar
  ```
- **網路**：`caloud-build` bridge 的介面名稱是 `cldbuild`，符合 `cld+`，因此沿用 ADR 0015 的防火牆：可連外網，封鎖內網、metadata、主機與 TCP 25。網段 `10.239.0.0/24`，在環境用的 `10.240.0.0/12` 之外。
- **隔離**：
  - 不同建置之間：每次建置都是獨立的 gVisor 沙箱，沒有共用的 daemon 或 socket。
  - 快取：只在同一個環境的 volume 內共用，不跨用戶。
- **產出**：`docker load` 成 `caloud-env/<env>/<service>:<commit 前 12 碼>`。compose 改寫為 image 後，交給既有的 sandbox override（runsc、固定 IP）啟動。
- **映像**：`moby/buildkit:v0.35.0-rootless` 以 digest 固定，由 `runner-setup.sh` 預先拉取。

### 3. 權杖與 log

- installation token 只在 `Deploy` 訊息與 agent 的記憶體中。git 以 `GIT_CONFIG_*` 環境變數帶入（同 ADR 0017），不寫檔、不進 log；`EnvSource` 的 Debug 輸出遮蔽此欄位。
- log 每行最多 500 字、每個階段最多 2000 行；控制面每個環境保留最近 2000 行（`deploy_logs`）。

### 4. Runner 主機（cafeca-dev-004）

- `runner-setup.sh install` 新增以下項目，`uninstall` 一併移除：
  - 建立 `caloud-build` 網路；
  - 拉取 buildkit 映像；
  - 安裝 `caloud-agent.service`（systemd；以節點權杖檔重連）。
- 新增後台「Runner 節點」頁（原 M8-03 的一部分，提前為 M4-11）：列出節點、建立加入權杖、排空與啟用。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| 主機 Docker 直接 `docker build` | 最簡單、有快取 | 建置以 runc 執行，可連主機與內網，違反 07 §3.6 |
| 常駐 rootless buildkitd（runsc） | 快取共用、啟動快 | socket 從主機連不到；no-process-sandbox 下各建置可讀彼此的狀態目錄 |
| 常駐 buildkitd（runc）＋ TCP | 可用 | RUN 步驟能連到 daemon API，跨租戶風險 |
| **每次建置一個 gVisor 臨時容器（採用）** | 建置之間完全隔離；沿用環境防火牆 | 每次多數秒啟動；快取限同一環境 |

## 影響

- 協定：`EnvSpec.source`、`AgentMsg::EnvLog`、`EnvStatus.stage`，皆為選用欄位，舊 agent 相容。
- 資料表：`environments` 新增 `project`、`commit_sha`、`stage`；新增 `deploy_logs`。
- `caloud-runner` 使用 `caloud_orchestrator::git` 取得原始碼（ADR 0012 已允許 runner 依賴 orchestrator）。
