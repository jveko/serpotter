//! Durable settings admin handlers.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;

use super::require_admin;
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsOut {
    social_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsIn {
    #[serde(default)]
    pub social_enabled: Option<bool>,
}

pub async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }
    match state.db.get_social_enabled().await {
        Ok(social_enabled) => {
            let out = SettingsOut {
                social_enabled,
                note: None,
            };
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn put_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SettingsIn>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }
    if let Some(v) = body.social_enabled {
        if let Err(e) = state.db.set_social_enabled(v).await {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    }
    match state.db.get_social_enabled().await {
        Ok(social_enabled) => {
            let out = SettingsOut {
                social_enabled,
                note: None,
            };
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
