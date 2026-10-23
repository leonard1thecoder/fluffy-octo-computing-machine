-- Newsletter subscriptions get a human-readable receipt reference, the same
-- way orders do, so the customer and the back office can talk about a
-- subscription with a short code.
ALTER TABLE subscribers ADD COLUMN IF NOT EXISTS reference TEXT;
UPDATE subscribers
   SET reference = 'NL-' || to_char(created_at, 'YYYYMMDD') || '-' || upper(substr(replace(id::text, '-', ''), 1, 6))
 WHERE reference IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_subscribers_reference ON subscribers(reference);
