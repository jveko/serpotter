//! API keys admin handlers + credit sync.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;

use super::{mask_key, require_admin};
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyOut {
    id: i64,
    service: String,
    key_preview: String,
    active: bool,
    consecutive_fails: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateKeyBody {
    pub service: String,
    pub key: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncCreditsBody {
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncKeyResult {
    id: i64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncCreditsOut {
    service: String,
    synced: i64,
    errors: i64,
    results: Vec<SyncKeyResult>,
}

pub async fn list_keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }
    match state.db.list_api_keys().await {
        Ok(rows) => {
            let out: Vec<KeyOut> = rows
                .into_iter()
                .map(|r| KeyOut {
                    id: r.id,
                    service: r.service,
                    key_preview: mask_key(&r.key),
                    active: r.active != 0,
                    consecutive_fails: r.consecutive_fails,
                })
                .collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateKeyBody>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }
    if body.service.trim().is_empty() || body.key.trim().is_empty() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "service and key required",
        );
    }
    match state
        .db
        .insert_api_key(body.service.trim(), body.key.trim())
        .await
    {
        Ok(row) => {
            let out = KeyOut {
                id: row.id,
                service: row.service,
                key_preview: mask_key(&row.key),
                active: row.active != 0,
                consecutive_fails: row.consecutive_fails,
            };
            (StatusCode::CREATED, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }
    match state.db.delete_api_key(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn toggle_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }
    match state.db.get_api_key(id).await {
        Ok(Some(row)) => {
            let next = row.active == 0;
            match state.db.set_api_key_active(id, next).await {
                Ok(true) => {
                    let out = KeyOut {
                        id: row.id,
                        service: row.service,
                        key_preview: mask_key(&row.key),
                        active: next,
                        consecutive_fails: if next { 0 } else { row.consecutive_fails },
                    };
                    (StatusCode::OK, Json(out)).into_response()
                }
                Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
                Err(e) => problem_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DatabaseError",
                    e.to_string(),
                ),
            }
        }
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// Soft-fail credit sync for tavily and/or firecrawl. Never sets active=0 on fetch fail.
pub async fn sync_credits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SyncCreditsBody>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }

    let services: Vec<&str> = match body.service.as_deref() {
        Some("tavily") => vec!["tavily"],
        Some("firecrawl") => vec!["firecrawl"],
        Some(other) => {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                format!("unsupported service {other}"),
            );
        }
        None => vec!["tavily", "firecrawl"],
    };

    let ctx = state.admin_ctx();
    match crate::credit_sync::sync_credits_for_services(&ctx.db, &ctx.providers, &services).await {
        Ok(report) => (
            StatusCode::OK,
            Json(SyncCreditsOut {
                service: report.service,
                synced: report.synced,
                errors: report.errors,
                results: report
                    .results
                    .into_iter()
                    .map(|r| SyncKeyResult {
                        id: r.id,
                        ok: r.ok,
                        remaining: r.remaining,
                        limit: r.limit,
                    })
                    .collect(),
            }),
        )
            .into_response(),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
