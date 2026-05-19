-- Add matrix_event_id to PostComment intents to track the resulting Matrix event
ALTER TABLE intent_queue_post_comment ADD COLUMN matrix_event_id TEXT;

-- Index for the projector to quickly find intents waiting for sync
CREATE INDEX IF NOT EXISTS idx_post_intents_event_id ON intent_queue_post_comment (matrix_event_id);

-- Optional: Add similar tracking to delete intents if we want closed-loop for deletions too
ALTER TABLE intent_queue_delete_comment ADD COLUMN target_event_id TEXT;
CREATE INDEX IF NOT EXISTS idx_delete_intents_target_id ON intent_queue_delete_comment (target_event_id);
