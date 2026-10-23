-- Coupons can be scoped to one subscriber and/or one product.
ALTER TABLE coupons ADD COLUMN IF NOT EXISTS subscriber_email TEXT;
ALTER TABLE coupons ADD COLUMN IF NOT EXISTS product_id UUID REFERENCES products(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_coupons_subscriber_email ON coupons(subscriber_email);
CREATE INDEX IF NOT EXISTS idx_coupons_product_id ON coupons(product_id);

-- Announcements / promotions that the owner can blast to subscribers.
CREATE TABLE IF NOT EXISTS announcements (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
  body_html TEXT NOT NULL,
  body_text TEXT,
  audience TEXT NOT NULL DEFAULT 'subscribers' CHECK (audience IN ('all','subscribers')),
  status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','sent','failed')),
  sent_at TIMESTAMPTZ,
  created_by UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_announcements_status ON announcements(status);
CREATE INDEX IF NOT EXISTS idx_announcements_created_at ON announcements(created_at DESC);

-- Supabase RLS (owner/bypass via backend role; anon denied).
ALTER TABLE announcements ENABLE ROW LEVEL SECURITY;
