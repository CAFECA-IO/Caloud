# ADR 0011：Runner 以 WebSocket 主動連入控制面

- 狀態：Accepted
- 日期：2026-11-10
- 相關需求：FR-OPS-04、FR-OPS-06、FR-OPS-09、FR-DEV-03

## 背景

Runner 節點包含平臺主機與用戶自有設備。自有設備常在 NAT 或企業防火牆後，沒有公網 IP；Edge 仍須把流量送到設備上的環境。設計文件原本寫 gRPC over mTLS。

## 決策

- Runner 上的 `caloud-agent` 主動以 HTTPS WebSocket 連到 `GET /agent/v1/connect`（`caloud.cafeca.io`），斷線以指數退避重連。
- 第一則訊息為 `register`：帶一次性加入權杖（首次）或長期節點權杖，以及偵測到的規格；控制面回 `ack` 並在首次發給節點權杖（只存 keccak 雜湊）。
- 之後每 15 秒 `heartbeat`；60 秒無心跳標為離線。
- **通道（M4-07，2026-11-11 修訂）**：註冊成功後，agent 另開第二條 WebSocket `GET /agent/v1/tunnel`（`Authorization: Bearer <節點權杖>`），binary frame 承載 yamux（`tokio-yamux`）。控制面為 yamux client，Edge 每條上游連線開一條 stream，第一行 `CONNECT <host>:<port>`，agent 連到該位址後雙向轉送。
  - 不與控制訊息共用同一條連線：大量傳輸不會擋住心跳，控制協定也不必改。
- 訊息為 JSON 文字訊框，格式定義於 [06 §6](../06-api-spec.md#6-agent-協定)。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **WebSocket＋yamux**（採用） | 沿用 axum／hyper；穿得過只允許 HTTPS 的防火牆；控制與通道共用一條連線 | 需自訂訊息格式 |
| gRPC over mTLS | 強型別 | 新增 tonic 與憑證簽發；反向通道仍需另外設計；常被企業 proxy 擋 |
| WireGuard 等 VPN | 網路層通道 | 需核心模組與權限，自有設備安裝門檻高 |

## 影響

- `caloud-api` 啟用 axum `ws` 功能；agent 使用 `tokio-tungstenite`（[ADR 0012](0012-runner-crate.md)）。
- `caloud-proxy` 新增 `tokio-yamux`、`tokio-util`，提供 `TunnelRegistry` 與 WebSocket 轉 byte stream 的 `ws_io`。
