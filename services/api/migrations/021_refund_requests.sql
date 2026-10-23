-- Customer-initiated refund requests: the customer asks, the back office decides.
ALTER TABLE refunds ADD COLUMN IF NOT EXISTS decided_by UUID REFERENCES users(id) ON DELETE SET NULL;
ALTER TABLE refunds ADD COLUMN IF NOT EXISTS decided_at TIMESTAMPTZ;
ALTER TABLE refunds ADD COLUMN IF NOT EXISTS decision_note TEXT;

-- Widen the status check to cover the request/decision states.
ALTER TABLE refunds DROP CONSTRAINT IF EXISTS refunds_status_check;
ALTER TABLE refunds ADD CONSTRAINT refunds_status_check
  CHECK (status IN ('requested','pending','succeeded','failed','rejected','manual_action_required'));

CREATE INDEX IF NOT EXISTS idx_refunds_requested ON refunds(status) WHERE status = 'requested';
