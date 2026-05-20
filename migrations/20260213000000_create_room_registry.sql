-- Create room_registry table to track Space-Room relationships
-- This implements Principle B: Registry via Space
CREATE TABLE IF NOT EXISTS room_registry (
    room_id TEXT PRIMARY KEY,
    site_id TEXT NOT NULL,
    post_slug TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT 1,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_room_registry_site_post ON room_registry(site_id, post_slug);
