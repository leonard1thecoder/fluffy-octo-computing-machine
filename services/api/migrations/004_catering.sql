DO $$ BEGIN CREATE TYPE catering_status AS ENUM ('new','reviewing','quoted','accepted','declined'); EXCEPTION WHEN duplicate_object THEN NULL; END $$;
DO $$ BEGIN CREATE TYPE quote_status AS ENUM ('draft','sent','accepted','expired','declined'); EXCEPTION WHEN duplicate_object THEN NULL; END $$;

CREATE TABLE IF NOT EXISTS catering_requests (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID REFERENCES users(id) ON DELETE SET NULL,
  event_type TEXT NOT NULL,
  event_date DATE NOT NULL,
  guest_count INTEGER NOT NULL CHECK (guest_count BETWEEN 1 AND 5000),
  venue TEXT,
  budget_cents BIGINT CHECK (budget_cents IS NULL OR budget_cents >= 0),
  menu_requirements TEXT,
  contact_name TEXT NOT NULL,
  contact_email TEXT NOT NULL,
  contact_phone TEXT,
  status catering_status NOT NULL DEFAULT 'new',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS catering_quotes (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  request_id UUID NOT NULL REFERENCES catering_requests(id) ON DELETE CASCADE,
  total_cents BIGINT NOT NULL CHECK (total_cents >= 0),
  currency CHAR(3) NOT NULL DEFAULT 'ZAR',
  status quote_status NOT NULL DEFAULT 'draft',
  valid_until DATE NOT NULL,
  deposit_cents BIGINT CHECK (deposit_cents IS NULL OR deposit_cents >= 0),
  created_by UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS catering_quote_items (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  quote_id UUID NOT NULL REFERENCES catering_quotes(id) ON DELETE CASCADE,
  description TEXT NOT NULL,
  qty INTEGER NOT NULL CHECK (qty >= 1),
  unit_price_cents BIGINT NOT NULL CHECK (unit_price_cents >= 0),
  line_total_cents BIGINT NOT NULL CHECK (line_total_cents >= 0)
);
