-- Observability: correlate request_log with x-request-id and Approach-2 audit fields.
ALTER TABLE request_log ADD COLUMN request_id TEXT;
ALTER TABLE request_log ADD COLUMN token_name TEXT;
ALTER TABLE request_log ADD COLUMN strategy TEXT;
ALTER TABLE request_log ADD COLUMN providers_consulted TEXT;
ALTER TABLE request_log ADD COLUMN attempt_count INTEGER;
ALTER TABLE request_log ADD COLUMN key_id INTEGER;
ALTER TABLE request_log ADD COLUMN node_id INTEGER;

CREATE INDEX IF NOT EXISTS idx_request_log_request_id ON request_log(request_id);

UPDATE schema_version SET version = 12 WHERE id = 1;
