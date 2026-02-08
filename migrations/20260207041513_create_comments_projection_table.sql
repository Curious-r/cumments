-- A read-only projection of comments, populated by the projectionist service.
CREATE TABLE IF NOT EXISTS comments (
    -- A unique ID for this row
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- The Matrix event ID for the comment, to prevent duplicates
    event_id TEXT NOT NULL UNIQUE,

    -- The Matrix room ID where the comment was posted
    room_id TEXT NOT NULL,

    -- The site and post this comment belongs to, for easy querying
    site_id TEXT NOT NULL,
    post_slug TEXT NOT NULL,

    -- Information about the author
    author_mxid TEXT NOT NULL,
    author_nickname TEXT, -- Display name at the time of posting

    -- The comment content itself
    content TEXT NOT NULL,

    -- The timestamp from the Matrix event (origin_server_ts)
    timestamp DATETIME NOT NULL,

    -- When the row was created in our database
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- An index to allow efficient lookup of comments for a given page
CREATE INDEX IF NOT EXISTS idx_comments_site_post ON comments (site_id, post_slug);
