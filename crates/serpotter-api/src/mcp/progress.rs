use super::errors::tool_error;
use rmcp::model::{CallToolResult, ContentBlock, ProgressNotificationParam, RequestMetaObject};
use rmcp::service::{Peer, RoleServer};

/// Best-effort MCP progress. Missing token or notify errors never fail the tool.
pub(crate) async fn soft_progress(
    peer: &Peer<RoleServer>,
    meta: &RequestMetaObject,
    progress: f64,
    total: f64,
    message: &str,
) {
    let Some(token) = meta.get_progress_token() else {
        return;
    };
    let _ = peer
        .notify_progress(
            ProgressNotificationParam::new(token, progress)
                .with_total(total)
                .with_message(message.to_string()),
        )
        .await;
}

/// Serialize a tool result as a single pretty JSON text block. The only error
/// path (serde serialization failure) goes through the same structured
/// [`tool_error`] envelope as every other tool failure, so clients never see a
/// bare, kind-less error text.
pub(crate) fn text_ok<T: serde::Serialize>(
    value: T,
    request_id: Option<String>,
) -> Result<CallToolResult, rmcp::ErrorData> {
    match serde_json::to_string_pretty(&value) {
        Ok(s) => Ok(CallToolResult::success(vec![ContentBlock::text(s)])),
        Err(e) => Ok(tool_error(
            "InternalError",
            format!("serialize failed: {e}"),
            request_id,
        )),
    }
}
