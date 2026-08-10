use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken, RequestMetaObject,
};
use rmcp::service::{Peer, RoleServer};
use serpotter_product::{ProgressEvent, ProgressSink};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::Notify;

use super::errors::tool_error;

/// MCP progress sink: emits each product event as a `notifications/progress`
/// frame. Opt-in — without a client `_meta.progressToken` it no-ops, so the
/// request keeps the plain-JSON fast path.
///
/// Delivery is asynchronous: rmcp's [`Peer::notify_progress`] is `async fn`
/// (it round-trips through the peer's outbound queue), while the product
/// `ProgressSink` contract is synchronous. `emit` therefore enqueues each
/// frame into an unbounded FIFO channel; a per-request delivery task forwards
/// frames as they arrive (live streaming, FIFO order). The handler calls
/// [`flush`](Self::flush) after the product future completes but before
/// returning the terminal result, so every frame reaches the transport first —
/// that ordering is what makes rmcp's stateless response builder pick
/// `text/event-stream` instead of plain JSON.
pub(crate) struct McpProgressSink {
    token: Option<ProgressToken>,
    n: AtomicU64,
    /// FIFO queue to the per-request delivery task; `flush` takes and closes it.
    tx: Mutex<Option<UnboundedSender<ProgressNotificationParam>>>,
    /// Notified once the delivery task has drained the queue after flush.
    done: Arc<Notify>,
    /// Whether a delivery task is running (only when a progress token exists).
    active: bool,
}

impl McpProgressSink {
    pub fn new(peer: Peer<RoleServer>, meta: &RequestMetaObject) -> Self {
        let token = meta.get_progress_token();
        let (tx, mut rx) = unbounded_channel();
        let done = Arc::new(Notify::new());
        // No token → no frames can be emitted; skip the delivery task entirely
        // so the plain-JSON fast path pays nothing beyond the no-op sink.
        let active = token.is_some();
        if active {
            let done_task = done.clone();
            tokio::spawn(async move {
                while let Some(param) = rx.recv().await {
                    let _ = peer.notify_progress(param).await;
                }
                done_task.notify_one();
            });
        }
        Self {
            token,
            n: AtomicU64::new(0),
            tx: Mutex::new(Some(tx)),
            done,
            active,
        }
    }

    /// Deliver any queued progress frames before the terminal result, so rmcp's
    /// stateless response builder sees a notification first and switches to SSE.
    pub(crate) async fn flush(&self) {
        let tx = self.tx.lock().expect("progress tx lock").take();
        drop(tx); // closing the channel lets the delivery task drain and exit
        if self.active {
            self.done.notified().await;
        }
    }
}

impl ProgressSink for McpProgressSink {
    fn emit(&self, event: &ProgressEvent) {
        let Some(token) = &self.token else {
            return;
        };
        let message = event.message();
        let n = self.n.fetch_add(1, Ordering::Relaxed);
        let param = ProgressNotificationParam::new(token.clone(), n as f64).with_message(message);
        // Best-effort: a closed queue (already flushed) simply drops the frame.
        let _ = self
            .tx
            .lock()
            .expect("progress tx lock")
            .as_ref()
            .and_then(|tx| tx.send(param).ok());
    }
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
