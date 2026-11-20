-- M6-05、M6-07（ADR 0021）：GitHub Checks、PR 留言、建置資源與建置分鐘

ALTER TABLE verifications ADD COLUMN check_run_id INTEGER;
ALTER TABLE verifications ADD COLUMN pr_number INTEGER;
ALTER TABLE verifications ADD COLUMN build_secs INTEGER NOT NULL DEFAULT 0;

-- 單列：建置與沙箱資源、每位用戶每月建置分鐘（0＝不限）
CREATE TABLE build_settings (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    cpus            INTEGER NOT NULL DEFAULT 4,
    memory_mb       INTEGER NOT NULL DEFAULT 8192,
    minutes_monthly INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0
);
INSERT INTO build_settings (id) VALUES (1);

-- 建置用量：記在專案擁有者名下
CREATE TABLE build_usage (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    account TEXT NOT NULL,
    project TEXT,
    kind    TEXT NOT NULL,                -- verify | deploy
    ref_id  TEXT NOT NULL,
    secs    INTEGER NOT NULL,
    at      INTEGER NOT NULL
);
CREATE INDEX build_usage_account ON build_usage (account, at);

-- 每個 PR 一則留言
CREATE TABLE github_pr_comments (
    project    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    pr         INTEGER NOT NULL,
    comment_id INTEGER NOT NULL,
    PRIMARY KEY (project, pr)
);
