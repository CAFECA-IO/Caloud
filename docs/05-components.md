# 05 通用元件

> 版本 0.2 · 狀態：Accepted · 相關：[04 設計語言](04-design-language.md)、[08 開發規範](08-coding-standards.md)、[ADR 0006](adr/0006-design-system-files.md)

本文件定義**前端 UI 元件**與**後端共用模組**。新功能必須優先組合既有元件；需要新元件時，先在本文件新增規格，再到 `web/ds/components.css` 實作，並加入 `/styleguide`。

---

## A. 前端 UI 元件

### A.1 檔案與命名

| 檔案 | 內容 |
| --- | --- |
| `web/ds/tokens.css` | 基礎 Token（[04 §2.1](04-design-language.md#21-基礎-tokentokenscss)） |
| `web/ds/theme.css` | 語意 Token（[04 §2.2–2.3](04-design-language.md#22-語意-token色彩)） |
| `web/ds/components.css` | 本節所有元件 |
| `web/ds/ui.js` | 共用函式與圖示（§A.4、§A.5） |
| `web/index.html` | 管理介面：只組合元件，不另寫樣式 |
| `web/styleguide.html` | 元件總覽與風格比較（§A.6） |

| 前綴 | 意義 | 例 |
| --- | --- | --- |
| `c-` | 元件 | `c-btn`、`c-card` |
| `c-x__y` | 子元素 | `c-card__title` |
| `c-x--y` | 變體 | `c-btn--primary` |
| `l-` | 版面 | `l-stack` |
| `u-` | 工具，單一用途 | `u-muted` |
| `is-` | 狀態，由 JS 切換 | `is-active` |
| `sg-` | 僅限 styleguide 頁面 | `sg-swatch` |

- `web/` 的 HTML／CSS／JS 只能出現本文件列出的 class（`scripts/check-docs.sh` 檢查）。
- `index.html` 不得有 `<style>` 或樣式屬性；需要新樣式時新增元件、變體或工具 class。
- id 只給 JS 取元素；JS 用的資料放在 `data-*` 屬性。

### A.2 元件目錄

| 元件 | class | 用途 | 需求 |
| --- | --- | --- | --- |
| Icon | `c-icon` | 24×24 線條圖示（§A.5） | — |
| AppShell | `c-shell`、`c-shell__main` | 側欄＋內容的頁面骨架 | FR-UI-11 |
| Sidebar | `c-sidebar`、`c-sidebar__brand`、`c-sidebar__mark`、`c-sidebar__nav`、`c-sidebar__item`、`c-sidebar__count`、`c-sidebar__footer` | 主導覽；`__count` 顯示待處理數；`__mark` 為品牌標誌圖（04 §4.1） | FR-UI-11 |
| Account | `c-account`、`c-account__avatar`、`c-account__text`、`c-account__name`、`c-account__role` | 側欄中的登入者 | — |
| Page | `c-page` | 內容容器 | — |
| PageHeader | `c-page-header`、`c-page-header__text`、`c-page-header__title`、`c-page-header__desc`、`c-page-header__actions` | 頁面標題、說明、主要操作 | FR-UI-01 |
| SectionTitle / Label | `c-section-title`、`c-label` | 區塊標題／小標籤 | — |
| Button | `c-btn`、`c-btn--primary`、`c-btn--ghost`、`c-btn--danger`、`c-btn--sm`、`c-btn--icon` | 操作 | — |
| Field | `c-field`、`c-field__label`、`c-field__hint`、`c-field__error` | 標籤＋輸入＋說明＋錯誤 | — |
| Input / Select / Textarea | `c-input`、`c-select`、`c-textarea`、`c-textarea--sm` | 表單輸入；錯誤時設 `aria-invalid="true"`；`--sm` 為較矮的多行輸入（環境變數） | — |
| Checkbox | `c-check` | 勾選項目（`<label class="c-check"><input type="checkbox">說明</label>`） | FR-APP-16、FR-DOM-09 |
| Search | `c-search` | 帶圖示的搜尋框 | FR-UI-08 |
| Segmented | `c-segmented`、`c-segmented__item` | 篩選切換（`aria-pressed`） | FR-UI-08 |
| Tabs | `c-tabs`、`c-tabs__item`、`c-tabs__count` | Drawer 內分頁（`role="tab"`）；頁面分頁以連結（`aria-current="page"`），`__count` 標示待處理數 | FR-UI-10、FR-UI-14 |
| Switch | `c-switch`、`c-switch__track`、`c-switch__text` | 開關設定（`<input type="checkbox" role="switch">`）；取代開關用途的 Checkbox | FR-UI-14 |
| Diff | `c-diff`、`c-diff__line`、`c-diff__line--add`、`c-diff__line--del` | compose 等文字的逐行差異 | FR-UI-14、FR-ENV-15 |
| Flow | `c-flow`、`c-flow__step`、`c-flow__label`、`c-flow__meta` | 驗證、部署的階段流程條；步驟加 `is-done`／`is-current`／`is-failed` | FR-UI-14 |
| Menu | `c-menu`、`c-menu__list`、`c-menu__item`、`c-menu__item--danger` | 「更多」選單（`<details>`） | FR-UI-14 |
| Card | `c-card`、`c-card__header`、`c-card__title`、`c-card__body`、`c-card__body--flush` | 內容分區；`--flush` 給表格 | — |
| Stat | `c-stat`、`c-stat__label`、`c-stat__value`、`c-stat__meta` | 總覽數字卡，可為連結 | FR-UI-09 |
| Table | `c-table-wrap`、`c-table`、`c-table__primary`、`c-table__secondary`、`c-table__actions` | 資料列表；可點擊列加 `is-clickable` | — |
| Badge | `c-badge`、`c-badge__dot`、`c-badge--success`、`c-badge--warning`、`c-badge--danger`、`c-badge--accent`、`c-badge--pulse` | 狀態（StatusPill） | FR-UI-04 |
| Banner | `c-banner`、`c-banner__body`、`c-banner--warning`、`c-banner--danger` | 頁面層級提示、待處理事項 | FR-UI-09 |
| InlineMessage | `c-msg`、`c-msg--danger` | 頁面載入錯誤 | — |
| EmptyState | `c-empty`、`c-empty__title` | 空列表＋下一步 | — |
| Skeleton | `c-skeleton` | 載入中佔位 | FR-UI-05 |
| Code / Log | `c-code`、`c-log` | 行內程式碼／多行 log | FR-APP-06 |
| CopyField | `c-copy`、`c-copy__value` | 可一鍵複製的值 | FR-UI-06 |
| KeyValue | `c-kv`、`c-kv__key`、`c-kv__value` | 詳細資料的欄位列表 | FR-UI-10 |
| Stepper | `c-stepper`、`c-stepper__step`、`c-stepper__index`、`c-stepper__content`、`c-stepper__label` | 多步驟流程；步驟加 `is-done`／`is-current` | FR-UI-07 |
| Drawer | `c-drawer`、`c-drawer__header`、`c-drawer__heading`、`c-drawer__title`、`c-drawer__body`、`c-drawer__footer` | 右側面板，檢視／編輯單一資源 | FR-UI-10 |
| ConfirmDialog | `c-dialog`、`c-dialog--wide`、`c-dialog__body`、`c-dialog__title`、`c-dialog__actions` | 破壞性操作確認；`--wide`（640px）放差異等較寬內容 | FR-UI-03 |
| Toast | `c-toast-region`、`c-toast`、`c-toast--success`、`c-toast--danger` | 操作結果短暫回饋 | FR-UI-05 |
| Login | `c-login`、`c-login__panel`、`c-login__mark`、`c-login__title`、`c-login__desc` | 登入與等待核准畫面；`__mark` 為品牌標誌圖（04 §4.1） | FR-AUTH-01、FR-USER-04 |
| Layout | `l-stack`、`l-row`、`l-grid`、`l-spacer` | 垂直堆疊／水平排列／自動欄格／撐開空間 | — |
| Utilities | `u-muted`、`u-small`、`u-mono`、`u-nowrap`、`u-truncate`、`u-hidden`、`u-sr-only` | 次要色／小字／等寬／不換行／截斷／隱藏／僅報讀器 | — |
| States | `is-active`、`is-current`、`is-done`、`is-failed`、`is-clickable`、`is-loading` | 由 JS 切換 | — |

### A.3 元件規格

#### Button

```html
<button class="c-btn c-btn--primary"><svg class="c-icon">…</svg><span>新增 App</span></button>
<button class="c-btn">停止</button>
<button class="c-btn c-btn--ghost c-btn--icon c-btn--sm" aria-label="重新整理" title="重新整理">…</button>
<button class="c-btn c-btn--danger">刪除</button>
```

| 項目 | 規格 |
| --- | --- |
| 尺寸 | 高 `--control-height`（`--sm` 為 28px），水平內距 `--space-3`，字重 500，圓角 `--radius-control` |
| 變體 | 次要（預設）：surface 底＋強邊框；`--primary`：`--color-accent` 底；`--ghost`：透明；`--danger`：危險色字，與 `--primary` 併用為實心危險 |
| 狀態 | hover、`:disabled`（0.55 透明）、`is-loading`（由 `busy()` 設定，圖示旋轉）、`:focus-visible` |
| 規則 | 每個畫面最多一個 `--primary`；`--danger` 必搭配 ConfirmDialog；只有圖示時必有 `aria-label`＋`title` |

#### Field

```html
<label class="c-field">
  <span class="c-field__label">名稱</span>
  <input class="c-input" placeholder="例如 blog">
  <span class="c-field__hint">留空時以第一個 service 命名。</span>
  <span class="c-field__error" role="alert"></span>
</label>
```

錯誤：輸入元件設 `aria-invalid="true"`，訊息寫入 `c-field__error`（空字串時隱藏）。

#### Table

```html
<section class="c-card"><div class="c-card__body c-card__body--flush"><div class="c-table-wrap"><table class="c-table">
  <thead><tr><th>名稱</th><th>狀態</th><th><span class="u-sr-only">操作</span></th></tr></thead>
  <tbody><tr class="is-clickable" tabindex="0" data-id="…">
    <td><div class="c-table__primary">blog</div><div class="c-table__secondary u-mono">3f9a1c2e</div></td>
    <td>…Badge…</td>
    <td><div class="c-table__actions">…ghost icon buttons…</div></td>
  </tr></tbody>
</table></div></div></section>
```

- 表頭在 `--color-surface-sunken` 上，`--text-xs`／500／muted。
- 第一欄為主要文字＋次要資訊（ID、網域）。最後一欄為操作，使用 ghost icon button。
- 整列可點擊時開啟 Drawer；列內按鈕不觸發開啟。
- 載入中以 `UI.skeletonRows(cols)` 填入。

#### Badge

`UI.statusPill(kind, value)` 產生，不可手寫對照（[04 §6.3](04-design-language.md#63-狀態文字)）。部署中使用 `--pulse`。

#### Drawer

由 `UI.drawer({ title, subtitle, onClose })` 建立，回傳 `{ el, body, footer, close }`。Footer 預設隱藏，需要時移除 `u-hidden`。Footer 排列：破壞性操作在左、`l-spacer`、次要、主要在右。

#### Switch

```html
<label class="c-switch">
  <span class="c-switch__text">確認後立即重新部署<span class="u-small u-muted">關閉時，下次部署才生效</span></span>
  <input type="checkbox" role="switch"><span class="c-switch__track" aria-hidden="true"></span>
</label>
```

- 開關型設定（立即生效或對話框內的選項）一律使用 Switch；Checkbox 只用於「我已確認」這類勾選與多選（ADR 0024）。
- 軌道 44×24，開啟時 `--color-accent`；`:focus-visible` 顯示在軌道上；`:disabled` 0.55 透明。
- 立即生效的開關：變更失敗時還原並以 Toast 顯示錯誤。

#### Diff

```html
<pre class="c-diff" aria-label="compose 修改內容"><span class="c-diff__line">services:</span><span class="c-diff__line c-diff__line--add">+   - web-data:/app/.data</span></pre>
```

- 每行一個 `__line`；`--add` 為成功色淺底、`--del` 為危險色淺底，行首保留 `+`／`-`，不只靠顏色區分。
- 等寬字、可水平捲動；由 `UI.diffLines(before, after)` 產生。

#### Flow

```html
<ol class="c-flow" aria-label="驗證階段">
  <li class="c-flow__step is-done"><span class="c-flow__label">取得</span><span class="c-flow__meta">0:12</span></li>
  <li class="c-flow__step is-current" aria-current="step"><span class="c-flow__label">建置</span><span class="c-flow__meta">6:40</span></li>
  <li class="c-flow__step"><span class="c-flow__label">測試</span></li>
</ol>
```

- 水平排列、可換行；完成為成功色、進行中為強調色（`--pulse` 動畫遵守 reduced motion）、`is-failed` 為危險色。

#### Menu

```html
<details class="c-menu"><summary class="c-btn">更多</summary>
  <div class="c-menu__list" role="menu"><button class="c-menu__item" role="menuitem">重新解析</button><button class="c-menu__item c-menu__item--danger" role="menuitem">刪除專案</button></div>
</details>
```

- 次要或危險的操作收在「更多」；點選項目後關閉選單。危險項目仍須 ConfirmDialog。

#### Stepper（網域設定）

```html
<ol class="c-stepper">
  <li class="c-stepper__step is-done"><span class="c-stepper__index">✓</span><div class="c-stepper__content"><span class="c-stepper__label">輸入網域</span>…</div></li>
  <li class="c-stepper__step is-current">…</li>
  <li class="c-stepper__step">…</li>
</ol>
```

只有目前步驟顯示輸入元件；完成的步驟顯示結果摘要。

#### CopyField

`UI.copyField(value, label)` 產生；點擊後複製並以 Toast 回饋「已複製<label>」。用於 DNS 紀錄、驗證值、ID。

#### Stat

```html
<a class="c-stat" href="#apps"><span class="c-stat__label">運作中的 App</span><span class="c-stat__value">12<span class="u-muted u-small"> / 14</span></span><span class="c-stat__meta">…</span></a>
```

可點擊時使用 `<a>` 指向對應頁面。

### A.4 前端共用函式（`web/ds/ui.js`，全域 `UI`）

| 函式 | 說明 |
| --- | --- |
| `$(sel, root)`、`$$(sel, root)` | 查詢元素 |
| `esc(s)` | HTML 轉義；**所有插入 innerHTML 的動態值都必須經過** |
| `api(path, opts)` | 呼叫後端；非 2xx 時 throw `Error(伺服器訊息)`，含 `status` |
| `ts(unix)`、`relTime(unix)` | 完整時間／相對時間（`<time>`，title 為完整時間） |
| `statusPill(kind, value)` | Badge HTML |
| `showMsg(el, err)` | 顯示或清除 InlineMessage／欄位錯誤 |
| `toast(message, tone)` | Toast；`tone` 為 `success`（預設）或 `danger` |
| `confirmDialog({ title, body, html, confirmText, tone, wide, setup })` | 回傳 `Promise<boolean>`；不得使用 `window.confirm`。`html` 為已轉義的內容（取代 `body`），`setup(dialog)` 可綁定事件（例：Switch 改按鈕文字），`wide` 使用 `c-dialog--wide` |
| `switchHtml({ id, label, hint, checked })` | Switch HTML |
| `diffLines(before, after)` | 逐行差異的 `c-diff` HTML（ADR 0024） |
| `drawer({ title, subtitle, onClose })` | 開啟 Drawer |
| `busy(button, promise)` | 處理中狀態 |
| `copy(text, label)`、`copyField(value, label)` | 複製／CopyField HTML |
| `empty({ iconName, title, body, action })` | EmptyState HTML |
| `skeletonRows(cols, rows)` | 表格 Skeleton 列 |
| `icon(name)` | 圖示 SVG |
| `initTheme()`、`toggleTheme()` | 淺色／深色（記在本機） |

### A.5 圖示

`UI.icon(name)`；24×24、線寬 1.75、`currentColor`，裝飾用時 `aria-hidden`。

| 名稱 | 用途 |
| --- | --- |
| `home` `branch` `box` `globe` `users` | 導覽：總覽、專案、Apps、網域、使用者 |
| `plus` `upload` `play` `stop` `refresh` `trash` `link` `check` `x` `copy` `search` | 操作 |
| `alert` `info` `shield` `inbox` `loader` | 狀態與提示 |
| `logout` `sun` `moon` `key` | 帳戶與設定、API 權杖 |

新增圖示：在 `ui.js` 的 `ICONS` 加入路徑並更新本表；只使用簡單幾何線條，不引用外部圖示庫。

### A.6 Styleguide 頁面

`/styleguide`（`web/styleguide.html`）列出所有元件與色彩 Token 的實際值（可切換淺色／深色）。新增或修改元件時同步更新。

專用 class（不得用於 `index.html`）：`sg-bar`、`sg-section`、`sg-swatches`、`sg-swatch`、`sg-swatch__chip`、`sg-demo`。

---

## B. 後端共用模組

新程式碼必須使用以下模組，不得重複實作同樣功能。

| 模組 | 位置 | 用途 | 使用規則 |
| --- | --- | --- | --- |
| `Config` | `caloud-core::config` | 環境變數設定 | 新設定一律加在這裡並更新 README 設定表 |
| `now()` | `caloud-core` | 目前 Unix 秒 | 不直接呼叫 `SystemTime` |
| `project_name(app_id)`、`model_upstream_host(app_id, service)` | `caloud-core` | compose project 名稱與 Edge alias | 命名規則只能定義在此 |
| `db::connect`、`db::upsert_login`、`db::load_routes` | `caloud-core::db` | 資料庫連線、登入、路由查詢 | 跨模組重複使用的查詢放這裡 |
| `db::audit(db, actor, action, target, detail)` | `caloud-core::db` | 稽核紀錄 | FR-AUD-01 列出的動作都必須呼叫；`action` 用 snake_case `<資源>_<動詞>` |
| `Role` | `caloud-core::model` | 角色與權限判斷 | 權限判斷只用 `can_deploy()`／`is_admin()`，不比對字串 |
| `CafecaVerifier` | `caloud-auth` | 登入驗證 | 唯一驗證入口 |
| `new_nonce()`、`new_token()`、`new_subdomain_id(len)` | `caloud-auth` | 安全隨機值、子網域 ID | 不自行用 `rand` 產生秘密值或 ID |
| `subdomain::validate_id`、`subdomain::IdRules`、`EnvKind` | `caloud-core::subdomain` | 子網域 ID 規則（FR-ENV-07） | 產生或接受 ID 前一律檢查；可用性（未使用、未封鎖）由 `service::admin::new_env_id` 查詢 |
| `placement::choose_node` | `caloud-core::placement` | 依容量選節點（FR-OPS-08） | 所有派工都經此函式 |
| `agent::{AgentMsg, ServerMsg, NodeSpecs}` | `caloud-core::agent` | Agent 協定型別（[06 §6](06-api-spec.md#6-agent-協定)） | 控制面與 `caloud-agent` 共用，不另定義 |
| `tunnel::{ws_io, request_connect, parse_connect, forbidden_ip}` | `caloud-core::tunnel` | Runner 通道協定（[06 §6.1](06-api-spec.md#61-通道m4-07)） | 控制面與 `caloud-agent` 共用，不另寫 `CONNECT` 解析 |
| `TunnelRegistry::dial(node, target)` | `caloud-proxy` | 連到上游：有節點走通道、否則 TCP | Edge（HTTP、TLS 直通）連上游一律經此；節點移除時呼叫 `remove` |
| `policy::validate(yaml)` | `caloud-orchestrator` | compose 檢查 | 任何使用 compose 內容前都要先呼叫 |
| `analyze(repo, &Options)` | `caloud-analyzer` | 平臺專案的服務自動解析（FR-AN-01～06） | 解析儲存庫的唯一入口；不自行讀儲存庫檔案判斷服務；結果的 compose 已經過 `prepare_platform` |
| `sandbox::{EnvNet, env_override, service_ips, check_image_only}` | `caloud-orchestrator` | 平臺環境的 gVisor、網段、固定 IP、名稱解析 override（[ADR 0015](adr/0015-runner-sandbox.md)） | 控制面與 agent 共用；環境網段與 IP 只在此計算 |
| `envs::EnvRunner`、`selfcheck::check` | `caloud-runner` | agent 端環境部署／刪除與自我檢查（FR-OPS-12、FR-OPS-13） | agent 執行 docker 只經 `EnvRunner`；自我檢查未通過不接平臺環境 |
| `service::environments::*` | `caloud-api::service` | 平臺環境的開出、刪除與 agent 回報（FR-OPS-13） | 只經 AgentHub 送出 `deploy`／`destroy`，路由由 `load_routes` 產生 |
| `policy::prepare_platform(yaml, depth)` | `caloud-orchestrator` | 平臺 App 的 compose 轉換與檢查（[07 §3.6](07-security.md#36-平臺-app-政策)） | 平臺專案的 compose 一律經此，不直接用 `validate_with` |
| `ComposeRunner` | `caloud-orchestrator` | 部署／停止／刪除／狀態／log | 唯一可執行 `docker` 指令的地方；執行檔由 `Config.docker_bin` 指定 |
| `RouteTable::reload(db)` | `caloud-proxy` | 重新載入路由 | 網域或 App 狀態改變後必呼叫（FR-DOM-04） |
| `AppInput`、`BindInput` | `caloud-api::service` | App 建立／更新、網域綁定的輸入（REST 與 MCP 共用） | 新欄位只加在這裡，兩個入口同時生效 |
| `GitSource`、`DeploySpec` | `caloud-orchestrator::compose` | Git 來源與一次部署的設定 | 部署前先 `fetch_git` 並依政策檢查 |
| `git::fetch_repo(git_bin, dir, url, ref)` | `caloud-orchestrator` | 淺層取得公開儲存庫並回傳 commit 與分支；套用 [07 §1](07-security.md#1-威脅模型) 的 git 安全設定與大小上限 | 取得任何儲存庫都經此（Git 來源 App、平臺專案），不另組 git 指令 |
| `service::projects::*` | `caloud-api::service` | 平臺專案的匯入、解析、確認（FR-GH-04、FR-AN-10） | 解析在背景任務以 `spawn_blocking` 呼叫 `caloud_analyzer::analyze` |
| `service::*` | `caloud-api::service` | App、網域、使用者的業務操作（權限、驗證、稽核） | REST handler 與 MCP 工具**只能**經由服務層操作資源，不得直接寫 SQL 或呼叫 `ComposeRunner` |
| `tokens::authenticate(state, bearer)` | `caloud-api::tokens` | API 權杖驗證，回傳 `CurrentUser` | `/mcp` 唯一的身分來源 |
| `ApiError`、`ApiResult<T>` | `caloud-api::error` | 錯誤型別與 HTTP 對應 | handler 一律回傳 `ApiResult`；訊息依 [04 §5.5](04-design-language.md#55-錯誤訊息) |
| `CurrentUser` | `caloud-api::auth` | 取得登入者與來源（`via`） | 需要登入的 handler 以參數取得；`require_deploy()`／`require_admin()`／`can_manage(owner)` 做權限檢查；稽核使用 `audit_detail()` 附上來源 |
| `spawn_deploy(state, app, compose)` | `caloud-api::apps` | 背景部署並更新狀態與路由 | 觸發部署的唯一方式 |
