-- ADR 0024：專案頁改版與設定沿用

-- 確認方式：user（用戶確認）| auto（設定相同，沿用上一份確認）
ALTER TABLE analyses ADD COLUMN confirmed_via TEXT;
UPDATE analyses SET confirmed_via = 'user' WHERE confirmed_at IS NOT NULL;

-- 部署使用的解析（回滾依據）
ALTER TABLE deployments ADD COLUMN analysis TEXT;

-- 驗證：通過後沒有自動部署的原因、各階段起訖時間
ALTER TABLE verifications ADD COLUMN deploy_note TEXT;
ALTER TABLE verifications ADD COLUMN stage_times TEXT NOT NULL DEFAULT '{}';
-- 驗證時的設定指紋（之後重新確認同一筆解析也不影響）
ALTER TABLE verifications ADD COLUMN settings TEXT;

-- 用戶標為「不需要」的變數
CREATE TABLE project_var_ignores (
    project     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    created_by  TEXT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (project, name)
);
