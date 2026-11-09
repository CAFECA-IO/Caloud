## 摘要

<!-- 這個 PR 做了什麼 -->

## 工作項目與需求

- 工作項目：<!-- 例：M2-01（docs/03-development-plan.md） -->
- 需求編號：<!-- 例：FR-DOM-05 -->

## 文件遵循檢查表（docs/README.md「按圖施工」）

- [ ] 實作前已閱讀相關文件；規格不清處已先修改文件
- [ ] 需求有變更 → 已更新 `docs/01-requirements.md`
- [ ] 架構／資料模型有變更 → 已更新 `docs/02-architecture.md`，重大決策已新增 ADR
- [ ] API 有變更 → 已更新 `docs/06-api-spec.md`
- [ ] UI 有變更 → 只使用 `docs/04` 的 Token 與 `docs/05` 的元件；新元件已先寫入 `docs/05`
- [ ] 安全相關（compose 政策、權限、驗證）→ 已更新 `docs/07-security.md`，並請兩位審查
- [ ] 符合 `docs/08-coding-standards.md`；程式與測試註解引用需求編號
- [ ] 已更新 `docs/03-development-plan.md` 的工作項目狀態

## 驗證

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] `scripts/check-docs.sh`
- [ ] UI：淺色／深色、寬度 360px、鍵盤操作（若有 UI 變更）
