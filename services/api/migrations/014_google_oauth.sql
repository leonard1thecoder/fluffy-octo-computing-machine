-- Google OAuth 2.0 / OpenID Connect integration.
--
-- users.password_hash is now nullable: Google-only accounts have no password.
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;

-- One row per external identity linked to an application user.
-- provider = 'google', provider_user_id = Google's stable OIDC 'sub' claim.
CREATE TABLE IF NOT EXISTS user_identities (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL CHECK (char_length(provider) BETWEEN 1 AND 40),
  provider_user_id TEXT NOT NULL CHECK (char_length(provider_user_id) BETWEEN 1 AND 255),
  email TEXT,
  email_verified BOOLEAN NOT NULL DEFAULT false,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(provider, provider_user_id)
);
CREATE INDEX IF NOT EXISTS idx_user_identities_user ON user_identities(user_id);
CREATE INDEX IF NOT EXISTS idx_user_identities_provider_user ON user_identities(provider, provider_user_id);

-- Single-use OAuth transaction state (state, nonce, PKCE verifier/challenge).
-- Rows expire quickly; a cleanup job may delete old rows.
CREATE TABLE IF NOT EXISTS oauth_transactions (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  state TEXT NOT NULL UNIQUE,
  nonce TEXT NOT NULL UNIQUE,
  code_verifier TEXT NOT NULL,
  code_challenge TEXT NOT NULL,
  provider_user_id TEXT,
  email TEXT,
  expires_at TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_oauth_transactions_state ON oauth_transactions(state);
CREATE INDEX IF NOT EXISTS idx_oauth_transactions_expires ON oauth_transactions(expires_at);

-- Supabase RLS (owner/bypass via backend role; anon denied).
ALTER TABLE user_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE oauth_transactions ENABLE ROW LEVEL SECURITY;
