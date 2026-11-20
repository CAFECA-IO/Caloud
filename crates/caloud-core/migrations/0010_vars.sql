-- M7-03 秘密變數（FR-ENV-05、FR-APP-11、ADR 0019）

-- 值以主金鑰加密（AAD project_var:<project>:<env>:<name>）；只寫不讀
CREATE TABLE project_vars (
    project    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    env        TEXT NOT NULL,                -- production | staging
    name       TEXT NOT NULL,
    value_enc  TEXT NOT NULL,
    hint       TEXT,                         -- 值 ≥ 12 字元時為末 4 碼
    generated  INTEGER NOT NULL DEFAULT 0,   -- 平臺產生（資料庫密碼），不可修改
    updated_by TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (project, env, name)
);

-- 每次變更加 1；部署時記錄到環境，兩者不同表示需重新部署才生效
ALTER TABLE projects ADD COLUMN vars_rev INTEGER NOT NULL DEFAULT 0;
ALTER TABLE environments ADD COLUMN vars_rev INTEGER;
