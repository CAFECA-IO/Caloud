# ADR 0006：Design System 拆為 web/ds/ 多檔

- 狀態：Accepted
- 日期：2026-11-09
- 相關需求：FR-UI-04～12
- 取代：[0005](0005-no-build-frontend.md) 中「單一 HTML 檔」的部分

## 背景

介面改版後元件數量增加（AppShell、Drawer、Stepper、Toast 等），並要比較三個風格提案。全部寫在 `index.html` 內會讓樣式無法在 `/styleguide` 重用，也難以單獨審查 Token 變更。

## 決策

- Design System 放在 `web/ds/`：`tokens.css`（基礎）、`theme.css`（語意；原為 `themes/<風格>.css`，ADR 0007 後只保留一套）、`components.css`（元件）、`ui.js`（共用函式與圖示）。
- 維持**無建置步驟**：檔案以 `include_str!` 內嵌，經 `GET /assets/{*path}` 白名單提供（`caloud-api::assets`）。
- `index.html` 與 `styleguide.html` 只引用這些檔案、組合元件；`index.html` 不寫樣式。
- 淺色／深色跟隨系統，`data-theme` 屬性可強制切換。（提案期間曾以 `data-style` 切換風格，ADR 0007 後移除。）

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **多檔＋內嵌＋白名單路由**（採用） | 無 Node 工具鏈；單一執行檔；可在多頁重用 | 多幾個 HTTP 請求（內網工具可接受） |
| 維持單一 HTML | 最簡單 | 無法重用、審查困難 |
| 引入 Vite／Tailwind | 生態完整 | 需要 Node 建置流程，違反 NFR-OPS-01 的精神 |

## 影響

- 新增 Design System 檔案時要同時更新 `assets.rs` 的 `FILES` 與 [06 §4](../06-api-spec.md#4-非-api-路徑)
- 測試 `index_assets_are_embedded` 確保 `index.html` 引用的資源都已內嵌
- `check-docs.sh` 改為掃描整個 `web/`
