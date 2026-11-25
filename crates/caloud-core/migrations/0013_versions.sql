-- ADR 0023：建置識別與版本可見性
-- 執行中的版本只在健康檢查通過時更新；commit_sha 改為本次部署的目標
ALTER TABLE environments ADD COLUMN running_commit TEXT;
ALTER TABLE environments ADD COLUMN deployed_at INTEGER;
UPDATE environments SET running_commit = commit_sha, deployed_at = updated_at
  WHERE status = 'running' AND stage = 'done' AND commit_sha IS NOT NULL;

-- 專案環境的回應是否帶 X-Caloud-Commit
ALTER TABLE projects ADD COLUMN commit_header INTEGER NOT NULL DEFAULT 1;

-- 部署紀錄
CREATE TABLE deployments (
    id            TEXT PRIMARY KEY,
    project       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    env           TEXT NOT NULL,
    commit_sha    TEXT NOT NULL,
    trigger       TEXT NOT NULL,          -- manual | forced | push
    status        TEXT NOT NULL,          -- in_progress | success | failure
    stage         TEXT,                   -- 失敗的階段
    error         TEXT,
    gh_deployment_id INTEGER,
    created_by    TEXT,
    created_at    INTEGER NOT NULL,
    finished_at   INTEGER
);
CREATE INDEX deployments_project ON deployments (project, created_at);
CREATE INDEX deployments_env ON deployments (env, created_at);
