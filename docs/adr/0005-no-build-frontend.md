# ADR 0005：前端為無建置步驟的單一 HTML

- 狀態：Superseded by [0006](0006-design-system-files.md)（「單一 HTML 檔」部分；無建置步驟、編譯時內嵌仍有效）
- 日期：2026-11-09
- 相關需求：FR-UI-01～04、NFR-OPS-01

## 背景

管理介面頁面少（登入、Apps、網域、使用者），使用者為維運人員。

## 決策

`web/index.html` 以原生 HTML／CSS／JS 撰寫，編譯時以 `include_str!` 內嵌於執行檔。設計一致性由 [04 設計語言](../04-design-language.md)、[05 通用元件](../05-components.md) 與 `scripts/check-docs.sh` 保證。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **單一 HTML**（採用） | 無 Node 工具鏈；單一執行檔 | 規模變大後難維護 |
| React／Vite SPA | 元件化、生態 | 需 Node 建置流程 |
| 伺服器端模板（askama）＋ htmx | 純 Rust | 互動狀態較難處理 |

## 影響

- 當 `index.html` 超過約 1,500 行或頁面超過 6 個時，重新評估並以新 ADR 取代
