//! API keys admin handlers + credit sync.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;
use serpotter_providers::PROVIDER_SERVICES;

use super::extract::database_problem;
use super::extract::{bounded_field, AppJson, AppPath};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    credits_remaining: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credits_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_synced_at: Option<String>,
    inflight: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_used_at: Option<String>,
    /// `'vendor_suspended'` / `'auth_fail'` / `'manual'` / absent. Lets an
    /// operator tell a dead vendor account apart from a key they switched off
    /// themselves — only the latter is expected to come back on its own.
    /// `'auth_fail'` is the fail@3 auth hard-disable, stamped in the same
    /// UPDATE that clears `active`; unlike `vendor_suspended` the re-enable
    /// cron DOES revive those rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    disabled_reason: Option<String>,
}

fn key_out_from_admin(r: serpotter_db::ApiKeyAdminRow) -> KeyOut {
    KeyOut {
        id: r.id,
        service: r.service,
        key_preview: mask_key(&r.key),
        active: r.active != 0,
        consecutive_fails: r.consecutive_fails,
        credits_remaining: r.credits_remaining,
        credits_limit: r.credits_limit,
        usage_synced_at: r.usage_synced_at,
        inflight: r.inflight,
        lease_until: r.lease_until,
        last_used_at: r.last_used_at,
        disabled_reason: r.disabled_reason,
    }
}

fn key_out_from_insert(r: serpotter_db::ApiKeyRow) -> KeyOut {
    KeyOut {
        id: r.id,
        service: r.service,
        key_preview: mask_key(&r.key),
        active: r.active != 0,
        consecutive_fails: r.consecutive_fails,
        credits_remaining: None,
        credits_limit: None,
        usage_synced_at: None,
        inflight: 0,
        lease_until: None,
        last_used_at: None,
        disabled_reason: None,
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncCreditsOut {
    service: String,
    synced: i64,
    errors: i64,
    /// Active keys left for a later pass because the per-service vendor usage
    /// cap was reached. Non-zero means the sync is partial, not complete.
    skipped: i64,
    results: Vec<SyncKeyResult>,
}

pub async fn list_keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.list_api_keys().await {
        Ok(rows) => {
            let out: Vec<KeyOut> = rows.into_iter().map(key_out_from_admin).collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}

pub async fn create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppJson(body): AppJson<CreateKeyBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let service = match bounded_field("service", &body.service) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                "service and key required",
            );
        }
        Err(detail) => {
            return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
        }
    };
    let key = match bounded_field("key", &body.key) {
        Ok(k) if !k.is_empty() => k,
        Ok(_) => {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                "service and key required",
            );
        }
        Err(detail) => {
            return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
        }
    };
    if !PROVIDER_SERVICES.contains(&service) {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            format!("unsupported service {service}"),
        );
    }
    match ctx.db.insert_api_key(service, key).await {
        Ok(row) => {
            let out = key_out_from_insert(row);
            (StatusCode::CREATED, Json(out)).into_response()
        }
        Err(e) if serpotter_db::Db::is_unique_violation(&e) => problem_response(
            StatusCode::CONFLICT,
            "DuplicateKey",
            format!("key already exists for service {service}"),
        ),
        Err(e) => database_problem(e),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateKeyBody {
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

/// Rotate an api key or change its service without delete+recreate.
/// `{service?, key?}` — at least one field required. Key rotation resets
/// `consecutiveFails`; a service change clears the stored credit snapshot
/// (old account's numbers must be re-synced before they mean anything).
pub async fn update_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppPath(id): AppPath<i64>,
    AppJson(body): AppJson<UpdateKeyBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    if body.service.is_none() && body.key.is_none() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "at least one of service or key required",
        );
    }
    let service = match body.service.as_deref() {
        None => None,
        Some(raw) => match bounded_field("service", raw) {
            Ok(s) if !s.is_empty() => Some(s),
            Ok(_) => {
                return problem_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationError",
                    "service must not be blank",
                );
            }
            Err(detail) => {
                return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
            }
        },
    };
    let key = match body.key.as_deref() {
        None => None,
        Some(raw) => match bounded_field("key", raw) {
            Ok(k) if !k.is_empty() => Some(k),
            Ok(_) => {
                return problem_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationError",
                    "key must not be blank",
                );
            }
            Err(detail) => {
                return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
            }
        },
    };
    if let Some(svc) = service {
        if !PROVIDER_SERVICES.contains(&svc) {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                format!("unsupported service {svc}"),
            );
        }
    }
    match ctx.db.update_api_key(id, service, key).await {
        Ok(true) => {}
        Ok(false) => {
            return problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found");
        }
        Err(e) if serpotter_db::Db::is_unique_violation(&e) => {
            return problem_response(
                StatusCode::CONFLICT,
                "DuplicateKey",
                "key already exists — rotating to a secret used by another row is not allowed",
            );
        }
        Err(e) => {
            return database_problem(e);
        }
    };
    match ctx.db.get_api_key_admin(id).await {
        Ok(Some(updated)) => (StatusCode::OK, Json(key_out_from_admin(updated))).into_response(),
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
        Err(e) => database_problem(e),
    }
}

pub async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppPath(id): AppPath<i64>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.delete_api_key(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
        Err(e) => database_problem(e),
    }
}

pub async fn toggle_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppPath(id): AppPath<i64>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.get_api_key_admin(id).await {
        Ok(Some(row)) => {
            let next = row.active == 0;
            match ctx.db.set_api_key_active(id, next).await {
                Ok(true) => match ctx.db.get_api_key_admin(id).await {
                    Ok(Some(updated)) => {
                        (StatusCode::OK, Json(key_out_from_admin(updated))).into_response()
                    }
                    Ok(None) => {
                        problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found")
                    }
                    Err(e) => database_problem(e),
                },
                Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
                Err(e) => database_problem(e),
            }
        }
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
        Err(e) => database_problem(e),
    }
}

/// Soft-fail credit sync. Tavily/Firecrawl fetch real usage; exa/xai soft-error only.
/// Never sets active=0 on fetch fail.
pub async fn sync_credits(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppJson(body): AppJson<SyncCreditsBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }

    let services: Vec<&str> = match body.service.as_deref() {
        Some("tavily") => vec!["tavily"],
        Some("firecrawl") => vec!["firecrawl"],
        Some("exa") => vec!["exa"],
        Some("xai") => vec!["xai"],
        Some(other) => {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                format!("unsupported service {other}"),
            );
        }
        // Default "all" stays vendors with real usage APIs only.
        None => vec!["tavily", "firecrawl"],
    };

    match crate::credit_sync::sync_credits_for_services(
        &ctx.db,
        &ctx.providers,
        &services,
        crate::credit_sync::MAX_KEYS_PER_SERVICE,
    )
    .await
    {
        Ok(report) => (
            StatusCode::OK,
            Json(SyncCreditsOut {
                service: report.service,
                synced: report.synced,
                errors: report.errors,
                skipped: report.skipped,
                results: report
                    .results
                    .into_iter()
                    .map(|r| SyncKeyResult {
                        id: r.id,
                        ok: r.ok,
                        remaining: r.remaining,
                        limit: r.limit,
                        error: r.error,
                    })
                    .collect(),
            }),
        )
            .into_response(),
        Err(e) => database_problem(e),
    }
}
