//! Best-effort request_log writes (search / extract / research / MCP tools).

use std::time::Instant;

use serpotter_db::Db;

use crate::AppState;

/// Truncate query/url preview to 120 chars for storage.
pub fn query_preview(s: &str) -> String {
    let mut out: String = s.chars().take(120).collect();
    if s.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// Fire-and-forget insert into request_log. Never fails the request path.
///
/// `service` is the vendor family when known (e.g. `tavily`); `provider_used` is
/// the dial/route label (may be `hybrid`, `blend`, or same as service).
#[allow(clippy::too_many_arguments)]
pub fn spawn_log(
    state: &AppState,
    path: &'static str,
    status: i64,
    service: Option<String>,
    provider_used: Option<String>,
    error_kind: Option<&'static str>,
    query_preview: Option<String>,
    started: Instant,
) {
    spawn_log_db(
        state.db.clone(),
        path,
        status,
        service,
        provider_used,
        error_kind,
        query_preview,
        started,
    );
}

/// Same as [`spawn_log`] with an owned [`Db`] (MCP tools without full AppState).
#[allow(clippy::too_many_arguments)]
pub fn spawn_log_db(
    db: Db,
    path: &'static str,
    status: i64,
    service: Option<String>,
    provider_used: Option<String>,
    error_kind: Option<&'static str>,
    query_preview: Option<String>,
    started: Instant,
) {
    let duration_ms = started.elapsed().as_millis() as i64;
    tokio::spawn(async move {
        let _ = db
            .insert_request_log(
                path,
                "POST",
                status,
                service.as_deref(),
                provider_used.as_deref(),
                Some(duration_ms),
                error_kind,
                query_preview.as_deref(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await;
    });
}
