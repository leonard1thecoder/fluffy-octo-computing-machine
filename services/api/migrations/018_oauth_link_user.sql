-- OAuth transactions can optionally be bound to an already-authenticated user.
-- When set, the callback links the Google identity to that user instead of
-- looking up / creating an account by email.
ALTER TABLE oauth_transactions
  ADD COLUMN IF NOT EXISTS link_user_id UUID REFERENCES users(id) ON DELETE CASCADE;
CREATE INDEX IF NOT EXISTS idx_oauth_transactions_link_user ON oauth_transactions(link_user_id);
