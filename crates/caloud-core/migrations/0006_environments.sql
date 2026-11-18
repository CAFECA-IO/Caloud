-- M4-10 平臺環境（FR-OPS-13、ADR 0015）：維運者經 Admin API 在 Runner 開出的環境

CREATE TABLE environments (
    subdomain      TEXT PRIMARY KEY,
    hostname       TEXT NOT NULL UNIQUE,
    kind           TEXT NOT NULL,               -- production | staging | verify
    node           TEXT NOT NULL REFERENCES nodes(id),
    subnet_index   INTEGER NOT NULL,            -- 節點內網段索引（sandbox::EnvNet）
    compose        TEXT NOT NULL,
    public_service TEXT NOT NULL,
    public_port    INTEGER NOT NULL,
    public_ip      TEXT,                        -- running 時對外服務的固定 IP
    cpu_millis     INTEGER NOT NULL,            -- 調度用的配置量
    memory_bytes   INTEGER NOT NULL,
    status         TEXT NOT NULL,               -- deploying | running | failed | destroying
    error          TEXT,
    services       TEXT NOT NULL DEFAULT '[]',  -- [{name, ip}]
    created_by     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    UNIQUE (node, subnet_index)
);
