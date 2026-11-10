-- M4 平臺基礎（ADR 0010、ADR 0011）：網域資源與 Runner 節點

-- FR-OPS-03：網域池。purposes 為 JSON 陣列（production／staging／verify／system）
CREATE TABLE domain_pools (
    root            TEXT PRIMARY KEY,
    purposes        TEXT NOT NULL,
    cloudflare_zone TEXT,
    id_lengths      TEXT NOT NULL,          -- JSON {"production":8,"staging":12,"verify":16}
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

-- FR-OPS-03、FR-ENV-07：子網域保留字
CREATE TABLE reserved_names (
    name       TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);

INSERT INTO reserved_names (name, created_at) VALUES
    ('www', 0), ('app', 0), ('api', 0), ('mcp', 0), ('docs', 0), ('status', 0),
    ('admin', 0), ('login', 0), ('cafeca', 0), ('caloud', 0);

-- FR-OPS-03：封鎖的主機名稱（平臺子網域或自訂網域）
CREATE TABLE blocked_hosts (
    hostname   TEXT PRIMARY KEY,
    reason     TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- FR-OPS-04、FR-OPS-06：Runner 節點。owner 為 NULL 表示平臺節點
CREATE TABLE nodes (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    owner           TEXT REFERENCES users(account) ON DELETE CASCADE,
    specs           TEXT NOT NULL,          -- JSON NodeSpecs
    alloc_cpu       INTEGER NOT NULL,       -- 可配置量（millicores）
    alloc_mem       INTEGER NOT NULL,       -- 可配置量（bytes）
    used_cpu        INTEGER NOT NULL DEFAULT 0,
    used_mem        INTEGER NOT NULL DEFAULT 0,
    assigned_cpu    INTEGER NOT NULL DEFAULT 0,  -- 已配置量，由調度更新
    assigned_mem    INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'active',
    token_hash      TEXT NOT NULL UNIQUE,   -- keccak256(節點權杖)
    last_seen       INTEGER NOT NULL,
    created_at      INTEGER NOT NULL
);

-- FR-OPS-04：一次性加入權杖
CREATE TABLE node_join_tokens (
    token_hash TEXT PRIMARY KEY,
    owner      TEXT REFERENCES users(account) ON DELETE CASCADE,
    created_by TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);
