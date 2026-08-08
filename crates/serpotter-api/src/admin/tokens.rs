//! API tokens admin handlers.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::{generate_token, problem_response};

use super::{mask_token, require_admin};
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenOut {
    id: i64,
    name: String,
    /// Full token only on create; list masks middle.
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_preview: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTokenBody {
    #[serde(default)]
    pub name: String,
}

pub async fn list_tokens(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.list_tokens().await {
        Ok(rows) => {
            let out: Vec<TokenOut> = rows
                .into_iter()
                .map(|r| TokenOut {
                    id: r.id,
                    name: r.name,
                    token: None,
                    token_preview: Some(mask_token(&r.token)),
                    created_at: r.created_at,
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

pub async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateTokenBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let token = match generate_token() {
        Ok(t) => t,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "TokenError",
                e.to_string(),
            );
        }
    };
    match ctx.db.insert_token(&token, &body.name).await {
        Ok(row) => {
            let out = TokenOut {
                id: row.id,
                name: row.name,
                token: Some(token),
                token_preview: None,
                created_at: row.created_at,
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

pub async fn delete_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.delete_token_by_id(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "token not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
