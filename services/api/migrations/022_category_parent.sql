-- Allow categories to be nested under a top-level parent category.
-- Top-level categories have parent_id IS NULL (e.g. "Cake & cookies", "Dessert").
-- Sub-categories reference their parent via parent_id.
ALTER TABLE categories ADD COLUMN IF NOT EXISTS parent_id UUID REFERENCES categories(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_categories_parent ON categories(parent_id);
