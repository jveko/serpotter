//! Best-effort request_log writes (search / extract / research / MCP tools).

use std::time::Instant;

use axum::http::{request::Parts, HeaderMap};
use serpotter_auth::extract_token;
use serpotter_db::{Db, TokenRow};
use serpotter_product::ExecMeta;

use crate::AppState;

/// Fields for one request_log insert (service = vendor family; provider_used = dial label).
#[derive(Clone, Debug)]
pub struct LogFields {
    pub path: &'static str,
    pub status: i64,
    pub service: Option<String>,
    pub provider_used: Option<String>,
    pub error_kind: Option<&'static str>,
    pub query_preview: Option<String>,
    pub request_id: Option<String>,
    pub token_name: Option<String>,
    pub strategy: Option<String>,
    pub providers_consulted: Option<String>,
    pub attempt_count: Option<i64>,
    pub key_id: Option<i64>,
    pub node_id: Option<i64>,
}

/// Truncate query/url preview to 120 chars for storage.
pub fn query_preview(s: &str) -> String {
    let mut out: String = s.chars().take(120).collect();
    if s.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// Read `x-request-id` (SetRequestId already set it on the request before handlers).
pub fn request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Multi-leg dial labels — never stored in `service`.
fn is_dial_label(s: &str) -> bool {
    matches!(s, "hybrid" | "blend" | "blend-verify" | "verify")
}

/// Vendor family for `service`: never hybrid/blend; first consulted on dial labels.
/// On bare meta (errors): last attempted vendor when `attempt_count > 0`.
pub fn service_from_meta(provider_used: Option<&str>, meta: &ExecMeta) -> Option<String> {
    if let Some(pu) = provider_used {
        if is_dial_label(pu) {
            return meta.providers_consulted.first().cloned();
        }
        return Some(pu.to_string());
    }
    if meta.attempt_count > 0 {
        meta.providers_consulted
            .last()
            .cloned()
            .or_else(|| meta.providers_consulted.first().cloned())
    } else {
        None
    }
}

/// Dial / route label for research rows: strategy with `verify` → `blend-verify`
/// (matches search `provider_used`). Strategy column still stores raw strategy.
/// `single` / missing → first consulted vendor (or None).
pub fn research_dial_label(meta: &ExecMeta) -> Option<String> {
    match meta.strategy.as_deref() {
        Some("verify") => Some("blend-verify".into()),
        Some("single") | None => meta.providers_consulted.first().cloned(),
        Some(s) => Some(s.to_string()),
    }
}

/// Build log fields from product ExecMeta + dial label + auth/correlation.
/// (8 positional fields mirror the LogFields rows; matches `insert_request_log`
/// convention of allowing the param list.)
#[allow(clippy::too_many_arguments)]
pub fn fields_from_meta(
    path: &'static str,
    status: i64,
    error_kind: Option<&'static str>,
    query_preview: Option<String>,
    request_id: Option<String>,
    token_name: Option<String>,
    provider_used: Option<String>,
    meta: &ExecMeta,
) -> LogFields {
    let service = service_from_meta(provider_used.as_deref(), meta);
    LogFields {
        path,
        status,
        service,
        provider_used,
        error_kind,
        query_preview,
        request_id,
        token_name,
        strategy: meta.strategy.clone(),
        providers_consulted: meta.providers_csv(),
        attempt_count: Some(i64::from(meta.attempt_count)),
        key_id: meta.key_id,
        node_id: meta.node_id,
    }
}

/// Resolve MCP token_name + request_id from HTTP Parts (extensions + headers).
///
/// Prefers `TokenRow` stashed by mcp_auth_middleware; falls back to
/// `get_token_by_value` so valid tok- never leaves token_name NULL.
pub async fn resolve_mcp_log_ctx(db: &Db, parts: &Parts) -> (Option<String>, Option<String>) {
    let request_id = request_id_from_headers(&parts.headers);
    if let Some(row) = parts.extensions.get::<TokenRow>() {
        return (Some(row.name.clone()), request_id);
    }
    if let Some(tok) = extract_token(&parts.headers) {
        if let Ok(Some(row)) = db.get_token_by_value(&tok).await {
            return (Some(row.name), request_id);
        }
    }
    (None, request_id)
}

/// Fire-and-forget insert into request_log. Never fails the request path.
pub fn spawn_log(state: &AppState, fields: LogFields, started: Instant) {
    spawn_log_db(state.db.clone(), fields, started);
}

/// Same as [`spawn_log`] with an owned [`Db`] (MCP tools without full AppState).
pub fn spawn_log_db(db: Db, fields: LogFields, started: Instant) {
    let duration_ms = started.elapsed().as_millis() as i64;
    tokio::spawn(async move {
        if let Err(e) = db
            .insert_request_log(
                fields.path,
                "POST",
                fields.status,
                fields.service.as_deref(),
                fields.provider_used.as_deref(),
                Some(duration_ms),
                fields.error_kind,
                fields.query_preview.as_deref(),
                fields.request_id.as_deref(),
                fields.token_name.as_deref(),
                fields.strategy.as_deref(),
                fields.providers_consulted.as_deref(),
                fields.attempt_count,
                fields.key_id,
                fields.node_id,
            )
            .await
        {
            tracing::warn!(error = %e, "insert_request_log failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_dial_uses_first_consulted_as_service() {
        let mut meta = ExecMeta::default();
        meta.note_attempt("tavily", 1, None, true);
        meta.note_attempt("firecrawl", 2, None, false);
        assert_eq!(
            service_from_meta(Some("hybrid"), &meta).as_deref(),
            Some("tavily")
        );
    }

    #[test]
    fn single_provider_service_matches_dial() {
        let mut meta = ExecMeta::default();
        meta.note_attempt("exa", 3, Some(9), true);
        assert_eq!(
            service_from_meta(Some("exa"), &meta).as_deref(),
            Some("exa")
        );
    }

    #[test]
    fn error_with_attempts_uses_last_consulted() {
        let mut meta = ExecMeta::default();
        meta.note_attempt("tavily", 1, None, false);
        meta.note_attempt("firecrawl", 2, None, false);
        assert_eq!(
            service_from_meta(None, &meta).as_deref(),
            Some("firecrawl")
        );
    }

    #[test]
    fn research_dial_verify_maps_to_blend_verify() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("verify".into());
        meta.note_attempt("tavily", 1, None, true);
        assert_eq!(
            research_dial_label(&meta).as_deref(),
            Some("blend-verify")
        );
        // strategy column stays raw when fields_from_meta is used
        let f = fields_from_meta(
            "/api/research",
            200,
            None,
            None,
            None,
            None,
            research_dial_label(&meta),
            &meta,
        );
        assert_eq!(f.provider_used.as_deref(), Some("blend-verify"));
        assert_eq!(f.strategy.as_deref(), Some("verify"));
        assert_eq!(f.service.as_deref(), Some("tavily"));
    }

    #[test]
    fn research_dial_hybrid_keeps_hybrid() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("hybrid".into());
        meta.note_attempt("firecrawl", 2, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("hybrid"));
    }

    #[test]
    fn research_dial_single_uses_first_vendor() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("single".into());
        meta.note_attempt("exa", 3, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("exa"));
    }

    #[test]
    fn error_before_vendor_service_none() {
        let meta = ExecMeta::default();
        assert!(service_from_meta(None, &meta).is_none());
    }
}
