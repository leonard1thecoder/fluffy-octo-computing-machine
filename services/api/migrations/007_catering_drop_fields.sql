-- Contact-us catering flow no longer needs event date / guest count.
ALTER TABLE catering_requests DROP COLUMN IF EXISTS event_date;
ALTER TABLE catering_requests DROP COLUMN IF EXISTS guest_count;
