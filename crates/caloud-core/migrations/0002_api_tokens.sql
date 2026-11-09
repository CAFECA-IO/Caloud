-- FR-MCP-02／03：個人 API 權杖（ADR 0008）
CREATE TABLE api_tokens (
    id           TEXT PRIMARY KEY,                 -- uuid
    account      TEXT NOT NULL REFERENCES users(account) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,             -- keccak256(token)
    prefix       TEXT NOT NULL,                    -- 前 8 字元，供使用者辨識
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    last_used_at INTEGER
);

CREATE INDEX api_tokens_account ON api_tokens(account);
