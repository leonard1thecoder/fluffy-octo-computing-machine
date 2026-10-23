-- One-time codes used to hand an authenticated OAuth session to the browser
-- without putting JWTs in a URL. The raw code is shown to the user once and
-- only its SHA-256 hash is stored; the code is single use and short lived.
CREATE TABLE IF NOT EXISTS oauth_exchange_codes (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  code_hash TEXT NOT NULL UNIQUE,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at TIMESTAMPTZ NOT NULL,
  consumed_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_oauth_exchange_user ON oauth_exchange_codes(user_id);
CREATE INDEX IF NOT EXISTS idx_oauth_exchange_expires ON oauth_exchange_codes(expires_at);

-- Supabase RLS (owner/bypass via backend role; anon denied).
ALTER TABLE oauth_exchange_codes ENABLE ROW LEVEL SECURITY;
