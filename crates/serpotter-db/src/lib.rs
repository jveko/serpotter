//! SQLite (sqlx) pool + migrations for Serpotter.

pub const EXPECTED_SCHEMA_VERSION: i64 = 1;

/// Placeholder until connect_and_migrate lands.
pub fn expected_schema_version() -> i64 {
    EXPECTED_SCHEMA_VERSION
}
