# ADR 0004：以單一 SQL 陳述式決定第一位 super admin

- 狀態：Accepted
- 日期：2026-11-09
- 相關需求：FR-AUTH-03

## 背景

需求為「部署後第一位登入者即 super admin」，必須在並發登入時只產生一位。

## 決策

在 `db::upsert_login` 以單一陳述式完成：

```sql
INSERT INTO users (...) VALUES (?1, ?2,
  CASE WHEN (SELECT COUNT(*) FROM users) = 0 THEN 'super_admin' ELSE ?3 END, ?4, ?4)
ON CONFLICT(account) DO UPDATE SET ... RETURNING *
```

SQLite 同一時間只有一個寫入者，陳述式內的子查詢與寫入為原子操作。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **單一陳述式**（採用） | 簡單、原子 | 依賴 SQLite 單一寫入者語意 |
| transaction + SELECT 後 INSERT | 易讀 | DEFERRED transaction 仍可能競態 |
| 啟動時以設定指定 | 無競態 | 需事先知道帳戶地址 → 以 FR-AUTH-08 補充 |

## 影響

- 有 `first_login_becomes_super_admin` 測試
- 改用 PostgreSQL 時需以 advisory lock 或 SERIALIZABLE 重新實作
