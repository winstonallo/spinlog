CREATE TABLE IF NOT EXISTS oauth_accounts (
    user_id      TEXT NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    provider     TEXT NOT NULL,
    provider_uid TEXT NOT NULL,
    PRIMARY KEY (provider, provider_uid)
);

CREATE TABLE IF NOT EXISTS oauth_states (
    csrf_token    TEXT PRIMARY KEY,
    pkce_verifier TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    expires_at TEXT NOT NULL
);
