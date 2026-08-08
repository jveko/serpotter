//! MCP tool error envelope: every tool failure returns ONE JSON text block
//! `{"kind","message","requestId"}` so clients can machine-read a stable,
//! non-`Display` error kind plus the human message and correlation id.
//!
//! `kind` reuses the stable tags the REST + request_log paths already expose
//! (via `search_err_log`/`extract_err_log`/`research_err_log`), or
//! `ValidationError` for local parameter failures.

use rmcp::model::{CallToolResult, ContentBlock};
use serde::Serialize;

/// Wire shape of a single MCP tool error.
#[derive(Debug, Serialize)]
struct ToolErrorEnvelope<'a> {
    kind: &'a str,
    message: &'a str,
    #[serde(rename = "requestId")]
    request_id: Option<&'a str>,
}

/// Build the error `CallToolResult` for a tool failure. Serializing the three
/// string fields cannot fail in practice; the fallback keeps the error visible
/// rather than losing it.
pub fn tool_error(kind: &str, message: String, request_id: Option<String>) -> CallToolResult {
    let body = serde_json::to_string(&ToolErrorEnvelope {
        kind,
        message: &message,
        request_id: request_id.as_deref(),
    })
    .unwrap_or_else(|_| {
        r#"{"kind":"InternalError","message":"failed to serialize tool error","requestId":null}"#
            .to_string()
    });
    CallToolResult::error(vec![ContentBlock::text(body)])
}
