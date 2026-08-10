//! MCP tool error envelope: every tool failure returns ONE JSON text block
//! `{"kind","message","requestId"}` so clients can machine-read a stable,
//! non-`Display` error kind plus the human message and correlation id.
//!
//! `kind` reuses the stable tags the REST + request_log paths already expose
//! (via `search_err_log`/`extract_err_log`/`research_err_log`), or
//! `ValidationError` for local parameter failures.

use rmcp::model::{CallToolResult, ContentBlock};

/// Fallback envelope when even serializing the error value itself fails.
const FALLBACK_TOOL_ERROR: &str =
    r#"{"kind":"InternalError","message":"failed to serialize tool error","requestId":null}"#;

/// Structured failure: the error envelope `{kind,message,requestId}` lands in
/// `structuredContent` as well as the human text block, so clients can read
/// failures without string extraction.
pub fn tool_error_structured(
    kind: &str,
    message: String,
    request_id: Option<String>,
) -> CallToolResult {
    let value = serde_json::json!({
        "kind": kind,
        "message": message,
        "requestId": request_id,
    });
    let body = serde_json::to_string(&value).unwrap_or_else(|_| FALLBACK_TOOL_ERROR.to_string());
    let mut result = CallToolResult::structured_error(value);
    // structured_error already builds a text block from the value; replace it
    // with the exact same serialization we used for the text body for parity.
    result.content = vec![ContentBlock::text(body)];
    result
}
