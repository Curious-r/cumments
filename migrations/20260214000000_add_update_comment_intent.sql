-- Add updated_at to comments table
-- SQLite has restrictions on ADD COLUMN with non-constant defaults in some versions.
-- Using '2026-05-20 00:00:00' (a constant) or simply allowing it to be nullable/defaulted via CURRENT_TIMESTAMP if supported.
-- The error "non-constant default" usually refers to CURRENT_TIMESTAMP on ADD COLUMN in older SQLite.
-- Let's use a constant string that SQLite understands as a timestamp.
ALTER TABLE comments ADD COLUMN updated_at DATETIME NOT NULL DEFAULT '2026-01-01 00:00:00';

-- Create intent queue for updating comments
CREATE TABLE IF NOT EXISTS intent_queue_update_comment (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id TEXT NOT NULL,
    post_slug TEXT NOT NULL,
    event_id TEXT NOT NULL,
    content TEXT NOT NULL,
    author_fingerprint TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending', -- pending, processing, waiting_for_sync, completed, failed
    created_at DATETIME NOT NULL DEFAULT (STRFTIME('%Y-%m-%d %H:%M:%f', 'NOW')),
    updated_at DATETIME NOT NULL DEFAULT (STRFTIME('%Y-%m-%d %H:%M:%f', 'NOW'))
);

CREATE INDEX IF NOT EXISTS idx_intent_update_status ON intent_queue_update_comment(status);
