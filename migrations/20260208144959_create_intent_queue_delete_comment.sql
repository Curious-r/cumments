-- A queue of intents to delete comments.
-- The reconciler will process these intents and redact the corresponding
-- messages in Matrix.
CREATE TABLE IF NOT EXISTS intent_queue_delete_comment (
    -- A unique ID for this intent
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- The intent payload, as a JSON object
    payload TEXT NOT NULL,

    -- The status of the intent: 'pending', 'completed', or 'failed'
    status TEXT NOT NULL DEFAULT 'pending',

    -- Timestamps
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
