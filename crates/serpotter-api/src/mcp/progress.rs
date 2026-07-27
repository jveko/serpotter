use rmcp::model::{CallToolResult, ContentBlock, Meta, ProgressNotificationParam};
use rmcp::service::{Peer, RoleServer};

/// Best-effort MCP progress. Missing token or notify errors never fail the tool.
pub(crate) async fn soft_progress(
    peer: &Peer<RoleServer>,
    meta: &Meta,
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

pub(crate) fn text_ok<T: serde::Serialize>(value: T) -> Result<CallToolResult, rmcp::ErrorData> {
    match serde_json::to_string_pretty(&value) {
        Ok(s) => Ok(CallToolResult::success(vec![ContentBlock::text(s)])),
        Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "serialize failed: {e}"
        ))])),
    }
}
