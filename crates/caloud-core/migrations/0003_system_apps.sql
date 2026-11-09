-- FR-APP-13～16、FR-DOM-09～10（ADR 0009）
ALTER TABLE apps ADD COLUMN source TEXT NOT NULL DEFAULT 'compose' CHECK (source IN ('compose', 'git'));
ALTER TABLE apps ADD COLUMN git_url TEXT;
ALTER TABLE apps ADD COLUMN git_ref TEXT;
ALTER TABLE apps ADD COLUMN git_path TEXT;
ALTER TABLE apps ADD COLUMN compose_file TEXT;
ALTER TABLE apps ADD COLUMN services TEXT;            -- JSON 陣列；NULL = 全部
ALTER TABLE apps ADD COLUMN env TEXT;                 -- .env 內容（M3 加密，FR-APP-11）
ALTER TABLE apps ADD COLUMN privileged INTEGER NOT NULL DEFAULT 0;

ALTER TABLE domains ADD COLUMN tls_mode TEXT NOT NULL DEFAULT 'none' CHECK (tls_mode IN ('none', 'passthrough'));
ALTER TABLE domains ADD COLUMN tls_port INTEGER;
ALTER TABLE domains ADD COLUMN target_host TEXT;      -- 綁定時解析：容器 alias 或 host.docker.internal
