CREATE TABLE accounts (
    id            uuid PRIMARY KEY,
    email         text NOT NULL UNIQUE,
    password_hash text,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Email sign-up and passwordless sign-in both go through a mailed single-use link, so no
-- account exists for an address nobody proved to own. `password_hash` is only used when the
-- link creates the account.
CREATE TABLE magic_links (
    token_hash     bytea PRIMARY KEY,
    email          text NOT NULL,
    password_hash  text,
    client_id      text NOT NULL,
    redirect_uri   text NOT NULL,
    state          text NOT NULL,
    code_challenge text NOT NULL,
    expires_at     timestamptz NOT NULL,
    used_at        timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX magic_links_expires_idx ON magic_links (expires_at);

CREATE TABLE token_families (
    id         uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);
CREATE INDEX token_families_account_idx ON token_families (account_id);

CREATE TABLE auth_codes (
    code_hash      bytea PRIMARY KEY,
    account_id     uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    client_id      text NOT NULL,
    redirect_uri   text NOT NULL,
    code_challenge text NOT NULL,
    expires_at     timestamptz NOT NULL,
    used_at        timestamptz,
    -- Set when the code is exchanged, so a second exchange can revoke what the first one issued.
    family_id      uuid REFERENCES token_families (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX auth_codes_expires_idx ON auth_codes (expires_at);

CREATE TABLE access_tokens (
    token_hash bytea PRIMARY KEY,
    family_id  uuid NOT NULL REFERENCES token_families (id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);
CREATE INDEX access_tokens_expires_idx ON access_tokens (expires_at);

CREATE TABLE refresh_tokens (
    token_hash bytea PRIMARY KEY,
    family_id  uuid NOT NULL REFERENCES token_families (id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz
);
CREATE INDEX refresh_tokens_expires_idx ON refresh_tokens (expires_at);

CREATE TABLE preferences (
    account_id uuid PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    version    bigint NOT NULL,
    document   jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- No content, no token: who asked for a provider token, which provider, when.
CREATE TABLE realtime_token_audit (
    id         bigserial PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    action     text NOT NULL,
    provider   text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX realtime_token_audit_account_idx ON realtime_token_audit (account_id, created_at);
