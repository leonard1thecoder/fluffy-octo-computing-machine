-- Newsletter subscribers with double opt-in via Brevo.
CREATE TABLE IF NOT EXISTS subscribers (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  email TEXT NOT NULL UNIQUE CHECK (char_length(email) BETWEEN 1 AND 320),
  email_normalized TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','verified','unsubscribed')),
  verification_token_hash TEXT UNIQUE,
  verification_expires_at TIMESTAMPTZ,
  subscribed_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_subscribers_email_normalized ON subscribers(email_normalized);
CREATE INDEX IF NOT EXISTS idx_subscribers_status ON subscribers(status);

-- Supabase RLS (owner/bypass via backend role; anon denied).
ALTER TABLE subscribers ENABLE ROW LEVEL SECURITY;
