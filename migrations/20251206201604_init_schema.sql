CREATE TABLE IF NOT EXISTS comments (
    id TEXT PRIMARY KEY NOT NULL,
    site_id TEXT NOT NULL DEFAULT 'default',
    post_slug TEXT NOT NULL,
    author_id TEXT NOT NULL,
    author_name TEXT NOT NULL,
    is_guest BOOLEAN NOT NULL DEFAULT 0,
    is_redacted BOOLEAN NOT NULL DEFAULT 0,
    content TEXT NOT NULL,
    created_at DATETIME NOT NULL,
    reply_to TEXT
);

CREATE INDEX IF NOT EXISTS idx_comments_lookup
ON comments(site_id, post_slug, created_at);

CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
