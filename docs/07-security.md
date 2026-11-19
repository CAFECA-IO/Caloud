# 07 安全規範

> 版本 0.1 · 狀態：Accepted · 相關：[01 需求 NFR-SEC](01-requirements.md#3-非功能需求nfr)、[02 架構](02-architecture.md)

## 1. 威脅模型

| 資產 | 威脅 | 對策 | 需求 |
| --- | --- | --- | --- |
| 主機（docker.sock = root） | 惡意 compose 逃逸至主機 | §3 compose 政策；建議 rootless Docker／userns-remap | FR-APP-02 |
| Super Admin 權限 | 部署後被他人搶先登入 | 部署後立即登入；`CALOUD_BOOTSTRAP_ACCOUNT` | FR-AUTH-03、FR-AUTH-08 |
| 登入 | 重放簽章、仿冒網站轉用簽章 | nonce 一次性＋5 分鐘；驗證 `message.domain` 完全相同；EIP-712 綁定 chainId 與帳戶 | FR-AUTH-01、FR-AUTH-02 |
| Session | 竊取 cookie、CSRF | HttpOnly、SameSite=Strict、Secure；DB 只存雜湊；M2 檢查 Origin | FR-AUTH-05 |
| 特權 App | 經由 build、host 網路或主機連接埠影響主機 | 只限 Admin；仍禁止 privileged、主機路徑掛載等（§3.5）；建立與修改寫入稽核 | FR-APP-16 |
| TLS 直通 | 直通網域的流量 Caloud 無法檢視 | 只轉發已驗證網域；SNI 不符或未綁定即關閉連線 | FR-DOM-09 |
| 網域 | 綁定不屬於自己的網域並簽發憑證 | DNS TXT 驗證後才進路由與申請憑證；DoH 端點只能由維運者以 `CALOUD_DOH_URL` 設定 | FR-DOM-02、FR-DOM-05 |
| 租戶隔離 | App 互相存取、搶占名稱 | 每 App 獨立 project 與 default 網路；禁止 `container_name`、`ports` | FR-APP-07 |
| 管理介面 | XSS | 前端所有動態值經 `esc()`；不使用 `eval`／inline 事件屬性 | FR-UI-04 |
| 資源 | 單一 App 耗盡主機 | 預設 `mem_limit: 1g`、`pids_limit: 512`；M3 配額 | FR-APP-12 |
| API 權杖 | 外洩後被冒用 | 只顯示一次、DB 存雜湊、預設 90 天到期、可撤銷、記錄最後使用時間；權限隨使用者角色即時變化 | FR-MCP-02、FR-MCP-03 |
| MCP 端點 | 瀏覽器跨站呼叫、DNS rebinding | `/mcp` 只接受 Bearer、不接受 cookie；`Origin` 不符即拒絕 | FR-MCP-03 |
| MCP 工具 | AI 助手誤刪資源 | 刪除類工具標示 `destructiveHint` 並要求 `confirm: true`；所有操作寫入稽核並標示 `via=mcp` | FR-MCP-05、FR-MCP-07 |
| 內部資訊 | 錯誤訊息洩漏 | 500 固定訊息，細節只寫 log | NFR-SEC-03 |
| 平臺網域（[ADR 0010](adr/0010-platform.md)） | 用戶頁面讀寫主控台 cookie；用戶之間互設 cookie；釣魚利用官方信譽 | 用戶環境只在 `*.caloud.tw` 與用戶自己的網域；主控台在 `caloud.cafeca.io`；caloud.tw 加入 Public Suffix List；封鎖清單立即停用 | FR-OPS-03 |
| 子網域 ID | 猜到或撞到別人的網址 | CSPRNG 產生；排除使用中、保留字、封鎖清單；production ID 刪除後保留才釋出；子網域不當作存取控制 | FR-ENV-07 |
| Runner 加入 | 冒充節點接收用戶工作與秘密 | 加入權杖一次性、最長 1 小時、只存雜湊；節點權杖只存雜湊、移除節點即失效；第一則訊息不是有效 `register` 即關閉 | FR-OPS-04 |
| Runner 回報 | 節點竄改其他節點狀態 | 連線綁定單一節點，心跳只更新自己的那一列；agent 回報的數值只用於調度，不作權限判斷 | FR-OPS-04 |
| Runner 通道 | 冒充節點接收流量；經通道打控制面內網 | 只接受有效節點權杖；只有控制面能開 stream；agent 只連 `CONNECT` 指定位址並拒絕 metadata 與控制面主機 | FR-OPS-09 |
| 匯入公開儲存庫 | 以網址夾帶帳密或非 https 協定讀取內網；git hook、symlink、LFS 在取得時執行或讀取主機資料；超大儲存庫塞滿磁碟 | 只接受 `https://` 且不可帶帳密；git 以 `protocol.allow=never`＋`protocol.https.allow=always`、`core.symlinks=false`、`core.hooksPath=/dev/null`、`GIT_LFS_SKIP_SMUDGE=1` 執行，不提示帳密；淺層取得、逾時 120 秒；checkout 後超過 1 GB 即刪除並回錯誤；每位用戶最多 20 個專案 | FR-GH-04、FR-AN-10 |
| 服務解析 | 惡意儲存庫以 symlink 讀主機檔案、以超大或大量檔案拖垮控制面、讓解析執行程式碼 | 不跟隨 symlink、只讀根目錄內的檔案；單檔 1 MB、最多 5,000 個檔案；略過 `node_modules`、`.git`、`target`、`dist`、`.next`、`vendor`；只讀不執行；產出仍須通過 §3.6 | FR-AN-01、FR-AN-05 |
| 用戶自有設備 | 被入侵的設備取得其他用戶的工作或內網 | 只派送擁有者的工作與秘密；通道只轉發擁有者的網域，不提供反向進平臺內網的路徑 | FR-DEV-02、FR-OPS-08 |
| 遷移 agent | 冒用遷移權杖上傳偽造資料；在舊主機取得過多權限 | 遷移權杖一次性、只綁定單一遷移、過期作廢；agent 只連出；資料庫只需唯讀帳號；寫入凍結指令由用戶自行設定 | FR-MIG-03 |
| 遷移快照 | 資料庫與檔案（含個資）在傳輸或保存時外洩 | TLS 傳輸；平臺端加密保存；只供該專案的遷移環境還原；遷移完成或取消後依保留期刪除 | FR-MIG-03、FR-MIG-05 |
| 自訂網域憑證委派 | 委派的 `_acme-challenge` 被用來替他人簽發憑證 | 委派目標名稱含隨機值且只綁定該網域；只在網域 TXT 驗證通過後申請 | FR-MIG-02 |
| hosts 預覽 | 預覽影響其他用戶或被當作正式站 | 只有改 hosts 的那台電腦會連到 Caloud；遷移環境的寫入會在最後同步時被舊站資料覆蓋，驗收清單提示不要在預覽中做正式交易 | FR-MIG-07 |
| 執行用戶程式碼 | 容器逃逸、打內網或 metadata | 控制面與 Runner 分機；gVisor（runsc）；每個環境獨立 `cld<索引>` bridge 與固定網段；防火牆 `CALOUD-ENV-FWD` 丟棄往 10/8、172.16/12、192.168/16、169.254/16、100.64/10 與 TCP 25 的轉送，`CALOUD-ENV-IN` 丟棄環境連往 Runner 主機本身的封包（[ADR 0015](adr/0015-runner-sandbox.md)）；agent 收到的 compose 再以平臺政策檢查一次，且 M4 不接受 `build` | FR-OPS-11 |
| Runner 自我檢查 | 未設定隔離的主機接到用戶環境 | 啟動時檢查 runsc、防火牆 chain、磁碟，並以兩個探測環境實際驗證；未通過的節點不被調度平臺環境 | FR-OPS-12 |
| Runner 主機設定 | 設定腳本破壞主機上其他服務 | `runner-setup.sh` 只新增 Caloud 自己的 chain、檔案與 daemon.json 的 `runtimes.runsc`，預設 runtime 不變，以 reload 生效不重啟容器；`uninstall` 完整移除 | FR-OPS-11 |
| Edge TLS 與憑證（[ADR 0016](adr/0016-edge-tls.md)） | 替他人網域簽發憑證；終結錯誤網域的流量 | HTTP-01 只對 TXT 驗證通過、非直通且綁定運作中 App 的網域申請；DNS-01 只對設有權杖的網域池根網域；SNI 沒有直通設定也沒有憑證即關閉連線；wildcard 只配對一層 | FR-DOM-05、FR-OPS-10 |
| GitHub webhook（[ADR 0017](adr/0017-github-app-public-console.md)） | 偽造事件觸發大量解析；重放 | HMAC-SHA256 驗簽（常數時間比較）、delivery 去重、本體 5 MB 上限、每 IP 速率限制 | FR-GH-02 |
| GitHub 安裝綁定 | 以他人的 `installation_id` 冒領安裝，讀取他人私有儲存庫 | 安裝時一次性 OAuth，確認安裝屬於該 GitHub 使用者；state 一次性、綁 session、10 分鐘；已綁定他人的安裝回錯誤 | FR-GH-01 |
| 主控台對外 | 跨站請求偽造、點擊劫持、暴力嘗試 | session cookie `SameSite=Strict`；寫入 API 檢查 `Origin`；CSP、`X-Frame-Options: DENY`、HSTS；登入與寫入 API 速率限制；`X-Forwarded-For` 只信任來自 Edge 的請求 | NFR-SEC-01 |
| Cloudflare 權杖 | 資料庫或備份外洩後被用來改 DNS | 以主金鑰加密保存（AAD 綁定網域池），只回傳末 4 碼；設定時先經 Cloudflare 驗證；建議權杖只給該 zone 的 DNS 編輯並限制來源 IP | FR-OPS-03、FR-OPS-14 |

## 2. 權限矩陣

| 操作 | super_admin | admin | member | pending | disabled |
| --- | --- | --- | --- | --- | --- |
| 登入、查看自己 | ✓ | ✓ | ✓ | ✓ | ✗ |
| 建立 App／網域 | ✓ | ✓ | ✓ | ✗ | ✗ |
| 管理自己的 App／網域 | ✓ | ✓ | ✓ | ✗ | ✗ |
| 管理他人的 App／網域 | ✓ | ✓ | ✗ | ✗ | ✗ |
| 查看使用者列表 | ✓ | ✓ | ✗ | ✗ | ✗ |
| 設定 member／pending／disabled | ✓ | ✓（對象非 admin） | ✗ | ✗ | ✗ |
| 授予／撤銷 admin | ✓ | ✗ | ✗ | ✗ | ✗ |
| 變更 super_admin、變更自己 | ✗ | ✗ | ✗ | ✗ | ✗ |
| 建立／撤銷自己的 API 權杖 | ✓ | ✓ | ✓ | ✗ | ✗ |
| 匯入專案、管理自己的專案 | ✓ | ✓ | ✓ | ✗ | ✗ |
| 管理他人的專案 | ✓ | ✓ | ✗ | ✗ | ✗ |
| 後台：網域資源、Runner 節點、加入權杖 | ✓ | ✓ | ✗ | ✗ | ✗ |
| 以 API 權杖使用 MCP | 同上方各列 | 同上方各列 | 同上方各列 | ✗（權杖失效） | ✗（權杖失效） |

實作：`Role::can_deploy()`、`Role::is_admin()`、`CurrentUser::can_manage(owner)`。

## 3. compose 政策

由 `caloud-orchestrator::policy::validate` 實作。本節為**正式規則清單**；`scripts/check-docs.sh` 會確認程式中每個禁用鍵都列在這裡。修改規則需同時更新本節與測試。

### 3.1 service 層禁用鍵（FORBIDDEN_KEYS）

| 鍵 | 原因 |
| --- | --- |
| `privileged` | 取得主機全部權限 |
| `cap_add` | 增加 Linux capability |
| `devices` | 存取主機裝置 |
| `device_cgroup_rules` | 存取主機裝置 |
| `security_opt` | 可關閉 seccomp／AppArmor |
| `userns_mode` | 可關閉 user namespace 隔離 |
| `cgroup_parent` | 脫離資源控制 |
| `container_name` | 與其他租戶撞名；由 Caloud 命名 |
| `ports` | 對外流量一律經 Caloud 代理；避免 port 衝突 |
| `build` | 不在主機上建置映像（讀取主機檔案） |
| `extends` | 會讀取主機上的其他檔案 |
| `env_file` | 會讀取主機檔案 |
| `sysctls` | 修改核心參數 |
| `volumes_from` | 掛載其他（可能是其他租戶的）容器 volume |

### 3.2 service 層 namespace 鍵（NAMESPACE_KEYS）

以下鍵**不可出現**（不論值為 `host` 或 `container:<x>`）：

| 鍵 | 原因 |
| --- | --- |
| `network_mode` | 共用主機或其他容器網路 |
| `pid` | 看見主機或其他容器的程序 |
| `ipc` | 共用記憶體 |
| `uts` | 共用主機名稱 namespace |
| `cgroup` | 共用 cgroup namespace |

### 3.3 其他規則

| 規則 | 說明 |
| --- | --- |
| 必須有 `image` | 每個 service 都要指定映像 |
| 只允許具名 volume 與 tmpfs | 短語法 `來源:目標` 中來源以 `/`、`.`、`~` 開頭視為 bind mount 而拒絕；長語法 `type` 須為 `volume` 或 `tmpfs` |
| 先展開 YAML 合併鍵 | 檢查前先套用 `<<: *anchor`，避免禁用鍵藏在 anchor（如 `x-*` 擴充欄位）中；寫出的 compose 也是展開後的結果 |
| 不可使用 YAML tag | `!reset`、`!override` 等 tag 會改變值的型別而繞過檢查；Caloud 自己的 override 檔不受此限 |
| 檢查欄位不可含 `$` | volume 來源與 `type`、namespace 鍵的值、`build` 的 `context`／`dockerfile`、`env_file` 路徑不可使用 `${VAR}` 變數替換，避免經 App env 或主機環境變數改寫成主機路徑或 `host` |
| 頂層 `volumes` | 不可有 `driver_opts`（可綁主機路徑）、`external` |
| 頂層 `networks` | 不可有 `external` |
| 頂層 `secrets`／`configs` | 不可使用 `file` |
| 頂層 `include` | 禁止 |

### 3.4 Caloud 自動加入的設定（override）

| 設定 | 值 |
| --- | --- |
| networks | `default` ＋ `caloud-edge`（alias `caloud-<id12>-<service>`） |
| labels | `io.caloud.app`、`io.caloud.project` |
| `mem_limit` | `1g` |
| `pids_limit` | `512` |
| `restart` | `unless-stopped` |

### 3.5 特權 App 政策

特權 App（FR-APP-16，[ADR 0009](adr/0009-system-apps.md)）只有 Admin 以上可以建立或啟用，用於部署需要主機網路的基礎服務（例如 LiveKit、iroh-relay）。特權 App 套用以下例外，**§3.1～3.3 其餘規則仍然適用**。

#### 允許的鍵（PRIVILEGED_ALLOWED）

| 鍵 | 條件 |
| --- | --- |
| `build` | 只能是 Git 來源 App；`context`、`dockerfile` 必須是相對路徑且不超出儲存庫根目錄；不可使用 `additional_contexts`、`secrets`、`ssh` |
| `network_mode` | 只能是 `host` |
| `ports` | 可發布 TCP／UDP；Caloud 保留的主機連接埠（80、443、API 埠）會被移除 |
| `env_file` | 必須是相對路徑且不超出儲存庫根目錄 |
| `tmpfs` | 允許 |

#### 仍然禁止

`privileged`、`cap_add`、`devices`、`device_cgroup_rules`、`security_opt`、`userns_mode`、`cgroup_parent`、`container_name`、`extends`、`sysctls`、`volumes_from`、`pid`／`ipc`／`uts`／`cgroup` namespace、主機路徑掛載（含 docker.sock）、頂層 `include` 與 external volume／network。

#### Caloud 對特權 App 的 override 差異

| 設定 | 一般 App | 特權 App |
| --- | --- | --- |
| `mem_limit`、`pids_limit` | 1g、512 | 不設（由 Admin 負責） |
| `restart` | 未設定時補 `unless-stopped` | 同左（已設定的 `restart` 一律保留，例如一次性的初始化 service 用 `"no"`） |
| networks | `default`＋edge | host 網路 service 不加入；其他同一般 App |
| `ports` | 禁止 | 以 `!override` 移除保留連接埠後保留其餘 |

#### Git 來源

- 只接受 `https://` 網址，不接受帳密、`file://`、`ssh://`；ref 只能是分支、標籤或 commit（`[A-Za-z0-9._/-]`）。
- 以淺層 clone 取得，部署前完整重新取得指定 ref，不保留上一次的工作目錄修改。
- 建置在主機的 Docker 上執行；建置過程可存取網路，因此只開放給 Admin。

### 3.6 平臺 App 政策

平臺用戶的專案（M5 起，FR-AN-05，實作：`Policy::platform`、`prepare_platform`）都從 Git 建置，介於一般 App 與特權 App 之間：允許 build，但不給任何主機層級的能力。容器一律以 gVisor 執行（FR-OPS-11）。

| 鍵 | 一般 App（§3.1～3.3） | 平臺 App | 特權 App（§3.5） |
| --- | --- | --- | --- |
| `build` | 禁止 | 允許；`context`、`dockerfile` 不超出儲存庫（或專案子目錄所在的儲存庫）；不可用 `additional_contexts`、`secrets`、`ssh` | 允許（同左） |
| `env_file` | 禁止 | 不讀檔：解析時把變數名稱列入盤點，值改由平臺秘密變數注入 | 允許儲存庫內路徑 |
| `ports` | 禁止 | 解析時移除，對外一律經 Edge 路由 | 允許（保留埠除外） |
| `network_mode`、`pid`、`ipc`、`uts`、`cgroup` | 禁止 | 禁止 | 只允許 `network_mode: host` |
| 主機路徑掛載、`docker.sock` | 禁止 | 禁止；需要持久化的路徑改為具名 volume（FR-AN-07） | 禁止 |
| `profiles` | 允許 | 允許；`profiles` 中的一次性 service（例如 `migrate`）可指定為發布前任務（FR-ENV-08） | 允許 |
| `deploy.resources.reservations.devices`（GPU） | 禁止 | 禁止（GPU 只考慮未來的信任設備） | 禁止 |
| 資源上限 | 1g、512 pids | 依方案（FR-CI-06 另計建置資源） | 不設 |

建置在 Runner 上以 rootless BuildKit 執行，不使用主機 Docker 的建置；建置可連外網但封鎖雲端 metadata 與平臺內網。

## 4. 機密處理

| 項目 | 規則 |
| --- | --- |
| session token | 256 bit，`new_token()` 產生；DB 存 keccak256 雜湊 |
| API 權杖 | `cld_` ＋ 256 bit，`new_token()` 產生；DB 存 keccak256 雜湊與前 8 字元（供辨識）；只在建立時回傳一次 |
| nonce | 128 bit，`new_nonce()` 產生 |
| Runner 加入權杖 | `caloud_join_` ＋ 256 bit；一次性、最長 1 小時；DB 只存 keccak256 雜湊 |
| Runner 節點權杖 | `caloud_node_` ＋ 256 bit；DB 只存 keccak256 雜湊；agent 端檔案權限 0600 |
| 子網域 ID | `new_subdomain_id()` 以 CSPRNG 產生 |
| log | 不記錄 token、cookie、完整簽章、compose 中的環境變數值 |
| 設定檔 | `.env` 不進版控（`.gitignore`）；只提交 `.env.example` |
| App 環境變數 | 存在資料庫（M7 改為以主金鑰加密，FR-APP-11）；API 與 MCP 只回傳變數名稱，不回傳值；稽核不記錄值 |
| 主金鑰 `CALOUD_MASTER_KEY` | 32 bytes 的 base64，只從環境變數讀取；不寫入資料庫、日誌、API 回應或 `Config` 的 Debug 輸出；正式機另存一份離線備份，遺失時已加密的祕密需重新設定（FR-OPS-14） |
| 加密的祕密 | `caloud_core::secret`：ChaCha20-Poly1305、隨機 nonce，格式 `v1:<base64(nonce‖密文)>`，AAD 為「用途:主鍵」 |
| Cloudflare 權杖 | 加密存於 `domain_pools.cloudflare_token_enc`；只在簽發時解開，用完即丟；稽核只記錄「已設定／已清除」 |
| GitHub App 機密 | client secret、私鑰、webhook secret 以主金鑰加密存於 `github_app`（AAD `github_app:<欄位>`）；API 只回末 4 碼；稽核只記錄「已更新」（FR-GH-05） |
| GitHub 權杖 | user token 只在安裝回呼時使用一次；installation token 只在記憶體快取至過期前 5 分鐘；git 以 `http.extraHeader` 帶入，不進網址、`.git/config` 與 log（FR-GH-01） |
| ACME 帳號金鑰與憑證私鑰 | `<data_dir>/acme/`、`<data_dir>/certs/` 下的檔案權限 0600；不經 API 輸出 |

## 5. 安全相關變更的審查要求

觸及以下任一項的 PR 需**兩位**審查者，其中一位熟悉本文件：

- `caloud-auth` 全部
- `caloud-orchestrator::policy`
- `caloud-api::auth`、`service.rs`、`tokens.rs`、`mcp.rs`、`admin.rs`、`agent.rs` 中的權限與驗證
- `caloud-runner` 的權杖保存與連線
- `caloud-core::secret`、`caloud-proxy::tls`、`caloud-api::certs`
- `caloud-api::github`、`caloud-api::guard`
- `deploy/`、`Dockerfile`
- 本文件
