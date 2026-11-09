# 04 設計語言（Design System）

> 版本 0.3 · 狀態：Accepted（正式風格：企業儀表板，見 [ADR 0007](adr/0007-ui-style-proposals.md)） · 相關：[05 通用元件](05-components.md)、[01 需求 FR-UI / NFR-A11Y](01-requirements.md)

Caloud 是給維運者用的工具。設計目標是**專業、清楚、好上手**：狀態一眼可辨，下一步永遠明確，危險操作明顯但不嚇人。

實作位置：`web/ds/`（[ADR 0006](adr/0006-design-system-files.md)）。元件總覽頁：`/styleguide`。

## 1. 設計原則

| 原則 | 說明 | 實務 |
| --- | --- | --- |
| 摘要先於細節 | 使用者最在意「有沒有正常運作、有什麼要處理」 | 首頁為總覽：Stat 卡＋「需要處理」清單；列表頁上方為篩選，細節放 Drawer |
| 狀態雙重編碼 | 顏色之外一定有文字與形狀 | 狀態一律用 Badge（色點＋文字），不只靠顏色 |
| 告訴下一步 | 每個錯誤、空畫面、未完成狀態都說明怎麼做 | 錯誤訊息＝發生什麼＋如何處理；EmptyState 附主要操作；多步驟流程用 Stepper |
| 不離開脈絡 | 檢視與編輯單一資源時保留列表 | 詳細與編輯使用 Drawer；破壞性確認用 ConfirmDialog |
| 即時回饋 | 每個操作都有可見結果 | 按鈕載入狀態（`busy`）、成功 Toast、失敗就地顯示 |
| 一致 | 同樣的事長得一樣 | 只用本文件的 Token 與 [05](05-components.md) 的元件 |
| 可及 | 每個人都能操作 | 對比 ≥ AA、鍵盤可達、焦點可見、尊重減少動態偏好 |

## 2. Token 架構

三層，元件只能引用 Token，不可寫死色碼或尺寸。`scripts/check-docs.sh` 會檢查 `web/` 用到的每個 `--token` 都列在本文件。

```
基礎 Token（tokens.css）                  間距、字級、動態、圖層、版面寬度
        │
語意 Token（theme.css）                  色彩、字體、圓角、控制項高度、陰影
        │  依淺／深色切換（跟隨系統，或 data-theme 強制）
元件（components.css）                   只引用上面兩層
```

允許寫死的值：`0`、`1px`／`2px` 線寬、圖示與頭像等固定元件尺寸（16／24／28／48px）、百分比、`fr`、`ch`。

### 2.1 基礎 Token（tokens.css）

| Token | 值 | 用途 |
| --- | --- | --- |
| `--space-0` … `--space-16` | `0`、`4px`、`8px`、`12px`、`16px`、`20px`、`24px`、`32px`、`40px`、`48px`、`64px` | 間距（4px 基準）：`--space-1` `--space-2` `--space-3` `--space-4` `--space-5` `--space-6` `--space-8` `--space-10` `--space-12` |
| `--text-xs` / `--text-sm` / `--text-md` / `--text-lg` / `--text-xl` / `--text-2xl` / `--text-3xl` | 12 / 13 / 14 / 16 / 20 / 24 / 30 px | 標籤／說明／內文／區塊標題／Drawer 標題／頁面標題／Stat 數字 |
| `--leading` / `--leading-tight` | `1.5` / `1.25` | 內文／標題行高 |
| `--weight-regular` / `--weight-medium` / `--weight-semibold` | 400 / 500 / 600 | 只用這三種字重 |
| `--duration-fast` / `--duration` / `--duration-slow` | 100 / 160 / 240 ms | 動態；減少動態偏好時為 0 |
| `--ease` | `cubic-bezier(0.2, 0, 0, 1)` | 所有轉場 |
| `--z-sticky` / `--z-overlay` / `--z-drawer` / `--z-toast` | 10 / 40 / 50 / 60 | 圖層 |
| `--sidebar-width` / `--content-width` / `--drawer-width` | 232 / 1200 / 560 px | 版面 |
| `--breakpoint-md` | `768px` | 唯一斷點（僅供參照） |
| `--border` / `--border-strong` | `1px solid` 一般／強調邊框色 | 組合 Token |
| `--focus-ring` | `2px solid var(--color-focus)` | `:focus-visible` |

### 2.2 語意 Token：色彩

定義於 `web/ds/theme.css`，淺色、深色兩組都必須定義下列全部 Token（`check-docs.sh` 檢查）。

| Token | 淺色 | 深色 |
| --- | --- | --- |
| `--color-bg` | `#f6f7f9` | `#0b0f17` |
| `--color-surface` | `#ffffff` | `#111722` |
| `--color-surface-raised` | `#ffffff` | `#18202d` |
| `--color-surface-sunken` | `#f1f3f6` | `#0d121b` |
| `--color-surface-hover` | `#f1f3f6` | `#18202d` |
| `--color-text` | `#111827` | `#e6e9ef` |
| `--color-text-muted` | `#4b5563` | `#a3acb9` |
| `--color-text-subtle` | `#5f6774` | `#8c95a3` |
| `--color-border` | `#e4e7ec` | `#222b38` |
| `--color-border-strong` | `#cfd4dc` | `#334052` |
| `--color-accent` | `#1d4ed8` | `#6ea2ff` |
| `--color-accent-hover` | `#1e40af` | `#93bbff` |
| `--color-accent-subtle` | `#eef3ff` | `#14223d` |
| `--color-on-accent` | `#ffffff` | `#08101f` |
| `--color-success` | `#15803d` | `#4ade80` |
| `--color-success-subtle` | `#ecfdf3` | `#0f2a1b` |
| `--color-warning` | `#a15c07` | `#fbbf24` |
| `--color-warning-subtle` | `#fef6e7` | `#2b2210` |
| `--color-danger` | `#b42318` | `#f87171` |
| `--color-danger-subtle` | `#fef3f2` | `#2d1515` |
| `--color-focus` | `#2563eb` | `#6ea2ff` |
| `--color-sidebar-bg` | `#0f172a` | `#070b12` |
| `--color-sidebar-text` | `#a8b3c4` | `#9aa4b2` |
| `--color-sidebar-text-active` | `#ffffff` | `#f8fafc` |
| `--color-sidebar-active-bg` | `#1e293b` | `#162030` |
| `--color-sidebar-border` | `#1e293b` | `#161d29` |
| `--color-overlay` | `rgb(15 23 42 / 0.45)` | `rgb(0 0 0 / 0.6)` |

| Token | 用途 |
| --- | --- |
| `--color-bg` | 頁面背景 |
| `--color-surface` | 卡片、表格、輸入框 |
| `--color-surface-raised` | Drawer、Dialog、Toast |
| `--color-surface-sunken` | 表頭、程式碼、log、Segmented 底 |
| `--color-surface-hover` | 列 hover、次要按鈕 hover |
| `--color-text` / `--color-text-muted` / `--color-text-subtle` | 主要／次要／提示文字（皆 ≥ 4.5:1） |
| `--color-border` / `--color-border-strong` | 分隔線／輸入框與按鈕邊框 |
| `--color-accent` / `--color-accent-hover` / `--color-accent-subtle` | 連結、選取、主要按鈕；淡底用於資訊 Banner、選取 Badge |
| `--color-on-accent` | 主要按鈕上的文字 |
| `--color-success` / `--color-warning` / `--color-danger`（含 `-subtle`） | 狀態色與其淡底；只用於狀態與破壞性操作 |
| `--color-focus` | 焦點環 |
| `--color-sidebar-bg` / `--color-sidebar-text` / `--color-sidebar-text-active` / `--color-sidebar-active-bg` / `--color-sidebar-border` | 深色側欄 |
| `--color-overlay` | Drawer／Dialog 背後遮罩 |

### 2.3 語意 Token：字體、形狀、陰影

| Token | 值 | 用途 |
| --- | --- | --- |
| `--font-ui` | `system-ui, -apple-system, "Segoe UI", "Noto Sans TC", "PingFang TC", sans-serif` | 介面文字 |
| `--font-display` | `var(--font-ui)` | 標題與數字 |
| `--font-mono` | `ui-monospace, "SF Mono", Menlo, Consolas, monospace` | 程式碼、ID、log |
| `--radius-control` | `6px` | 按鈕、輸入框 |
| `--radius-card` | `8px` | 卡片、表格、Banner |
| `--radius-overlay` | `10px` | Drawer、Dialog |
| `--control-height` | `32px` | 按鈕、輸入框、側欄項目高度 |
| `--brand-mark` | `linear-gradient(135deg, #2563eb, #1d4ed8)` | 品牌標誌與頭像底色 |
| `--shadow-card` | 淺 `0 1px 2px rgb(16 24 40 / 0.06)`；深 `none` | 卡片 |
| `--shadow-overlay` | 淺 `0 16px 40px rgb(16 24 40 / 0.18)`；深 `0 16px 40px rgb(0 0 0 / 0.5)` | 浮層 |

### 2.4 對比驗證

所有文字色（text、muted、subtle、accent、success、warning、danger）對 `bg`、`surface`、`surface-raised`、`surface-hover` 的對比皆 ≥ 4.5:1；狀態色對自己的 `-subtle` 底、`on-accent` 對 `accent` 與 `accent-hover`、側欄文字對側欄底亦同（NFR-A11Y-01）。調整色值後須重新驗證。

## 3. 版面

- **AppShell**：左側深色側欄（品牌、導覽、帳戶、主題、登出）＋右側內容。寬度 < 768px 時側欄改為頂部列，導覽可水平捲動。
- **Page**：最大寬 `--content-width`、置中；結構為 PageHeader（標題＋說明＋主要操作）→ 摘要 → 列表。
- **Drawer**：右側滑出，寬 `--drawer-width`，手機為全寬；Header（標題＋狀態）／Body（可捲動）／Footer（操作，破壞性在左、主要在右）。
- 頁面不得橫向捲動；表格在 `c-table-wrap` 內捲動。
- 間距用 `gap`，不靠元素外距。

## 4. 正式風格：企業儀表板

依 [ADR 0007](adr/0007-ui-style-proposals.md) 採用「企業儀表板」：

| 面向 | 規格 |
| --- | --- |
| 色彩 | 藍灰中性色＋藍色強調（`#1d4ed8`）；深藍側欄區隔導覽與內容；狀態色只用於狀態 |
| 字體 | 系統無襯線字；程式碼、ID、log 用等寬字 |
| 形狀 | 6px 控制項、8px 卡片、10px 浮層圓角；細陰影，深色模式不用陰影 |
| 密度 | 高：控制項 32px、表格儲存格 12／16px 內距 |
| 理由 | Caloud 的主要工作是在表格中掃讀狀態並處理例外，中性配色讓狀態色最突出，也最不易過時 |

另外兩個提案（開發者終端、CAFECA 品牌）已移除；其規格保留在 ADR 0007 供日後參考。風格調整一律修改 `theme.css` 的語意 Token 並更新本文件，不在元件中寫死值。

## 5. 狀態與回饋

| 情境 | 呈現 |
| --- | --- |
| 首次載入 | 表格顯示 Skeleton 列；Stat 卡顯示 Skeleton |
| 按鈕處理中 | `busy()`：停用、旋轉圖示、「處理中…」 |
| 成功 | Toast（4 秒，右下）；列表同步更新（FR-UI-05） |
| 可預期錯誤（4xx） | 表單內：欄位下方 `c-field__error`＋`aria-invalid`；列表操作：錯誤 Toast |
| 非預期錯誤（5xx／網路） | 「伺服器錯誤，請稍後再試。」／「無法連線到伺服器，請檢查網路後再試。」 |
| 背景作業 | Badge「部署中」帶脈動色點；每 3 秒輪詢（FR-UI-02） |
| 破壞性操作 | ConfirmDialog，說明後果，確認鈕為 danger（FR-UI-03） |
| 空列表 | EmptyState：圖示＋標題＋說明＋主要操作；篩選無結果時提示調整條件 |
| 需處理事項 | 總覽「需要處理」Banner 清單，每項附前往按鈕（FR-UI-09） |

## 6. 文案

### 6.1 語氣

- 繁體中文、平實、簡短；稱使用者為「你」。
- 不用驚嘆號、不用表情符號、不用「親愛的」「抱歉」等客套。
- 中文與英文、數字之間加半形空格：「上傳 docker compose 檔」「最近 500 行」。
- 中文語境用全形標點（，。：；「」），程式碼、指令、欄位名稱用 `等寬字`。
- 頁面說明一句話講清楚「這頁做什麼、下一步是什麼」。

### 6.2 標準用語

| 使用 | 不使用 |
| --- | --- |
| 登入／登出 | 登錄、簽入 |
| 網域 | 域名、網址（指主機名稱時） |
| 部署、重新部署 | 發佈、上線 |
| 容器 | 貨櫃 |
| 停止 | 暫停、關閉 |
| 刪除 | 移除（指資料時） |
| 綁定、轉發 | 連結、對應 |
| 驗證 | 認證、校驗 |
| 帳戶 | 帳號（指 CAFECA 身分時） |
| 核准 | 審批、通過 |

### 6.3 狀態文字

由 `UI.statusPill(kind, value)` 產生，不可手寫。

| kind | 值 → 顯示（色調） |
| --- | --- |
| `app` | running → 運作中（success）· deploying → 部署中（warning，脈動）· failed → 失敗（danger）· stopped → 已停止（neutral） |
| `domain` | true → 已驗證（success）· false → 待驗證（warning） |
| `route` | live → 轉發中（success）· idle → App 未運作（warning）· none → 未綁定（neutral） |
| `tls` | issued → 已簽發 · pending → 申請中 · failed → 失敗 · none → 未啟用 |
| `role` | super_admin → Super Admin · admin → Admin（accent）· member → Member · pending → 待核准（warning）· disabled → 已停用（danger） |

### 6.4 按鈕文字

動詞開頭、2–6 字：「新增 App」「建立並部署」「重新部署」「停止」「刪除 App」「驗證」「套用」「儲存並重新部署」「以 CAFECA 登入」。確認對話框的確認鈕重述動作與對象（「刪除 App」而非「確定」）。

### 6.5 錯誤訊息

格式：**發生了什麼＋怎麼處理**。

- ✅「service `web`：不允許使用 `ports`，請移除此設定。」
- ❌「錯誤：policy violation」

## 7. 無障礙檢查表

- [ ] 所有輸入元件有 `<label>`、`c-field__label` 或 `aria-label`
- [ ] 可互動元素是 `<button>`／`<a>`／表單元件；可點擊的表格列有 `tabindex="0"` 並支援 Enter
- [ ] 只有圖示的按鈕有 `aria-label` 與 `title`
- [ ] `:focus-visible` 顯示 `--focus-ring`
- [ ] 狀態不只靠顏色（Badge 一定有文字）
- [ ] 錯誤訊息 `role="alert"`；Toast 區域 `aria-live="polite"`
- [ ] 目前頁面的導覽項目有 `aria-current="page"`；篩選按鈕有 `aria-pressed`；分頁有 `role="tab"` 與 `aria-selected`
- [ ] Drawer 與 Dialog 使用原生 `<dialog>`，Esc 可關閉，開啟時焦點移入
