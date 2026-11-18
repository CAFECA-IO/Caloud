# ADR 0012：新增 `caloud-runner` crate 與 `caloud-agent` 執行檔

- 狀態：Accepted
- 日期：2026-11-10
- 相關需求：FR-OPS-04、FR-OPS-08

## 背景

Runner agent 跑在平臺節點與用戶設備上，與控制面是不同的執行檔；控制面也需要共用的協定型別與調度邏輯。

## 決策

- 新增 crate `caloud-runner`，產生執行檔 `caloud-agent`：規格偵測、連線與重連、節點權杖保存。只依賴 `caloud-core`。（2026-11-18 修訂：另依賴 `caloud-orchestrator`，見 [ADR 0015](0015-runner-sandbox.md)）
- 協定訊息型別（`AgentMsg`、`ServerMsg`、`NodeSpecs`）與調度函式 `choose_node` 放在 `caloud-core`，讓控制面與 agent 共用。
- 新依賴：`sysinfo`（跨平臺規格偵測）、`tokio-tungstenite`（agent 端 WebSocket）。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **獨立 crate＋執行檔**（採用） | agent 體積小、不含 API 與資料庫 | workspace 多一個 crate |
| `caloud` 執行檔加子命令 | 一個執行檔 | 用戶設備需安裝整個控制面程式 |
| 自行讀 `/proc` | 無依賴 | 只支援 Linux，解析細節多 |

## 影響

- [02 架構](../02-architecture.md) 的相依圖新增 `caloud-runner ──► caloud-core`。
