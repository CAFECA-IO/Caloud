-- M5-01：GitHub App（ADR 0017）

-- FR-GH-05：單列；機密以主金鑰加密
CREATE TABLE github_app (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    app_id              INTEGER NOT NULL,
    slug                TEXT NOT NULL,
    client_id           TEXT NOT NULL,
    client_secret_enc   TEXT NOT NULL,
    client_secret_hint  TEXT NOT NULL,
    private_key_enc     TEXT NOT NULL,
    private_key_hint    TEXT NOT NULL,
    webhook_secret_enc  TEXT NOT NULL,
    webhook_secret_hint TEXT NOT NULL,
    updated_at          INTEGER NOT NULL
);

-- FR-GH-01：安裝綁定到 Caloud 帳號
CREATE TABLE github_installations (
    id            INTEGER PRIMARY KEY,           -- GitHub installation id
    owner         TEXT NOT NULL REFERENCES users(account),
    account_login TEXT NOT NULL,
    account_type  TEXT NOT NULL,
    suspended     INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

-- FR-GH-02：webhook 去重（保留 7 天）
CREATE TABLE github_deliveries (
    delivery_id TEXT PRIMARY KEY,
    event       TEXT NOT NULL,
    received_at INTEGER NOT NULL
);

ALTER TABLE projects ADD COLUMN installation_id INTEGER;
ALTER TABLE projects ADD COLUMN repo_full_name TEXT;
CREATE INDEX projects_repo ON projects (installation_id, repo_full_name);
