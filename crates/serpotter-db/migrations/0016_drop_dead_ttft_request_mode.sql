-- C3a: drop the dead B22 columns from request_log. ttft_ms and request_mode
-- were threaded through the log pipeline as placeholders for a streaming /v1
-- wave that does not exist (B22 explicitly deferred); no surface ever wrote a
-- non-NULL value. SQLite DROP COLUMN (3.35+) is supported; the columns were
-- nullable and are referenced by no index, trigger, or view, so the drop is
-- safe in place.
ALTER TABLE request_log DROP COLUMN ttft_ms;
ALTER TABLE request_log DROP COLUMN request_mode;

UPDATE schema_version SET version = 16 WHERE id = 1;