-- Holder-set leases. Shared api_keys/nodes counters alone cannot distinguish a
-- late release from a newer holder, and expiring one holder must not zero or
-- clear unrelated live holders. Each acquire owns one child row; all finish and
-- refresh operations are scoped by its globally unique token.
--
-- The REFERENCES clauses express intent only: this connection pool does not
-- enable SQLite foreign-key enforcement, so row-delete paths explicitly clear
-- holder rows before deleting their parent.
CREATE TABLE api_key_leases (
    token INTEGER PRIMARY KEY AUTOINCREMENT,
    api_key_id INTEGER NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    lease_until TEXT NOT NULL
);
CREATE INDEX idx_api_key_leases_key_id ON api_key_leases(api_key_id);
CREATE INDEX idx_api_key_leases_expiry ON api_key_leases(lease_until);

CREATE TABLE node_leases (
    token INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    lease_until TEXT NOT NULL
);
CREATE INDEX idx_node_leases_node_id ON node_leases(node_id);
CREATE INDEX idx_node_leases_expiry ON node_leases(lease_until);

UPDATE schema_version SET version = 19 WHERE id = 1;
