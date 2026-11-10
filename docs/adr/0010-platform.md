# ADR 0010：Caloud 平臺化與網域分工

- 狀態：Accepted
- 日期：2026-11-10
- 相關需求：FR-ACC-*、FR-ENV-07、FR-OPS-03、FR-OPS-10、FR-DOM-11～13

## 背景

Caloud 原為單機自架工具。設計稿（Caloud Platform 系統設計，2026-11-10）把它擴充為多租戶平臺：用戶訂閱後提供 GitHub 儲存庫，平臺自動解析、驗證、部署，環境以隨機子網域對外。

## 決策

1. **網域分工**

   | 網域 | 用途 |
   | --- | --- |
   | `*.caloud.tw` | 用戶環境：production 8 字元、staging 12 字元、驗證 16 字元子網域 |
   | `caloud.tw` | 平臺介紹與方案頁 |
   | `caloud.cafeca.io` | 主控台、REST API、MCP、GitHub webhook、Agent 連線 |
   | 其他 `*.cafeca.io` | 系統服務（以系統 App 部署），只供系統管理者 |

   主控台與用戶內容不在同一個網站，用戶頁面碰不到主控台 cookie。`caloud.tw` 申請加入 Public Suffix List，讓用戶之間無法互設 cookie。

2. **網域資源規範放在後台**：網域池、ID 長度、保留字、封鎖清單由維運者設定（FR-OPS-03），不寫死在程式。
3. **DNS 一律在 Cloudflare**：wildcard 憑證以 DNS-01 經 Cloudflare API 申請；`*.caloud.tw` 記錄設為「僅 DNS」，TLS 由 Edge 終結。
4. **資料庫暫留 SQLite**（補充 [ADR 0003](0003-sqlite.md)）：第一版控制面仍是單一實例，Runner 不直連資料庫；查詢維持標準 SQL，控制面需要多實例時再遷移 PostgreSQL。
5. **方案與價格採暫定值**，由後台設定（FR-OPS-05）。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **用戶環境 `*.caloud.tw`、主控台 cafeca.io**（採用） | 用戶內容不影響官方網域的 cookie 與信譽 | 需維護兩個網域 |
| 全部放 `*.cafeca.io` | 一個網域 | 用戶頁面可對 `.cafeca.io` 設 cookie；釣魚頁帶官方信譽 |
| 先改 PostgreSQL | 符合設計文件 | 多一個服務、所有 migration 重寫，P1 工作量增加約三成 |

## 影響

- 新的里程碑 M4～M8 取代原「M4 進階」（[03](../03-development-plan.md)）。
- 既有單機功能（App、網域、系統 App）保留給維運者使用。
