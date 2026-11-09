# 07 安全規範

> 版本 0.1 · 狀態：Accepted · 相關：[01 需求 NFR-SEC](01-requirements.md#3-非功能需求nfr)、[02 架構](02-architecture.md)

## 1. 威脅模型

| 資產 | 威脅 | 對策 | 需求 |
| --- | --- | --- | --- |
| 主機（docker.sock = root） | 惡意 compose 逃逸至主機 | §3 compose 政策；建議 rootless Docker／userns-remap | FR-APP-02 |
| Super Admin 權限 | 部署後被他人搶先登入 | 部署後立即登入；`CALOUD_BOOTSTRAP_ACCOUNT` | FR-AUTH-03、FR-AUTH-08 |
| 登入 | 重放簽章、仿冒網站轉用簽章 | nonce 一次性＋5 分鐘；驗證 `message.domain` 完全相同；EIP-712 綁定 chainId 與帳戶 | FR-AUTH-01、FR-AUTH-02 |
| Session | 竊取 cookie、CSRF | HttpOnly、SameSite=Strict、Secure；DB 只存雜湊；M2 檢查 Origin | FR-AUTH-05 |
| 網域 | 綁定不屬於自己的網域並簽發憑證 | DNS TXT 驗證後才進路由與申請憑證；DoH 端點只能由維運者以 `CALOUD_DOH_URL` 設定 | FR-DOM-02、FR-DOM-05 |
| 租戶隔離 | App 互相存取、搶占名稱 | 每 App 獨立 project 與 default 網路；禁止 `container_name`、`ports` | FR-APP-07 |
| 管理介面 | XSS | 前端所有動態值經 `esc()`；不使用 `eval`／inline 事件屬性 | FR-UI-04 |
| 資源 | 單一 App 耗盡主機 | 預設 `mem_limit: 1g`、`pids_limit: 512`；M3 配額 | FR-APP-12 |
| API 權杖 | 外洩後被冒用 | 只顯示一次、DB 存雜湊、預設 90 天到期、可撤銷、記錄最後使用時間；權限隨使用者角色即時變化 | FR-MCP-02、FR-MCP-03 |
| MCP 端點 | 瀏覽器跨站呼叫、DNS rebinding | `/mcp` 只接受 Bearer、不接受 cookie；`Origin` 不符即拒絕 | FR-MCP-03 |
| MCP 工具 | AI 助手誤刪資源 | 刪除類工具標示 `destructiveHint` 並要求 `confirm: true`；所有操作寫入稽核並標示 `via=mcp` | FR-MCP-05、FR-MCP-07 |
| 內部資訊 | 錯誤訊息洩漏 | 500 固定訊息，細節只寫 log | NFR-SEC-03 |

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

## 4. 機密處理

| 項目 | 規則 |
| --- | --- |
| session token | 256 bit，`new_token()` 產生；DB 存 keccak256 雜湊 |
| API 權杖 | `cld_` ＋ 256 bit，`new_token()` 產生；DB 存 keccak256 雜湊與前 8 字元（供辨識）；只在建立時回傳一次 |
| nonce | 128 bit，`new_nonce()` 產生 |
| log | 不記錄 token、cookie、完整簽章、compose 中的環境變數值 |
| 設定檔 | `.env` 不進版控（`.gitignore`）；只提交 `.env.example` |

## 5. 安全相關變更的審查要求

觸及以下任一項的 PR 需**兩位**審查者，其中一位熟悉本文件：

- `caloud-auth` 全部
- `caloud-orchestrator::policy`
- `caloud-api::auth`、`service.rs`、`tokens.rs`、`mcp.rs` 中的權限與驗證
- `deploy/`、`Dockerfile`
- 本文件
