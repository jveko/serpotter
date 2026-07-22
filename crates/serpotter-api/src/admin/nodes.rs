//! Proxy nodes admin handlers.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;

use super::require_admin;
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeOut {
    id: i64,
    host: String,
    port: i64,
    enabled: bool,
    inflight: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNodeBody {
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
}

pub async fn list_nodes(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.list_nodes().await {
        Ok(rows) => {
            let out: Vec<NodeOut> = rows
                .into_iter()
                .map(|r| NodeOut {
                    id: r.id,
                    host: r.host,
                    port: r.port,
                    enabled: r.enabled != 0,
                    inflight: r.inflight,
                    username: r.username,
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

pub async fn create_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateNodeBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    if body.host.trim().is_empty() || body.port <= 0 {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "host and positive port required",
        );
    }
    match ctx
        .db
        .insert_node(
            body.host.trim(),
            body.port,
            body.username.as_deref(),
            body.password.as_deref(),
        )
        .await
    {
        Ok(row) => {
            let out = NodeOut {
                id: row.id,
                host: row.host,
                port: row.port,
                enabled: row.enabled != 0,
                inflight: row.inflight,
                username: row.username,
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

pub async fn delete_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.delete_node(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
