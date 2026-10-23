-- Refunds. A refund records the gross amount, the bank charge (default 8%)
-- and the net amount actually returned to the customer.
DO $$ BEGIN ALTER TYPE payment_status ADD VALUE IF NOT EXISTS 'refunded'; EXCEPTION WHEN duplicate_object THEN NULL; END $$;
DO $$ BEGIN ALTER TYPE payment_status ADD VALUE IF NOT EXISTS 'partially_refunded'; EXCEPTION WHEN duplicate_object THEN NULL; END $$;

CREATE TABLE IF NOT EXISTS refunds (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  order_id UUID NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  payment_id UUID REFERENCES payments(id) ON DELETE SET NULL,
  merchant_payment_id TEXT,
  provider_txn_id TEXT,
  -- Amount being refunded from the order.
  gross_cents BIGINT NOT NULL CHECK (gross_cents > 0),
  -- Bank/processing charge deducted from the refund.
  bank_charge_cents BIGINT NOT NULL CHECK (bank_charge_cents >= 0),
  -- What the customer actually receives: gross - bank charge.
  net_cents BIGINT NOT NULL CHECK (net_cents >= 0),
  bank_charge_pct INTEGER NOT NULL DEFAULT 8 CHECK (bank_charge_pct BETWEEN 0 AND 100),
  currency CHAR(3) NOT NULL DEFAULT 'ZAR',
  reason TEXT,
  -- pending | succeeded | failed | manual_action_required
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','succeeded','failed','manual_action_required')),
  provider_ref TEXT,
  failure_reason TEXT,
  requested_by UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  completed_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_refunds_order ON refunds(order_id);
CREATE INDEX IF NOT EXISTS idx_refunds_status ON refunds(status);
CREATE INDEX IF NOT EXISTS idx_refunds_created ON refunds(created_at DESC);

-- Supabase RLS (owner/bypass via backend role; anon denied).
ALTER TABLE refunds ENABLE ROW LEVEL SECURITY;
