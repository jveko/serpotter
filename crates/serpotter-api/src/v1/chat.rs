//! OpenAI-compatible chat completions + models (B4).
//!
//! Model → strategy mapping:
//! - `serpotter-search` / `search` → product [`serpotter_product::search_inner`]
//!   with the **last user message** as the query (system messages stripped);
//! - `serpotter-research` / `research` → product [`serpotter_product::research_inner`]
//!   (standard, not deep);
//! - any `grok-*` model → direct xAI completion via [`XaiClient::complete`]
//!   (the B19 helper; xAI always dials direct). `grok-4.5` (or the env
//!   `XAI_MODEL`) is the advertised alias of the configured model;
//! - anything else → 404 `UnknownModel` problem listing the valid set.
//!
//! One-shot (`stream=false`) answers a JSON `chat.completion`; streaming
//! (`stream=true`) answers `text/event-stream` with a role delta, content
//! deltas, a finish chunk and `data: [DONE]`, capturing first-token latency
//! (`ttft_ms`) for the request_log row. The product executes one-shot
//! (providers return full results), so the stream re-chunks the final text —
//! first-token time is the first emitted `data:` frame, logged honestly.
//! On product failure the stream emits a single `data: {"error": …}` frame
//! followed by `[DONE]`, with the same status/kind the REST problem mapping
//! produces.
//!
//! request_log: path `/v1/chat/completions`, `request_mode` = `stream` |
//! `oneshot`, `strategy` = the routed model-strategy label
//! (`search`/`research`/`direct`), `ttft_ms` set for streams.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use serpotter_auth::problem_response;
use serpotter_core::{SearchItem, SearchQuery};
use serpotter_keypool::KeyPoolError;
use serpotter_product::{ExecMeta, ResearchRequest};
use serpotter_providers::ProviderError;

use crate::log_request::{self, fields_from_meta, request_id_from_headers, ApiTokenLogged};
use crate::product::errors::{research_problem, search_problem};
use crate::product::{deadline_detail, run_with_deadline, AppJson, DeadlineOutcome};
use crate::AppState;

/// Model-route label stored in request_log `strategy` (the design contract:
/// strategy = the routed strategy `search`/`research`/`direct`, distinct from
/// the underlying routing strategy the product `ExecMeta` carries).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ModelRoute {
    Search,
    Research,
    Direct,
}

impl ModelRoute {
    fn log_label(self) -> &'static str {
        match self {
            ModelRoute::Search => "search",
            ModelRoute::Research => "research",
            ModelRoute::Direct => "direct",
        }
    }
}

/// The advertised xAI model id: `XAI_MODEL` (matching `XaiClient::new`) or
/// `grok-4.5` — the alias a `/v1/models` client sees for the configured model.
fn xai_model_alias() -> String {
    std::env::var("XAI_MODEL").unwrap_or_else(|_| "grok-4.5".into())
}

/// Map an OpenAI model id to the serpotter route. Unknown models answer
/// 404 `UnknownModel` with the valid set in `detail`.
fn route_model(model: &str) -> Result<ModelRoute, String> {
    match model {
        "serpotter-search" | "search" => Ok(ModelRoute::Search),
        "serpotter-research" | "research" => Ok(ModelRoute::Research),
        m if m.starts_with("grok-") => Ok(ModelRoute::Direct),
        _ => Err(format!(
            "unknown model {model:?}; valid models: serpotter-search, serpotter-research, {}",
            xai_model_alias()
        )),
    }
}

/// One OpenAI message. `content` is a string or an array of content parts
/// (OpenAI shape); non-text parts are skipped — images/audio are unsupported.
#[derive(Debug, Deserialize)]
struct ChatMessage {
    role: String,
    content: serde_json::Value,
}

impl ChatMessage {
    fn text(&self) -> String {
        match &self.content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }
}

/// Strip system messages; the query is the LAST user message (OpenAI
/// convention: the final user turn carries the request). Empty user turns are
/// treated as absent (`None`) — a missing query is a 400, never an empty
/// provider call.
fn last_user_message(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(ChatMessage::text)
        .filter(|t| !t.trim().is_empty())
}

/// Concatenated system-message content. The search/research surfaces have no
/// system role; the direct xAI path feeds it as the completion system prompt.
fn system_prompt(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter(|m| m.role == "system")
        .map(ChatMessage::text)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// OpenAI chat-completion request body (B4, additive-only subset).
#[derive(Debug, Deserialize)]
pub struct ChatCompletionsRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    max_tokens: Option<u32>,
}

#[tracing::instrument(skip_all, name = "v1_chat_completions")]
pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
    AppJson(body): AppJson<ChatCompletionsRequest>,
) -> Response {
    let started = Instant::now();
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name.clone());
    let stream = body.stream;
    let max_tokens = body.max_tokens;
    let system = system_prompt(&body.messages);

    let Some(query) = last_user_message(&body.messages) else {
        let mut fields = fields_from_meta(
            "/v1/chat/completions",
            400,
            Some("ValidationError"),
            None,
            request_id,
            token_name,
            None,
            &ExecMeta::default(),
        );
        fields.request_mode = Some(if stream { "stream" } else { "oneshot" });
        log_request::spawn_log(&state, fields, started);
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "missing user message: /v1 requires at least one non-empty user turn",
        );
    };
    let preview = log_request::query_preview(query.trim());
    let model = body.model;

    let route = match route_model(&model) {
        Ok(r) => r,
        Err(detail) => {
            let mut fields = fields_from_meta(
                "/v1/chat/completions",
                404,
                Some("UnknownModel"),
                Some(preview),
                request_id,
                token_name,
                None,
                &ExecMeta::default(),
            );
            fields.request_mode = Some(if stream { "stream" } else { "oneshot" });
            log_request::spawn_log(&state, fields, started);
            return problem_response(StatusCode::NOT_FOUND, "UnknownModel", detail);
        }
    };

    let created = unix_now();
    let id = format!(
        "chatcmpl-{}",
        request_id.clone().unwrap_or_else(mint_suffix)
    );
    let ctx = state.product_ctx();

    match route {
        ModelRoute::Search => {
            let q = SearchQuery {
                query,
                ..Default::default()
            };
            match run_with_deadline(
                ctx.request_timeout,
                serpotter_product::search_inner(&ctx, q),
            )
            .await
            {
                DeadlineOutcome::Completed(Ok(o)) => {
                    let resp = o.result;
                    let meta = o.meta;
                    let content = resp
                        .answer
                        .clone()
                        .unwrap_or_else(|| render_search_items(&resp.items));
                    let usage = usage_from_meta(&meta);
                    success_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        &content,
                        usage,
                        Some(preview),
                        &request_id,
                        &token_name,
                        &meta,
                        route,
                        Some(resp.provider_used.clone()),
                    )
                }
                DeadlineOutcome::Completed(Err(o)) => {
                    let meta = o.meta;
                    let (code, log_status, kind, detail) = search_problem(o.result);
                    error_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        code,
                        kind,
                        &detail,
                        log_status,
                        Some(preview),
                        &request_id,
                        &token_name,
                        &meta,
                        route,
                    )
                }
                DeadlineOutcome::Elapsed => timeout_response(
                    &state,
                    started,
                    stream,
                    &id,
                    &model,
                    created,
                    ctx.request_timeout,
                    Some(preview),
                    &request_id,
                    &token_name,
                    route,
                ),
            }
        }
        ModelRoute::Research => {
            let req = ResearchRequest {
                query,
                ..Default::default()
            };
            match run_with_deadline(
                ctx.request_timeout,
                serpotter_product::research_inner(&ctx, req),
            )
            .await
            {
                DeadlineOutcome::Completed(Ok(o)) => {
                    let resp = o.result;
                    let meta = o.meta;
                    let content = research_content(&resp);
                    let usage = usage_from_meta(&meta);
                    let provider_used = crate::log_request::research_dial_label(&meta);
                    success_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        &content,
                        usage,
                        Some(preview),
                        &request_id,
                        &token_name,
                        &meta,
                        route,
                        provider_used,
                    )
                }
                DeadlineOutcome::Completed(Err(o)) => {
                    let meta = o.meta;
                    let (code, log_status, kind, detail) = research_problem(o.result);
                    error_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        code,
                        kind,
                        &detail,
                        log_status,
                        Some(preview),
                        &request_id,
                        &token_name,
                        &meta,
                        route,
                    )
                }
                DeadlineOutcome::Elapsed => timeout_response(
                    &state,
                    started,
                    stream,
                    &id,
                    &model,
                    created,
                    ctx.request_timeout,
                    Some(preview),
                    &request_id,
                    &token_name,
                    route,
                ),
            }
        }
        ModelRoute::Direct => {
            // xAI always dials direct; acquire an xai key, complete, report.
            // The acquire itself is bounded by the request deadline so a
            // contended pool can't outlive REQUEST_TIMEOUT_SECS (F10).
            let lease = tokio::select! {
                l = ctx.keys.acquire("xai") => match l {
                    Ok(l) => l,
                    Err(e) => {
                        let (status, kind) = key_pool_problem(&e);
                        let detail = e.to_string();
                        return error_response(
                            &state,
                            started,
                            stream,
                            &id,
                            &model,
                            created,
                            status,
                            kind,
                            &detail,
                            status.as_u16() as i64,
                            Some(preview),
                            &request_id,
                            &token_name,
                            &ExecMeta::default(),
                            route,
                        );
                    }
                },
                _ = tokio::time::sleep(ctx.request_timeout) => {
                    return timeout_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        ctx.request_timeout,
                        Some(preview),
                        &request_id,
                        &token_name,
                        route,
                    );
                }
            };
            // The advertised alias (`grok-4.5` or XAI_MODEL) resolves to the
            // env model; a different grok-* id overrides it explicitly.
            let model_override = (model != xai_model_alias()).then_some(model.as_str());
            let call = ctx.providers.xai.complete(
                &lease.key,
                &system,
                &query,
                model_override,
                max_tokens.unwrap_or(1200),
            );
            match run_with_deadline(ctx.request_timeout, call).await {
                DeadlineOutcome::Completed(Ok(text)) => {
                    let _ = ctx.keys.report_success(lease.id).await;
                    success_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        &text,
                        None,
                        Some(preview),
                        &request_id,
                        &token_name,
                        &ExecMeta::default(),
                        route,
                        Some("xai".into()),
                    )
                }
                DeadlineOutcome::Completed(Err(e)) => {
                    // A transient upstream 429 is an exhaustion signal (credits
                    // zeroed, exhausted-tier ordering); anything else is a
                    // generic failure (bumps consecutive_fails, mirroring the
                    // product key-hold semantics).
                    if matches!(&e, ProviderError::Upstream { status: 429, .. }) {
                        let _ = ctx.keys.report_exhausted(lease.id).await;
                    } else {
                        let _ = ctx.keys.report_failure(lease.id).await;
                    }
                    let detail = e.to_string();
                    error_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        StatusCode::BAD_GATEWAY,
                        "ProviderError",
                        &detail,
                        502,
                        Some(preview),
                        &request_id,
                        &token_name,
                        &ExecMeta::default(),
                        route,
                    )
                }
                DeadlineOutcome::Elapsed => {
                    let _ = ctx.keys.release(lease.id).await;
                    timeout_response(
                        &state,
                        started,
                        stream,
                        &id,
                        &model,
                        created,
                        ctx.request_timeout,
                        Some(preview),
                        &request_id,
                        &token_name,
                        route,
                    )
                }
            }
        }
    }
}

/// GET /v1/models — the valid model set (search/research surfaces + the xAI
/// alias). Same tok- auth as the rest of the token surface.
#[tracing::instrument(skip_all, name = "v1_models")]
pub async fn models(
    State(_state): State<AppState>,
    ApiTokenLogged(_token): ApiTokenLogged,
) -> Response {
    (
        StatusCode::OK,
        axum::Json(json!({
            "object": "list",
            "data": [
                { "id": "serpotter-search", "object": "model", "created": 0 },
                { "id": "serpotter-research", "object": "model", "created": 0 },
                { "id": xai_model_alias(), "object": "model", "created": 0 },
            ],
        })),
    )
        .into_response()
}

/// Key-pool failure → problem kind for the direct xAI path (mirrors the
/// product mapping: NoHealthyKey / KeyBusy / DatabaseError).
fn key_pool_problem(e: &KeyPoolError) -> (StatusCode, &'static str) {
    match e {
        KeyPoolError::NoHealthyKey(_) => (StatusCode::SERVICE_UNAVAILABLE, "NoHealthyKey"),
        KeyPoolError::AcquireTimeout(_) => (StatusCode::SERVICE_UNAVAILABLE, "KeyBusy"),
        KeyPoolError::Db(_) => (StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError"),
    }
}

/// Terminal response + request_log row for a successful /v1 completion.
#[allow(clippy::too_many_arguments)]
fn success_response(
    state: &AppState,
    started: Instant,
    stream: bool,
    id: &str,
    model: &str,
    created: i64,
    content: &str,
    usage: Option<serde_json::Value>,
    preview: Option<String>,
    request_id: &Option<String>,
    token_name: &Option<String>,
    meta: &ExecMeta,
    route: ModelRoute,
    provider_used: Option<String>,
) -> Response {
    let ttft = stream.then(|| started.elapsed().as_secs_f64() * 1000.0);
    let mut fields = fields_from_meta(
        "/v1/chat/completions",
        200,
        None,
        preview,
        request_id.clone(),
        token_name.clone(),
        provider_used,
        meta,
    );
    fields.strategy = Some(route.log_label().to_string());
    fields.request_mode = Some(if stream { "stream" } else { "oneshot" });
    fields.ttft_ms = ttft;
    log_request::spawn_log(state, fields, started);

    if stream {
        let events = stream_events(id, model, created, content);
        sse_response(&events)
    } else {
        (
            StatusCode::OK,
            axum::Json(one_shot_json(id, model, created, content, usage)),
        )
            .into_response()
    }
}

/// Error terminal + request_log row (REST problem for one-shot, SSE error
/// frame + `[DONE]` for streams; both log the mapping status/kind).
#[allow(clippy::too_many_arguments)]
fn error_response(
    state: &AppState,
    started: Instant,
    stream: bool,
    id: &str,
    model: &str,
    created: i64,
    status: StatusCode,
    kind: &'static str,
    detail: &str,
    log_status: i64,
    preview: Option<String>,
    request_id: &Option<String>,
    token_name: &Option<String>,
    meta: &ExecMeta,
    route: ModelRoute,
) -> Response {
    let ttft = stream.then(|| started.elapsed().as_secs_f64() * 1000.0);
    let mut fields = fields_from_meta(
        "/v1/chat/completions",
        log_status,
        Some(kind),
        preview,
        request_id.clone(),
        token_name.clone(),
        None,
        meta,
    );
    fields.strategy = Some(route.log_label().to_string());
    fields.request_mode = Some(if stream { "stream" } else { "oneshot" });
    fields.ttft_ms = ttft;
    log_request::spawn_log(state, fields, started);

    if stream {
        let events = stream_error_events(id, model, created, kind, detail, status.as_u16());
        sse_response(&events)
    } else {
        problem_response(status, kind, detail)
    }
}

/// F10 deadline exceeded: 504 `RequestTimeout` (one-shot) or an SSE error
/// frame with the same status (stream).
#[allow(clippy::too_many_arguments)]
fn timeout_response(
    state: &AppState,
    started: Instant,
    stream: bool,
    id: &str,
    model: &str,
    created: i64,
    timeout: std::time::Duration,
    preview: Option<String>,
    request_id: &Option<String>,
    token_name: &Option<String>,
    route: ModelRoute,
) -> Response {
    let detail = deadline_detail(timeout);
    error_response(
        state,
        started,
        stream,
        id,
        model,
        created,
        StatusCode::GATEWAY_TIMEOUT,
        "RequestTimeout",
        &detail,
        504,
        preview,
        request_id,
        token_name,
        &ExecMeta::default(),
        route,
    )
}

/// One-shot `chat.completion` JSON body. `usage` is included only when the
/// product reported token usage (honest: never a fabricated block).
fn one_shot_json(
    id: &str,
    model: &str,
    created: i64,
    content: &str,
    usage: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut v = json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop",
        }],
    });
    if let Some(u) = usage {
        v["usage"] = u;
    }
    v
}

/// SSE frames for a successful completion: role delta, content deltas, finish
/// chunk. The caller wraps them with `data:` framing and `[DONE]`.
fn stream_events(id: &str, model: &str, created: i64, content: &str) -> Vec<String> {
    let mut events = vec![json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}],
    })
    .to_string()];
    for chunk in content_chunks(content, 16) {
        events.push(
            json!({
                "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
                "choices": [{"index": 0, "delta": {"content": chunk}, "finish_reason": null}],
            })
            .to_string(),
        );
    }
    events.push(
        json!({
            "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        })
        .to_string(),
    );
    events
}

/// Split content into word-group deltas (deterministic). The product returns
/// full text at once, so this is the re-chunking the stream performs; empty
/// content yields no content deltas (role + finish only).
fn content_chunks(content: &str, words_per_chunk: usize) -> Vec<String> {
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .chunks(words_per_chunk)
        .map(|chunk| chunk.join(" "))
        .collect()
}

/// SSE error frame (design: on product error emit `data: {"error": …}` then
/// `[DONE]`); status/kind mirror the REST problem mapping.
fn stream_error_events(
    id: &str,
    model: &str,
    created: i64,
    kind: &str,
    detail: &str,
    status: u16,
) -> Vec<String> {
    vec![json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "error": { "message": detail, "type": kind, "status": status },
    })
    .to_string()]
}

/// SSE wire framing: `data: <payload>\n\n` per event, terminated by
/// `data: [DONE]`. The response is finite (the product is one-shot), so it
/// drains normally under the graceful-shutdown cap — it never holds the
/// process open beyond the final frame.
fn sse_response(events: &[String]) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        sse_text(events),
    )
        .into_response()
}

fn sse_text(events: &[String]) -> String {
    let mut text = String::new();
    for e in events {
        text.push_str("data: ");
        text.push_str(e);
        text.push_str("\n\n");
    }
    text.push_str("data: [DONE]\n\n");
    text
}

/// OpenAI usage block from the product `ExecMeta` (B2 token counters); `None`
/// when the product reported no usage (e.g. the direct xAI path — B19's
/// `complete` returns text only).
fn usage_from_meta(meta: &ExecMeta) -> Option<serde_json::Value> {
    let input = meta.input_tokens;
    let output = meta.output_tokens;
    if input.is_none() && output.is_none() && meta.total_tokens.is_none() {
        return None;
    }
    let input = input.unwrap_or(0);
    let output = output.unwrap_or(0);
    let total = meta
        .total_tokens
        .unwrap_or_else(|| input.saturating_add(output));
    Some(json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": total,
    }))
}

/// Search answer text: the provider `answer` when present, else a rendered
/// top-item list (title/url/snippet) — never a fabricated summary.
fn render_search_items(items: &[SearchItem]) -> String {
    let mut out = String::new();
    for (i, it) in items.iter().take(10).enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str(&it.title);
        out.push('\n');
        out.push_str(&it.url);
        if let Some(s) = &it.snippet {
            out.push('\n');
            out.push_str(s);
        }
    }
    out
}

/// Research answer text: the evidence summary (with its sources) when
/// present, else the rendered web results.
fn research_content(resp: &serpotter_product::ResearchResponse) -> String {
    match &resp.evidence {
        Some(e) => {
            let mut out = e.summary.clone().unwrap_or_default();
            let sources: Vec<String> = resp
                .citations
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|c| format!("- {} {}", c.title, c.url))
                .collect();
            if !sources.is_empty() {
                if !out.is_empty() {
                    out.push_str("\n\nSources:\n");
                    out.push_str(&sources.join("\n"));
                } else {
                    out.push_str(&sources.join("\n"));
                }
            }
            if out.is_empty() {
                render_search_items(&resp.web_results)
            } else {
                out
            }
        }
        None => render_search_items(&resp.web_results),
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Random 16-hex suffix for the `chatcmpl-` id (uuid-ish without a dep);
/// time-derived fallback when the RNG fails.
fn mint_suffix() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_ok() {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(16);
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        return out;
    }
    format!("{:016x}", unix_now() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_core::SearchResponse;

    fn msg(role: &str, content: serde_json::Value) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content,
        }
    }

    #[test]
    fn route_model_maps_all_advertised_models() {
        assert_eq!(route_model("serpotter-search"), Ok(ModelRoute::Search));
        assert_eq!(route_model("search"), Ok(ModelRoute::Search));
        assert_eq!(route_model("serpotter-research"), Ok(ModelRoute::Research));
        assert_eq!(route_model("research"), Ok(ModelRoute::Research));
        assert_eq!(route_model("grok-4.5"), Ok(ModelRoute::Direct));
        assert_eq!(route_model("grok-4-fast"), Ok(ModelRoute::Direct));
    }

    #[test]
    fn route_model_unknown_lists_valid_set() {
        let err = route_model("banana").expect_err("banana is not a model");
        assert!(err.contains("serpotter-search"), "{err}");
        assert!(err.contains("serpotter-research"), "{err}");
        assert!(err.contains("grok-4.5"), "{err}");
    }

    #[test]
    fn last_user_message_strips_system_and_uses_last_user_turn() {
        let msgs = vec![
            msg("system", json!("be helpful")),
            msg("user", json!("first")),
            msg("assistant", json!("ok")),
            msg("user", json!("second")),
        ];
        assert_eq!(last_user_message(&msgs).as_deref(), Some("second"));
        assert_eq!(system_prompt(&msgs), "be helpful");
    }

    #[test]
    fn last_user_message_handles_part_array_content() {
        let msgs = vec![msg(
            "user",
            json!([
                {"type": "text", "text": "hello"},
                {"type": "image_url", "image_url": {"url": "x"}},
            ]),
        )];
        assert_eq!(last_user_message(&msgs).as_deref(), Some("hello"));
    }

    #[test]
    fn last_user_message_none_without_user_turn() {
        let msgs = vec![msg("system", json!("s"))];
        assert!(last_user_message(&msgs).is_none());
        let empty = vec![msg("user", json!("  "))];
        assert!(
            last_user_message(&empty).is_none(),
            "blank user turn absent"
        );
    }

    #[test]
    fn one_shot_json_carries_message_and_usage() {
        let v = one_shot_json(
            "chatcmpl-1",
            "serpotter-search",
            0,
            "hello",
            Some(json!({"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3})),
        );
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "hello");
        assert_eq!(v["choices"][0]["message"]["role"], "assistant");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["usage"]["total_tokens"], 3);
        let v2 = one_shot_json("chatcmpl-2", "m", 0, "x", None);
        assert!(v2.get("usage").is_none(), "usage absent when unknown");
    }

    #[test]
    fn stream_events_role_then_content_then_finish() {
        let events = stream_events("chatcmpl-1", "serpotter-search", 0, "hello world foo");
        assert!(events.len() >= 3, "{events:?}");
        let first: serde_json::Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(first["object"], "chat.completion.chunk");
        assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
        assert!(first["choices"][0]["finish_reason"].is_null());
        let content_events: Vec<serde_json::Value> = events[1..events.len() - 1]
            .iter()
            .map(|e| serde_json::from_str(e).unwrap())
            .collect();
        assert!(!content_events.is_empty(), "content deltas present");
        let joined: String = content_events
            .iter()
            .filter_map(|e| e["choices"][0]["delta"]["content"].as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, "hello world foo");
        let last: serde_json::Value = serde_json::from_str(events.last().unwrap()).unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn content_chunks_splits_words_and_handles_empty() {
        assert_eq!(
            content_chunks("a b c d", 2),
            vec!["a b".to_string(), "c d".to_string()]
        );
        assert_eq!(content_chunks("single", 8), vec!["single".to_string()]);
        assert!(content_chunks("", 2).is_empty());
    }

    #[test]
    fn sse_text_frames_each_event_and_done() {
        assert_eq!(
            sse_text(&[r#"{"x":1}"#.into(), r#"{"y":2}"#.into()]),
            "data: {\"x\":1}\n\ndata: {\"y\":2}\n\ndata: [DONE]\n\n"
        );
    }

    #[test]
    fn stream_error_events_carry_kind_and_status() {
        let events = stream_error_events("chatcmpl-1", "m", 0, "SearchError", "boom", 502);
        let v: serde_json::Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(v["error"]["type"], "SearchError");
        assert_eq!(v["error"]["status"], 502);
        assert_eq!(v["error"]["message"], "boom");
    }

    #[test]
    fn usage_from_meta_only_when_available() {
        assert!(usage_from_meta(&ExecMeta::default()).is_none());
        let mut meta = ExecMeta::default();
        meta.set_usage(Some(10), Some(20), Some(30), None);
        let u = usage_from_meta(&meta).unwrap();
        assert_eq!(u["prompt_tokens"], 10);
        assert_eq!(u["completion_tokens"], 20);
        assert_eq!(u["total_tokens"], 30);
        // total derived from input+output when unset
        let mut meta = ExecMeta::default();
        meta.set_usage(Some(5), Some(7), None, None);
        let u = usage_from_meta(&meta).unwrap();
        assert_eq!(u["total_tokens"], 12);
    }

    #[test]
    fn render_search_items_and_research_content() {
        let resp = SearchResponse {
            query: "q".into(),
            provider_used: "tavily".into(),
            answer: Some("the answer".into()),
            items: vec![],
            leg_errors: None,
            route_debug: None,
            cache_hit: None,
        };
        let out = render_search_items(&resp.items);
        assert!(out.is_empty());
        // research with evidence summary + citations
        let r = serpotter_product::ResearchResponse {
            query: "q".into(),
            web_results: vec![],
            social_results: None,
            social_error: None,
            scraped_pages: None,
            citations: Some(vec![serpotter_product::Citation {
                title: "src".into(),
                url: "https://example.com".into(),
            }]),
            evidence: Some(serpotter_product::Evidence {
                summary: Some("summary".into()),
                providers_consulted: None,
                web_leg_errors: None,
            }),
        };
        let content = research_content(&r);
        assert!(content.contains("summary"), "{content}");
        assert!(content.contains("https://example.com"), "{content}");
    }
}
