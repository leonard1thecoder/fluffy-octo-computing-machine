-- Contact-us flow submits catering requests without event date / guest count.
ALTER TABLE catering_requests ALTER COLUMN event_date DROP NOT NULL;
ALTER TABLE catering_requests ALTER COLUMN guest_count DROP NOT NULL;
