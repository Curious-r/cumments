CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE rooms (
    room_id TEXT PRIMARY KEY,
    site_id TEXT NOT NULL,
    post_slug TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(site_id, post_slug)
);

CREATE TABLE comments (
    id TEXT PRIMARY KEY,
    room_id TEXT NOT NULL,

    author_id TEXT NOT NULL,
    author_name TEXT NOT NULL,
    is_guest BOOLEAN NOT NULL DEFAULT FALSE,
    author_fingerprint TEXT,

    content TEXT NOT NULL,
    is_redacted BOOLEAN NOT NULL DEFAULT FALSE,

    created_at DATETIME NOT NULL,
    updated_at DATETIME,

    reply_to TEXT,

    FOREIGN KEY(room_id) REFERENCES rooms(room_id) ON DELETE CASCADE
);

CREATE INDEX idx_rooms_lookup ON rooms(site_id, post_slug);
CREATE INDEX idx_comments_room_time ON comments(room_id, created_at);
