//! Durable settings admin handlers.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::extract::database_problem;
use super::extract::AppJson;
use super::require_admin;
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsOut {
    social_enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsIn {
    #[serde(default)]
    pub social_enabled: Option<bool>,
}

pub async fn get_settings(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.get_social_enabled().await {
        Ok(social_enabled) => {
            let out = SettingsOut { social_enabled };
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}

pub async fn put_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppJson(body): AppJson<SettingsIn>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    if let Some(v) = body.social_enabled {
        if let Err(e) = ctx.db.set_social_enabled(v).await {
            return database_problem(e);
        }
    }
    match ctx.db.get_social_enabled().await {
        Ok(social_enabled) => {
            let out = SettingsOut { social_enabled };
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}
