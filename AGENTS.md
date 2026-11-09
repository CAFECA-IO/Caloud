# AGENTS.md — 給 AI 開發助手與新進開發者的工作守則

本專案採「**文件先行、按圖施工**」。`docs/` 是唯一規格來源，程式碼必須符合文件；不符時視為缺陷。

## 開始任何工作之前

1. 閱讀 [docs/README.md](docs/README.md) 的治理規則。
2. 在 [docs/03-development-plan.md](docs/03-development-plan.md) 找到對應工作項目與需求編號。沒有對應項目時，**先**在 [docs/01-requirements.md](docs/01-requirements.md) 新增需求、在 03 新增工作項目，再寫程式。
3. 閱讀該需求涉及的章節：
   - 架構與 crate 相依規則：[docs/02-architecture.md](docs/02-architecture.md)
   - API：[docs/06-api-spec.md](docs/06-api-spec.md)
   - UI：[docs/04-design-language.md](docs/04-design-language.md)、[docs/05-components.md](docs/05-components.md)
   - 安全與 compose 政策：[docs/07-security.md](docs/07-security.md)
   - 程式風格與測試：[docs/08-coding-standards.md](docs/08-coding-standards.md)
   - 過去的決策：[docs/adr/](docs/adr/)

## 實作時必須遵守

- 規格不清楚或與需求衝突時：**停下來修改文件或詢問**，不要憑猜測實作。
- 只使用 docs/05 §B 列出的後端共用模組；不重複實作權限判斷、隨機值、docker 指令、路由重新載入。
- 前端只使用 docs/04 的 Design Token 與 docs/05 的元件 class 和共用函式。
- 新 API 路徑先寫進 docs/06；新 compose 規則先寫進 docs/07。
- 程式與測試註解引用需求編號（例：`// FR-AUTH-02`）。
- 重大技術決策（新依賴類別、新 crate、改變架構邊界）新增 ADR。

## 完成前必須通過

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
scripts/check-docs.sh
```

並依 docs/03 §3 的完成定義（DoD）逐項確認，最後更新 docs/03 的工作項目狀態。
