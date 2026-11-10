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
- M4-07 起同一條連線以 yamux 多工：控制訊息一條 stream，Edge 轉發的每個請求各開一條 stream（反向通道）。
- 訊息為 JSON 文字訊框，格式定義於 [06 §6](../06-api-spec.md#6-agent-協定)。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **WebSocket＋yamux**（採用） | 沿用 axum／hyper；穿得過只允許 HTTPS 的防火牆；控制與通道共用一條連線 | 需自訂訊息格式 |
| gRPC over mTLS | 強型別 | 新增 tonic 與憑證簽發；反向通道仍需另外設計；常被企業 proxy 擋 |
| WireGuard 等 VPN | 網路層通道 | 需核心模組與權限，自有設備安裝門檻高 |

## 影響

- `caloud-api` 啟用 axum `ws` 功能；agent 使用 `tokio-tungstenite`（[ADR 0012](0012-runner-crate.md)）。
