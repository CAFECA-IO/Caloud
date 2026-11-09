# ADR 0001：以 docker compose CLI 執行部署

- 狀態：Accepted
- 日期：2026-11-09
- 相關需求：FR-APP-01～07

## 背景

使用者上傳的是標準 docker compose 檔，需要完整支援 `depends_on`、`healthcheck`、`profiles`、具名 volume 等語意，並能更新、停止、刪除。

## 決策

由 `ComposeRunner` 以 `tokio::process` 呼叫 `docker compose -p <project> -f compose.yml -f caloud.override.yml …`。狀態以 `docker compose ps --format json` 讀取。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **compose CLI**（採用） | 與 compose 規格完全相容；差異更新由 compose 處理 | 需安裝 compose plugin；需解析 CLI 輸出 |
| bollard 自行實作 compose 語意 | 純 Rust、型別安全 | 工作量大且難以追上 compose 規格 |
| 轉成 Kubernetes | 生態完整 | 對單機部署過重 |

## 影響

- Caloud 容器需包含 docker CLI 與 compose plugin（Dockerfile 已處理）
- 需要 Compose ≥ 2.20 以使用 `--wait`（NFR-COMPAT-02）
- 只有 `ComposeRunner` 可執行 docker 指令（[05 §B](../05-components.md#b-後端共用模組)）
