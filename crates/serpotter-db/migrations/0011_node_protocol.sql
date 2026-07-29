-- Per-node proxy scheme for reqwest::Proxy::all (http|https|socks5).
ALTER TABLE nodes ADD COLUMN protocol TEXT NOT NULL DEFAULT 'http';

UPDATE schema_version SET version = 11 WHERE id = 1;
