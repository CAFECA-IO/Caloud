# ADR 0008：MCP 介面採 Streamable HTTP 與個人 API 權杖

- 狀態：Accepted
- 日期：2026-11-09
- 相關需求：FR-MCP-01～08

## 背景

希望讓 AI 助手（Claude Desktop、Claude Code 等 MCP 用戶端）直接查詢與操作 Caloud：部署 App、查 log、設定網域。CAFECA 登入需要使用者在瀏覽器以 Passkey 簽署，MCP 用戶端無法完成這個流程。

## 決策

1. **傳輸**：MCP Streamable HTTP，單一端點 `POST /mcp`，與 REST API 同一個服務與埠。回應一律為 `application/json`（不開 SSE 串流）；`GET /mcp` 回 405。伺服器無狀態，不發 `Mcp-Session-Id`。
2. **協定版本**：支援 `2025-11-25`、`2025-06-18`、`2025-03-26`；用戶端要求其中之一時沿用，否則回應最新版。
3. **身分**：使用者在網頁（已用 CAFECA 登入）建立**個人 API 權杖**，MCP 用戶端以 `Authorization: Bearer` 帶入。權杖：
   - 格式 `cld_` + 256 bit 隨機值（base64url），DB 只存 keccak256 雜湊，只顯示一次
   - 代表建立者本人，權限即其**目前**角色；pending／disabled 立即失效
   - 可設有效天數（預設 90 天、上限 365 天），可隨時撤銷，記錄最後使用時間
4. **不接受 cookie**：`/mcp` 忽略 session cookie，避免瀏覽器跨站請求（CSRF）；帶有 `Origin` 標頭且不等於 `CALOUD_PUBLIC_ORIGIN` 時拒絕（防 DNS rebinding）。
5. **共用服務層**：工具與 REST handler 呼叫同一組服務函式（權限檢查、驗證、稽核都在服務層），不重複實作。

## 考慮過的方案

| 方案 | 優點 | 缺點 |
| --- | --- | --- |
| **Streamable HTTP＋個人權杖**（採用） | 用戶端設定簡單（URL＋標頭）；與現有服務共用；權杖可撤銷 | 權杖外洩風險需靠到期與撤銷控制 |
| MCP OAuth 2.1 授權流程 | 標準、無需手動複製權杖 | 需實作授權伺服器、動態註冊、PKCE；CAFECA 登入整合複雜，留待日後 |
| stdio 本機程式 | 不需網路認證 | 需在使用者電腦安裝並持有 docker 權限，違反集中管理 |
| 獨立 MCP 服務 | 隔離 | 多一個部署單元，需同步權限邏輯 |

## 影響

- 新增 `api_tokens` 資料表（migration 0002）與 `/api/tokens` 端點
- 新增 `caloud-api::service`（服務層）與 `caloud-api::mcp`
- `check-docs.sh` 檢查每個 MCP 工具都列在 [06 §5](../06-api-spec.md#5-mcp-介面)
- 日後若導入 MCP OAuth，以新 ADR 取代第 3 點，權杖機制可保留作為自動化用途
