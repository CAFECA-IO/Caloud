# Caloud 文件總覽

本資料夾是 Caloud 的**唯一規格來源（single source of truth）**。程式碼必須依文件實作；文件與程式不一致時，以文件為準，並視為缺陷修正其中一方。

## 文件清單

| # | 文件 | 內容 | 主要讀者 |
| --- | --- | --- | --- |
| 00 | [README.md](README.md) | 文件總覽與治理規則（本文件） | 所有人 |
| 01 | [01-requirements.md](01-requirements.md) | 需求規格：功能需求（FR）、非功能需求（NFR）、驗收條件 | PM、開發、QA |
| 02 | [02-architecture.md](02-architecture.md) | 系統架構：元件、流程、資料模型、部署拓樸 | 開發 |
| 03 | [03-development-plan.md](03-development-plan.md) | 開發計劃：里程碑、工作項目、完成定義、流程 | PM、開發 |
| 04 | [04-design-language.md](04-design-language.md) | 設計語言：設計原則、Design Tokens、文案、無障礙 | 設計、前端 |
| 05 | [05-components.md](05-components.md) | 通用元件：前端 UI 元件與後端共用模組規格 | 前端、後端 |
| 06 | [06-api-spec.md](06-api-spec.md) | API 規格：端點、請求／回應格式、錯誤碼 | 前端、後端、整合方 |
| 07 | [07-security.md](07-security.md) | 安全規範：威脅模型、compose 政策、權限矩陣 | 開發、維運 |
| 08 | [08-coding-standards.md](08-coding-standards.md) | 開發規範：Rust／前端程式風格、測試、Git 流程 | 開發 |
| — | [adr/](adr/) | 架構決策紀錄（Architecture Decision Records） | 開發 |

## 文件治理：按圖施工

### 規則

1. **文件先行**：新增或變更功能時，先修改對應文件（需求 → 架構／API／元件 → 計劃），再寫程式。文件與程式碼放在**同一個 PR**。
2. **可追溯**：每一項功能需求都有編號（例：`FR-AUTH-03`）。實作與測試在註解中引用編號，PR 描述列出涉及的編號。
3. **只能用文件定義過的東西**：
   - 前端只能使用 [04](04-design-language.md) 定義的 Design Token 與 [05](05-components.md) 定義的元件 class
   - API 路徑必須列在 [06](06-api-spec.md)
   - compose 政策的每一條規則必須列在 [07](07-security.md)
4. **重大技術決策寫 ADR**：選型、捨棄某方案、改變架構邊界，都要在 [adr/](adr/) 新增一份紀錄。ADR 一經接受不修改內容，只能以新 ADR 取代（Superseded）。
5. **自動檢查**：`scripts/check-docs.sh` 在 CI 執行，驗證上述第 2、3 點；不通過不得合併。

### 變更流程

```
需求變更 ─► 更新 01（新增/修改 FR 編號）
          ─► 影響架構？更新 02，必要時寫 ADR
          ─► 影響 API？更新 06
          ─► 影響 UI？更新 04 / 05
          ─► 更新 03 的工作項目
          ─► 實作 + 測試（引用 FR 編號）
          ─► scripts/check-docs.sh、cargo test、clippy 全部通過
          ─► PR（勾選文件遵循檢查表）
```

### 文件狀態標記

每份文件開頭標示版本與狀態：

| 狀態 | 意義 |
| --- | --- |
| Draft | 草稿，可自由修改 |
| Accepted | 已確認，修改需經 PR 審查 |
| Deprecated | 已淘汰，保留供查閱 |

### 用語

| 用語 | 定義 |
| --- | --- |
| App | 一份使用者上傳的 docker compose 檔所描述的服務集合，對應一個 compose project |
| Service | compose 檔中 `services` 底下的一個項目 |
| 網域（Domain） | 由 Caloud 托管並轉發流量的主機名稱 |
| 綁定（Bind） | 把網域指向某個 App 的 service 與容器內 port |
| Edge 網路 | Caloud 與所有被代理容器共同加入的 docker 網路 `caloud-edge` |
| 帳戶（Account） | CAFECA 身分合約地址，使用者唯一識別 |
