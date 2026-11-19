-- M7-16 最小部署流程（FR-ENV-12、ADR 0018）

-- 專案的環境：project 不為 NULL；每個專案每種環境一個
ALTER TABLE environments ADD COLUMN project TEXT REFERENCES projects(id);
ALTER TABLE environments ADD COLUMN commit_sha TEXT;
-- 部署階段：fetch | build | start | health | done；admin 開的環境為 NULL
ALTER TABLE environments ADD COLUMN stage TEXT;
CREATE UNIQUE INDEX environments_project_kind ON environments (project, kind) WHERE project IS NOT NULL;

-- 部署 log：每個環境保留最近 2000 行
CREATE TABLE deploy_logs (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    env   TEXT NOT NULL,
    stage TEXT NOT NULL,
    line  TEXT NOT NULL,
    at    INTEGER NOT NULL
);
CREATE INDEX deploy_logs_env ON deploy_logs (env, id);
