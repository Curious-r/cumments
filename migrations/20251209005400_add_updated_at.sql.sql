ALTER TABLE comments ADD COLUMN updated_at DATETIME;

UPDATE comments SET updated_at = created_at;
