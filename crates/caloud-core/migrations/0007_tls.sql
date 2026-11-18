-- M4-08：Edge TLS 與憑證管理（ADR 0016）

-- FR-OPS-03、FR-OPS-14：Cloudflare 權杖以主金鑰加密保存
ALTER TABLE domain_pools ADD COLUMN cloudflare_token_enc TEXT;
ALTER TABLE domain_pools ADD COLUMN cloudflare_token_hint TEXT;
ALTER TABLE domain_pools ADD COLUMN cloudflare_zone_id TEXT;

-- FR-OPS-10、FR-DOM-05
CREATE TABLE certificates (
    name         TEXT PRIMARY KEY,                -- 網域池根網域或已驗證的網域
    kind         TEXT NOT NULL CHECK (kind IN ('dns01', 'http01')),
    names        TEXT NOT NULL,                   -- JSON：憑證涵蓋的名稱
    status       TEXT NOT NULL CHECK (status IN ('pending', 'issued', 'failed')),
    not_after    INTEGER,
    error        TEXT,
    attempts     INTEGER NOT NULL DEFAULT 0,
    next_attempt INTEGER,
    staging      INTEGER NOT NULL DEFAULT 0,      -- 以 Let's Encrypt staging 簽發
    updated_at   INTEGER NOT NULL
);
