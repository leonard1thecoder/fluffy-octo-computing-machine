-- Remember where the user wanted to land before the OAuth round trip.
ALTER TABLE oauth_transactions ADD COLUMN IF NOT EXISTS next_path TEXT;
