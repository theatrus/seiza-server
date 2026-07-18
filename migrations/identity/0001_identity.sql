CREATE TABLE accounts (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    email_lookup TEXT NOT NULL UNIQUE,
    email_verified_at TEXT NOT NULL,
    webauthn_user_handle TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_authenticated_at TEXT NOT NULL
);

CREATE TABLE auth_challenges (
    id TEXT PRIMARY KEY,
    purpose TEXT NOT NULL,
    account_id TEXT,
    email_lookup TEXT,
    link_token_digest TEXT,
    code_digest TEXT,
    webauthn_state_json TEXT,
    attempts BIGINT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    FOREIGN KEY (account_id) REFERENCES accounts(id)
);

CREATE INDEX auth_challenges_email_purpose_consumed_idx
    ON auth_challenges(email_lookup, purpose, consumed_at);

CREATE TABLE auth_sessions (
    id TEXT PRIMARY KEY,
    token_digest TEXT NOT NULL,
    account_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    csrf_digest TEXT,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    absolute_expires_at TEXT NOT NULL,
    revoked_at TEXT,
    FOREIGN KEY (account_id) REFERENCES accounts(id)
);

CREATE INDEX auth_sessions_account_idx ON auth_sessions(account_id);
CREATE INDEX auth_sessions_expires_idx ON auth_sessions(expires_at);

CREATE TABLE passkey_credentials (
    id TEXT PRIMARY KEY,
    credential_id TEXT NOT NULL UNIQUE,
    account_id TEXT NOT NULL,
    credential_json TEXT NOT NULL,
    label TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT,
    FOREIGN KEY (account_id) REFERENCES accounts(id)
);

CREATE INDEX passkey_credentials_account_idx
    ON passkey_credentials(account_id);

CREATE TABLE api_keys (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    secret_digest TEXT NOT NULL,
    display_prefix TEXT NOT NULL,
    name TEXT NOT NULL,
    scopes_json TEXT NOT NULL,
    queue_weight DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    last_used_at TEXT,
    revoked_at TEXT,
    FOREIGN KEY (account_id) REFERENCES accounts(id)
);

CREATE INDEX api_keys_account_idx ON api_keys(account_id);
