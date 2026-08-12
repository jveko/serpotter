//! OpenAI-compatible `/v1` surface (B4): chat completions + models list.
//!
//! Route handlers live in [`chat`]; `lib.rs` wires:
//! - `POST /v1/chat/completions` → [`chat::chat_completions`]
//! - `GET /v1/models` → [`chat::models`]
//!   (see the cross-cluster notes — `lib.rs` is wired by Main.)

pub mod chat;

pub use chat::{chat_completions, models};
