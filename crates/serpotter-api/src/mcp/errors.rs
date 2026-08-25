//! MCP tool error envelope: every tool failure returns ONE JSON text block
//! `{"kind","message","requestId","retryable"}` so clients can machine-read a
//! stable, non-`Display` error kind plus the human message, correlation id,
//! and machine-readable retryability.
//!
//! `kind` reuses the stable tags the REST + request_log paths already expose
//! (via `search_err_log`/`extract_err_log`/`research_err_log`), or
//! `ValidationError` for local parameter failures. `retryable` is derived from
//! `kind_retryable` — false ONLY for `ValidationError`; every 5xx/timeout kind
//! (incl. `Timeout`, `Cancelled`, `InternalError`) is treated as transient.

use crate::product::errors::kind_retryable;
use rmcp::model::{CallToolResult, ContentBlock};

/// Fallback envelope when even serializing the error value itself fails.
/// `retryable` is hardcoded `true` (InternalError is not ValidationError).
const FALLBACK_TOOL_ERROR: &str = r#"{"kind":"InternalError","message":"failed to serialize tool error","requestId":null,"retryable":true}"#;

/// Structured failure: the error envelope `{kind,message,requestId,retryable}`
/// lands in `structuredContent` as well as the human text block, so clients
/// can read failures without string extraction.
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
    let mut result = CallToolResult::structured_error(value);
    // structured_error already builds a text block from the value; replace it
    // with the exact same serialization we used for the text body for parity.
    result.content = vec![ContentBlock::text(body)];
    result
}
