-- Outbound HTTP(S) proxy nodes for reqwest Proxy::all (lean).
CREATE TABLE IF NOT EXISTS nodes (
    id INTEGER PRIMARY KEY,
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    username TEXT,
    password TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    inflight INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_nodes_enabled ON nodes(enabled);

UPDATE schema_version SET version = 4 WHERE id = 1;
