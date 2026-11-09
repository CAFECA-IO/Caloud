# ADR 0002：內建 Rust 反向代理

- 狀態：Accepted
- 日期：2026-11-09
- 相關需求：FR-DOM-04～07

## 背景

網域需即時轉向容器，並自動取得 TLS 憑證。

## 決策

在 Caloud 內以 hyper 實作反向代理（`caloud-proxy`），路由表以 `ArcSwap` 保存，由資料庫重新載入；TLS 以 rustls＋instant-acme 實作（M2）。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **內建代理**（採用） | 單一執行檔；路由與資料庫一致；無外部 admin API 要保護 | 需自行實作 ACME、WebSocket |
| Caddy（Admin API） | TLS 成熟 | 多一個服務；需同步兩份狀態 |
| Traefik（Docker labels） | 自動發現 | 路由由容器 label 決定，難以強制網域驗證 |

## 影響

- M2 需完成 TLS 與 WebSocket（M2-01～04）
- 需效能基準測試確認 NFR-PERF-01
