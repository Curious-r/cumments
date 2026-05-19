-- Stores the mapping between a site_id and its corresponding Matrix Space.
CREATE TABLE IF NOT EXISTS sites (
    -- The site identifier (e.g., 'my-blog')
    id TEXT PRIMARY KEY,

    -- The Matrix room ID of the Space that contains all comment rooms for this site
    matrix_space_id TEXT NOT NULL,

    -- Optional display name for the site
    display_name TEXT,

    -- When this site was first registered/seen
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Add a foreign key to comments table to ensure data integrity (optional but good practice)
-- Note: SQLite doesn't easily allow adding FKs to existing tables without recreation,
-- so we will just create an index for now to speed up joins if we ever need them.
CREATE INDEX IF NOT EXISTS idx_comments_site_id ON comments (site_id);
