-- Admin request-log filter indexes: status and path_prefix LIKE lookups
-- in list_request_logs currently scan without an index.
CREATE INDEX IF NOT EXISTS idx_request_log_status ON request_log(status);
CREATE INDEX IF NOT EXISTS idx_request_log_path ON request_log(path);

UPDATE schema_version SET version = 13 WHERE id = 1;
