-- The admin back office uses "replied" and "closed" for contact enquiries.
DO $$ BEGIN
  ALTER TYPE catering_status ADD VALUE IF NOT EXISTS 'replied';
EXCEPTION WHEN duplicate_object THEN NULL; END $$;
DO $$ BEGIN
  ALTER TYPE catering_status ADD VALUE IF NOT EXISTS 'closed';
EXCEPTION WHEN duplicate_object THEN NULL; END $$;
