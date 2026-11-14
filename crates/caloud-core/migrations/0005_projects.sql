-- M5 平臺專案（FR-GH-04、FR-AN-10）：以公開儲存庫匯入並解析

CREATE TABLE projects (
    id                 TEXT PRIMARY KEY,
    owner              TEXT NOT NULL REFERENCES users(account),
    name               TEXT NOT NULL,
    repo_url           TEXT NOT NULL,
    git_ref            TEXT,                 -- NULL = 預設分支
    branch             TEXT,                 -- 解析時取得的預設分支名稱
    root               TEXT NOT NULL DEFAULT '',
    status             TEXT NOT NULL,        -- analyzing | needs_review | confirmed | error
    error              TEXT,
    confirmed_analysis TEXT,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL
);
CREATE INDEX projects_owner ON projects(owner);

-- 每次解析一筆；確認的那筆由 projects.confirmed_analysis 指向（不被重新解析覆蓋）
CREATE TABLE analyses (
    id             TEXT PRIMARY KEY,
    project        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    git_ref        TEXT NOT NULL,
    commit_sha     TEXT NOT NULL,
    result         TEXT NOT NULL,             -- caloud_analyzer::Analysis JSON
    compose        TEXT,                      -- 用戶編輯版；NULL = 使用 result.compose
    public_service TEXT,
    acknowledged   TEXT NOT NULL DEFAULT '[]', -- 已確認的低信心 finding 索引（JSON）
    confirmed_by   TEXT,
    confirmed_at   INTEGER,
    created_at     INTEGER NOT NULL
);
CREATE INDEX analyses_project ON analyses(project, created_at);
