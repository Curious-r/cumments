CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rooms (
    room_id TEXT PRIMARY KEY,
    site_id TEXT NOT NULL,
    post_slug TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_rooms_site_slug ON rooms(site_id, post_slug);

CREATE TABLE IF NOT EXISTS profiles (
    user_id TEXT PRIMARY KEY,
    display_name TEXT,
    avatar_url TEXT,
    last_updated_at DATETIME NOT NULL
);

CREATE TABLE IF NOT EXISTS comments (
    id TEXT PRIMARY KEY,
    room_id TEXT NOT NULL,

    author_id TEXT NOT NULL,
    author_name TEXT NOT NULL,
    author_fingerprint TEXT,
    avatar_url TEXT,
    is_guest BOOLEAN NOT NULL DEFAULT FALSE,

    content TEXT NOT NULL,
    is_redacted BOOLEAN NOT NULL DEFAULT FALSE,

    reply_to TEXT,

    created_at DATETIME NOT NULL,
    updated_at DATETIME,

    txn_id TEXT,
    raw_event TEXT,

    FOREIGN KEY(room_id) REFERENCES rooms(room_id)
);

CREATE INDEX IF NOT EXISTS idx_comments_room_created ON comments(room_id, created_at);
CREATE INDEX IF NOT EXISTS idx_comments_txn_id ON comments(txn_id) WHERE txn_id IS NOT NULL;
