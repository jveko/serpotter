//! Best-effort request_log writes (search / extract / research).

use std::time::Instant;

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
pub fn spawn_log(
    state: &AppState,
    path: &'static str,
    status: i64,
    provider_used: Option<String>,
    error_kind: Option<&'static str>,
    query_preview: Option<String>,
    started: Instant,
) {
    let db = state.db.clone();
    let duration_ms = started.elapsed().as_millis() as i64;
    let service = provider_used.clone();
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
            )
            .await;
    });
}
