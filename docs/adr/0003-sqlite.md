# ADR 0003：使用 SQLite

- 狀態：Accepted
- 日期：2026-11-09
- 相關需求：NFR-OPS-01、FR-AUTH-03

## 背景

Caloud 為單機部署，資料量小（使用者、App、網域各數百筆以內）。

## 決策

使用 SQLite（WAL 模式），經 sqlx 存取，migration 隨程式碼發布並於啟動時執行。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **SQLite**（採用） | 零維運；單一寫入者天然避免競態 | 不適合多節點 |
| PostgreSQL | 可擴展 | 多一個服務要維運 |

## 影響

- 多節點（M4）時需改用 PostgreSQL；查詢保持標準 SQL 以利遷移
