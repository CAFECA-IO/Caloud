-- 使用者：CAFECA 身分合約地址即唯一 ID
CREATE TABLE users (
    account      TEXT PRIMARY KEY,                 -- 0x… 小寫
    handle       TEXT,
    role         TEXT NOT NULL CHECK (role IN ('super_admin', 'admin', 'member', 'pending', 'disabled')),
    created_at   INTEGER NOT NULL,
    last_login   INTEGER NOT NULL
);

CREATE TABLE sessions (
    token_hash   TEXT PRIMARY KEY,                 -- sha256(token)，DB 外洩也無法冒用
    account      TEXT NOT NULL REFERENCES users(account) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
);

CREATE TABLE login_nonces (
    nonce        TEXT PRIMARY KEY,
    expires_at   INTEGER NOT NULL
);

-- 一個 App = 一份 docker compose 專案
CREATE TABLE apps (
    id           TEXT PRIMARY KEY,                 -- uuid，同時作為 compose project name 的一部分
    name         TEXT NOT NULL,
    owner        TEXT NOT NULL REFERENCES users(account),
    compose_yaml TEXT NOT NULL,                    -- 使用者上傳的原始檔
    status       TEXT NOT NULL CHECK (status IN ('stopped', 'deploying', 'running', 'failed')),
    last_error   TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

-- 托管的網域，綁定到某個 App 的 service:port
CREATE TABLE domains (
    hostname     TEXT PRIMARY KEY,                 -- 小寫、無結尾點
    owner        TEXT NOT NULL REFERENCES users(account),
    verify_token TEXT NOT NULL,                    -- DNS TXT _caloud-challenge.<hostname>
    verified     INTEGER NOT NULL DEFAULT 0,
    app_id       TEXT REFERENCES apps(id) ON DELETE SET NULL,
    service      TEXT,
    port         INTEGER,
    tls_status   TEXT NOT NULL DEFAULT 'none' CHECK (tls_status IN ('none', 'pending', 'issued', 'failed')),
    created_at   INTEGER NOT NULL
);

CREATE TABLE audit_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account      TEXT,
    action       TEXT NOT NULL,
    target       TEXT,
    detail       TEXT,
    at           INTEGER NOT NULL
);
