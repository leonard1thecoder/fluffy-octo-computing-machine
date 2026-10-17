-- Contact-us catering flow drops optional fields.
ALTER TABLE catering_requests DROP COLUMN IF EXISTS venue;
ALTER TABLE catering_requests DROP COLUMN IF EXISTS budget_cents;
ALTER TABLE catering_requests DROP COLUMN IF EXISTS contact_phone;
