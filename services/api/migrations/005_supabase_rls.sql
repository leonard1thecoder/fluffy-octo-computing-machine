-- Supabase hardening: deny PostgREST (anon/authenticated) access by default.
--
-- The Rust backend connects with the owner/service_role connection string, which
-- bypasses RLS, so no permissive policies are created here on purpose. Do NOT add
-- `FOR ... TO anon` policies unless the frontend reads these tables directly
-- through Supabase (it should not — it talks to the Rust API instead).
-- Do NOT use FORCE ROW LEVEL SECURITY: owners must keep bypassing RLS.
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE refresh_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_verification_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE password_reset_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_logs ENABLE ROW LEVEL SECURITY;
ALTER TABLE categories ENABLE ROW LEVEL SECURITY;
ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE product_variants ENABLE ROW LEVEL SECURITY;
ALTER TABLE inventory_movements ENABLE ROW LEVEL SECURITY;
ALTER TABLE delivery_zones ENABLE ROW LEVEL SECURITY;
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE order_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments ENABLE ROW LEVEL SECURITY;
ALTER TABLE payment_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE email_outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE catering_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE catering_quotes ENABLE ROW LEVEL SECURITY;
ALTER TABLE catering_quote_items ENABLE ROW LEVEL SECURITY;
