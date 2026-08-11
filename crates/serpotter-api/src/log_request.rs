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

/// Dial / route label for research rows. With F16 the `strategy` column stores
/// the RAW routed strategy (fast/balanced/verify/deep), so the research dial
/// label must be derived from it, matching search `provider_used`:
/// `verify` → `blend-verify` (3-leg), `balanced` → `blend` (2-leg),
/// anything else (fast/deep — single chains) → first consulted vendor.
pub fn research_dial_label(meta: &ExecMeta) -> Option<String> {
    match meta.strategy.as_deref() {
        Some("verify") => Some("blend-verify".into()),
        Some("balanced") => Some("blend".into()),
        _ => meta.providers_consulted.first().cloned(),
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

/// API-token extractor that LOGS failed authentication (F08).
///
/// Identical semantics to `crate::ApiToken` (parts-level `FromRequestParts`,
/// so auth still wins over body parsing, F01) but on a rejected token it
/// fire-and-forgets a 401 `request_log` row before returning the 401 —
/// otherwise failed auth attempts (missing/invalid token) are invisible in
/// the admin request-log surface. `token_name` stays `None`: the token either
/// does not exist or is not present, so there is no name to attribute.
pub struct ApiTokenLogged(pub serpotter_db::TokenRow);

/// Map a request URI to the static path label stored in request_log.
fn static_product_path(uri_path: &str) -> &'static str {
    match uri_path {
        "/api/search" => "/api/search",
        "/api/extract" => "/api/extract",
        "/api/research" => "/api/research",
        _ => "/api",
    }
}

#[allow(clippy::result_large_err)]
impl axum::extract::FromRequestParts<AppState> for ApiTokenLogged {
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match crate::require_api_token(state, &parts.headers).await {
            Ok(row) => Ok(ApiTokenLogged(row)),
            Err(rejection) => {
                // F08: failed auth gets a request_log row — status 401,
                // request_id from the inbound header (post SetRequestId),
                // path from the URI; the body was never parsed so no preview.
                let fields = LogFields {
                    path: static_product_path(parts.uri.path()),
                    status: 401,
                    service: None,
                    provider_used: None,
                    error_kind: Some("Unauthorized"),
                    query_preview: None,
                    request_id: request_id_from_headers(&parts.headers),
                    token_name: None,
                    strategy: None,
                    providers_consulted: None,
                    attempt_count: None,
                    key_id: None,
                    node_id: None,
                };
                spawn_log(state, fields, Instant::now());
                Err(rejection)
            }
        }
    }
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
        assert_eq!(service_from_meta(None, &meta).as_deref(), Some("firecrawl"));
    }

    #[test]
    fn research_dial_verify_maps_to_blend_verify() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("verify".into());
        meta.note_attempt("tavily", 1, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("blend-verify"));
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
    fn research_dial_balanced_maps_to_blend() {
        // F16: strategy stores the raw routed strategy ("balanced" for a
        // 2-leg blend); the research dial label derives "blend" from it.
        let mut meta = ExecMeta::default();
        meta.strategy = Some("balanced".into());
        meta.note_attempt("tavily", 1, None, true);
        meta.note_attempt("firecrawl", 2, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("blend"));
    }

    #[test]
    fn research_dial_fast_uses_first_vendor() {
        // F16: a fast single-chain web leg (raw strategy "fast") maps to the
        // first consulted vendor, not the raw strategy string.
        let mut meta = ExecMeta::default();
        meta.strategy = Some("fast".into());
        meta.note_attempt("tavily", 1, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("tavily"));
    }

    #[test]
    fn research_dial_single_uses_first_vendor() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("single".into());
        meta.note_attempt("exa", 3, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("exa"));
    }

    // ---- F60/D9: success-path fields_from_meta mapping with a REAL ExecMeta ----

    /// A single successful provider attempt: every log field maps from meta.
    #[test]
    fn fields_from_meta_success_single_provider() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("fast".into());
        meta.note_attempt("tavily", 42, Some(7), true);
        let f = fields_from_meta(
            "/api/search",
            200,
            None,
            Some("hello world".into()),
            Some("req-123".into()),
            Some("tok-a".into()),
            Some("tavily".into()),
            &meta,
        );
        assert_eq!(f.service.as_deref(), Some("tavily"));
        assert_eq!(f.provider_used.as_deref(), Some("tavily"));
        assert_eq!(f.strategy.as_deref(), Some("fast"));
        assert_eq!(f.providers_consulted.as_deref(), Some("tavily"));
        assert_eq!(f.attempt_count, Some(1));
        assert_eq!(f.key_id, Some(42));
        assert_eq!(f.node_id, Some(7));
        assert_eq!(f.status, 200);
        assert_eq!(f.error_kind, None);
    }

    /// Multi-leg success: sticky LAST success key/node wins; providers and
    /// attempts accumulate first-seen.
    #[test]
    fn fields_from_meta_success_multi_leg_sticky_last() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("balanced".into());
        meta.note_attempt("tavily", 1, Some(10), false);
        meta.note_attempt("firecrawl", 2, Some(11), true);
        meta.note_attempt("exa", 3, Some(12), true);
        let f = fields_from_meta(
            "/api/search",
            200,
            None,
            None,
            Some("req-456".into()),
            Some("tok-b".into()),
            Some("blend".into()),
            &meta,
        );
        // provider_used is a dial label → service = first consulted real vendor.
        assert_eq!(f.service.as_deref(), Some("tavily"));
        assert_eq!(f.provider_used.as_deref(), Some("blend"));
        assert_eq!(
            f.providers_consulted.as_deref(),
            Some("tavily,firecrawl,exa")
        );
        assert_eq!(f.attempt_count, Some(3));
        // sticky last success = exa's hold, not the failed tavily attempt.
        assert_eq!(f.key_id, Some(3));
        assert_eq!(f.node_id, Some(12));
        assert_eq!(f.strategy.as_deref(), Some("balanced"));
    }

    /// Hybrid with a x-leg success: first-seen order and last-success key.
    #[test]
    fn fields_from_meta_hybrid_web_then_x() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("balanced".into());
        meta.note_attempt("tavily", 1, None, true);
        meta.note_attempt("xai", 9, None, true);
        let f = fields_from_meta(
            "/api/search",
            200,
            None,
            None,
            None,
            None,
            Some("hybrid".into()),
            &meta,
        );
        assert_eq!(f.service.as_deref(), Some("tavily"));
        assert_eq!(f.providers_consulted.as_deref(), Some("tavily,xai"));
        // sticky LAST success is the x leg.
        assert_eq!(f.key_id, Some(9));
        assert!(f.node_id.is_none());
    }

    #[test]
    fn error_before_vendor_service_none() {
        let meta = ExecMeta::default();
        assert!(service_from_meta(None, &meta).is_none());
    }
}
