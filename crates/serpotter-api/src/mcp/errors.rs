//! MCP tool error envelope: every tool failure returns ONE JSON text block
//! `{"kind","message","requestId","retryable"}` so clients can machine-read a
//! stable, non-`Display` error kind plus the human message, correlation id,
//! and machine-readable retryability.
//!
//! `kind` reuses the stable tags the REST + request_log paths already expose
//! (via `search_err_log`/`extract_err_log`/`research_err_log`), or
//! `ValidationError` for local parameter failures. `retryable` is derived from
//! `kind_retryable` — false for the faults a retry cannot fix
//! (`ValidationError`, `DatabaseError`, `NotReady`); every 5xx/timeout kind
//! (incl. `Timeout`, `Cancelled`, `InternalError`, `KeyBusy`) is transient.

use crate::product::errors::kind_retryable;
use rmcp::model::{CallToolResult, ContentBlock};

/// Fallback envelope when even serializing the error value itself fails.
/// `retryable` is hardcoded `true` because `kind_retryable("InternalError")` is
/// `true` (it excludes only `ValidationError`/`DatabaseError`/`NotReady`).
/// Unreachable in practice: the envelope is four scalars, which serde_json
/// cannot fail to serialize.
const FALLBACK_TOOL_ERROR: &str = r#"{"kind":"InternalError","message":"failed to serialize tool error","requestId":null,"retryable":true}"#;

/// Tool failure: the error envelope `{kind,message,requestId,retryable}` is
/// delivered as the single JSON text block in `content`, with
/// `structuredContent` intentionally left empty.
///
/// The three result tools advertise `outputSchema` (the typed success
/// response), and the 2026-07-28 spec requires structured results to conform to
/// it — an envelope in `structuredContent` would fail client-side schema
/// validation (the TS SDK rejects it as a protocol violation) and hide the
/// actionable message. The spec's own Tool Execution Error result carries
/// `content` only. The text block is byte-identical to the one the previous
/// structured variant produced, so existing envelope parsers keep working.
pub fn tool_error_structured(
    kind: &str,
    message: String,
    request_id: Option<String>,
) -> CallToolResult {
    let retryable = kind_retryable(kind);
    let value = serde_json::json!({
        "kind": kind,
        "message": message,
        "requestId": request_id,
        "retryable": retryable,
    });
    let body = serde_json::to_string(&value).unwrap_or_else(|_| FALLBACK_TOOL_ERROR.to_string());
    CallToolResult::error(vec![ContentBlock::text(body)])
}
