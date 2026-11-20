-- M6 驗證與報告（FR-CI-01～05、FR-RPT-01～04、ADR 0020）

CREATE TABLE verifications (
    id           TEXT PRIMARY KEY,
    project      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    analysis     TEXT NOT NULL,                 -- 驗證時確認的解析（部署關卡比對）
    commit_sha   TEXT NOT NULL,
    branch       TEXT NOT NULL,
    trigger      TEXT NOT NULL,                 -- push | manual | deploy
    status       TEXT NOT NULL,                 -- queued | running | passed | failed | canceled | error
    stage        TEXT,
    stages       TEXT NOT NULL DEFAULT '{}',    -- {階段: pending|running|passed|failed|skipped}
    counts       TEXT NOT NULL DEFAULT '{}',    -- {blocker, high, medium, low}
    env          TEXT,                          -- 驗證環境子網域（結束後刪除環境紀錄）
    node         TEXT,
    deploy_after INTEGER NOT NULL DEFAULT 0,
    keep         INTEGER NOT NULL DEFAULT 0,
    keep_until   INTEGER,
    error        TEXT,
    created_by   TEXT,
    created_at   INTEGER NOT NULL,
    started_at   INTEGER,
    finished_at  INTEGER
);
CREATE INDEX verifications_project ON verifications (project, created_at);

CREATE TABLE verification_issues (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    verification TEXT NOT NULL REFERENCES verifications(id) ON DELETE CASCADE,
    fingerprint  TEXT NOT NULL,
    severity     TEXT NOT NULL,                 -- blocker | high | medium | low
    stage        TEXT NOT NULL,
    rule         TEXT NOT NULL,
    title        TEXT NOT NULL,
    detail       TEXT NOT NULL DEFAULT '',
    file         TEXT,
    line         INTEGER,
    UNIQUE (verification, fingerprint)
);

CREATE TABLE verification_logs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    verification TEXT NOT NULL REFERENCES verifications(id) ON DELETE CASCADE,
    stage        TEXT NOT NULL,
    line         TEXT NOT NULL,
    at           INTEGER NOT NULL
);
CREATE INDEX verification_logs_run ON verification_logs (verification, id);
