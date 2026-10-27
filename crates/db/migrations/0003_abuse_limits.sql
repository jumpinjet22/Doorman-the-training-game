-- Rate-limit counters and spent Turnstile tokens. Both live here rather than
-- in memory so the limits hold across several API instances.

-- One fixed window per (scope, key). `key` is a player id, or for new
-- players a salted hash of the client's network: never a raw IP address.
-- Rows older than 24 hours are deleted.
CREATE TABLE rate_limits (
    scope         text NOT NULL,
    key           bytea NOT NULL,
    window_start  timestamptz NOT NULL,
    hits          integer NOT NULL,
    PRIMARY KEY (scope, key, window_start)
);
CREATE INDEX rate_limits_window_start ON rate_limits (window_start);

-- SHA-256 of each Turnstile token already spent, so none is accepted twice
-- whatever Cloudflare says. Kept a little longer than a token lives.
CREATE TABLE turnstile_tokens (
    token_hash  bytea PRIMARY KEY,
    used_at     timestamptz NOT NULL
);
CREATE INDEX turnstile_tokens_used_at ON turnstile_tokens (used_at);
