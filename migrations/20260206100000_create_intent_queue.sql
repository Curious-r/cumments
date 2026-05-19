-- Create the queue table for PostCommentIntent
-- This table will be polled by the MessageDispatchReconciler.
CREATE TABLE intent_queue_post_comment (
    -- A unique ID for each intent
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- The serialized JSON payload of the PostCommentIntent
    payload TEXT NOT NULL,

    -- The status of the intent processing
    -- 'pending': Ready to be picked up
    -- 'processing': Picked up by a reconciler, work in progress
    -- 'failed': Processing failed, may need manual intervention
    status TEXT NOT NULL DEFAULT 'pending',

    -- The number of times processing has been attempted
    retry_count INTEGER NOT NULL DEFAULT 0,

    -- Timestamps for tracking and potential cleanup
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
