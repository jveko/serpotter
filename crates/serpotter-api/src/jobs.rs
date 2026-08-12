//! Async job API + in-process runner scaffold (B16).
//!
//! Jobs are rows in `provider_jobs` (storage/wiring owned by serpotter-db):
//! `POST /api/jobs` creates a row (status `running`) and spawns an in-process
//! worker that dispatches by `kind` and marks the row `done`/`failed`;
//! `GET /api/jobs` lists, `GET /api/jobs/{id}` reads one. All three endpoints
//! sit behind the same admin gate as the rest of `/api` (session Bearer or
//! ADMIN_SECRET).
//!
//! THIS WAVE lands the runner scaffold + lifecycle only: no kinds are
//! implemented yet (`tavily_research` arrives in the next wave, J2), so every
//! job fails with an honest `Unsupported job kind` error instead of
//! pretending to succeed. The dispatch point is [`run_job`].

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;
use serpotter_db::{Db, ProviderJobRow};

use crate::admin::require_admin;
use crate::cron::env_i64_or;
use crate::AppState;

/// Default job TTL when `JOB_TTL_SECS` is unset (1 hour).
pub(crate) const DEFAULT_JOB_TTL_SECS: i64 = 3600;

/// Read `JOB_TTL_SECS` (warn on unparseable value, like every other tuning env).
pub(crate) fn job_ttl_secs_from_env() -> i64 {
    env_i64_or("JOB_TTL_SECS", DEFAULT_JOB_TTL_SECS)
}

/// Mint a caller-supplied job id: 16 lowercase hex chars from 8 getrandom
/// bytes (uuid-ish without a dep). Falls back to a time-derived id if the
/// RNG fails — practically unreachable, but job creation must not deadlock
/// on entropy.
fn mint_job_id() -> String {
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{nanos:016x}")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobBody {
    pub kind: String,
    pub service: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListJobsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOut {
    id: String,
    kind: String,
    service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    created_at: String,
    updated_at: String,
    expires_at: String,
}

fn job_out(row: ProviderJobRow) -> JobOut {
    JobOut {
        id: row.id,
        kind: row.kind,
        service: row.service,
        params: serde_json::from_str(&row.params_json).ok(),
        status: row.status,
        result: row
            .result_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        error: row.error,
        created_at: row.created_at,
        updated_at: row.updated_at,
        expires_at: row.expires_at,
    }
}

/// POST /api/jobs — create a job row (status `running`) and spawn its worker.
pub async fn create_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateJobBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let kind = body.kind.trim();
    let service = body.service.trim();
    if kind.is_empty() || service.is_empty() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "kind and service are required",
        );
    }
    let ttl = body.ttl_secs.unwrap_or_else(job_ttl_secs_from_env);
    if ttl <= 0 {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "ttlSecs must be positive",
        );
    }
    let params_json = body
        .params
        .map(|p| p.to_string())
        .unwrap_or_else(|| "{}".into());
    let id = mint_job_id();
    match ctx
        .db
        .create_job(&id, kind, service, &params_json, ttl)
        .await
    {
        Ok(row) => {
            let db = state.db.clone();
            tokio::spawn(async move { process_job(db, id).await });
            (StatusCode::CREATED, Json(job_out(row))).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// GET /api/jobs?limit=N — newest-first list (db clamps limit to 1..=100).
pub async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListJobsQuery>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.list_jobs(q.limit.unwrap_or(20)).await {
        Ok(rows) => {
            let out: Vec<JobOut> = rows.into_iter().map(job_out).collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// GET /api/jobs/{id} — one job by id (404 when unknown).
pub async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.get_job(&id).await {
        Ok(Some(row)) => (StatusCode::OK, Json(job_out(row))).into_response(),
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "job not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// Run one job and persist the outcome. Spawned by [`create_job`]; also the
/// deterministic unit under test. Unknown kinds are marked `failed` with the
/// honest dispatcher error — never a silent success.
pub async fn process_job(db: Db, id: String) {
    match run_job(&db, &id).await {
        Ok(result) => {
            let _ = db.update_job_result(&id, "done", Some(&result), None).await;
        }
        Err(err) => {
            let _ = db.update_job_result(&id, "failed", None, Some(&err)).await;
        }
    }
}

/// Dispatch a job by kind. The runner scaffold lands this wave; real kinds
/// (e.g. `tavily_research`, J2) slot in here as `match` arms. Until then every
/// kind answers an explicit unsupported error.
async fn run_job(db: &Db, id: &str) -> Result<String, String> {
    let job = db
        .get_job(id)
        .await
        .map_err(|e| format!("job lookup failed: {e}"))?
        .ok_or_else(|| format!("job {id} not found"))?;
    // NEXT WAVE (J2): match job.kind.as_str() { "tavily_research" => … }.
    let kind = job.kind;
    Err(format!("Unsupported job kind: {kind}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use axum::Router;
    use serpotter_keypool::KeyPool;
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{
        ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
    };
    use tower::ServiceExt;

    const TEST_ADMIN_SECRET: &str = "test-admin-secret";

    fn router_with(state: AppState) -> Router {
        Router::new()
            .route("/api/jobs", post(create_job).get(list_jobs))
            .route("/api/jobs/{id}", get(get_job))
            .with_state(state)
    }

    async fn state_with(db: Db) -> AppState {
        AppState {
            keys: Arc::new(KeyPool::with_config(
                db.clone(),
                3,
                Duration::from_secs(30),
                serpotter_db::KEY_HOLD_TTL_SECS,
                serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
            )),
            outbound: Arc::new(ProxyPool::with_options(db.clone(), false)),
            providers: ProviderRegistry::with_clients(
                TavilyClient::new("http://127.0.0.1:9"),
                FirecrawlClient::new("http://127.0.0.1:9"),
                ExaClient::new("http://127.0.0.1:9"),
                XaiClient::new("http://127.0.0.1:9"),
            ),
            db,
            admin_secret: Some(TEST_ADMIN_SECRET.into()),
        }
    }

    #[test]
    fn minted_job_ids_are_16_lowercase_hex() {
        let a = mint_job_id();
        let b = mint_job_id();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.to_lowercase(), a);
        assert_ne!(a, b, "two mints must differ");
    }

    #[test]
    fn job_ttl_env_defaults_to_3600() {
        // env mutation guarded against parallel tests
        static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let _guard = ENV_LOCK.lock();
        std::env::remove_var("JOB_TTL_SECS");
        assert_eq!(job_ttl_secs_from_env(), DEFAULT_JOB_TTL_SECS);
        std::env::set_var("JOB_TTL_SECS", "900");
        assert_eq!(job_ttl_secs_from_env(), 900);
        std::env::remove_var("JOB_TTL_SECS");
    }

    #[tokio::test]
    async fn create_job_requires_admin_auth() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("db");
        let app = router_with(state_with(db).await);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"kind":"tavily_research","service":"tavily"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_job_returns_201_with_running_status() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("db");
        let app = router_with(state_with(db.clone()).await);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/jobs")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"kind":"tavily_research","service":"tavily","params":{"q":"x"},"ttlSecs":3600}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let v = body_value(res).await;
        assert_eq!(v["status"], "running");
        assert_eq!(v["kind"], "tavily_research");
        assert_eq!(v["params"]["q"], "x");
        let id = v["id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 16, "minted id is 16 hex chars: {id}");

        // The spawned worker must eventually mark the job failed (honest
        // unsupported-kind error — no kinds exist this wave).
        let mut failed = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Some(row) = db.get_job(&id).await.unwrap() {
                if row.status == "failed" {
                    failed = true;
                    assert!(
                        row.error
                            .as_deref()
                            .unwrap_or_default()
                            .contains("Unsupported job kind"),
                        "honest error text: {:?}",
                        row.error
                    );
                    break;
                }
            }
        }
        assert!(failed, "worker must mark the unknown kind failed");
    }

    #[tokio::test]
    async fn create_job_validation_400() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("db");
        let app = router_with(state_with(db).await);
        for (body, reason) in [
            (r#"{"kind":"","service":"tavily"}"#, "empty kind"),
            (r#"{"kind":"x","service":""}"#, "empty service"),
            (
                r#"{"kind":"x","service":"t","ttlSecs":0}"#,
                "non-positive ttl",
            ),
        ] {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/jobs")
                        .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "case: {reason}");
        }
    }

    #[tokio::test]
    async fn get_job_unknown_404_and_list_shape() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("db");
        let app = router_with(state_with(db.clone()).await);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/jobs/ffffffffffffffff")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        db.create_job("aaaaaaaaaaaaaaaa", "tavily_research", "tavily", "{}", 3600)
            .await
            .unwrap();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/jobs?limit=20")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_value(res).await;
        let arr = v.as_array().expect("jobs array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "aaaaaaaaaaaaaaaa");
        assert_eq!(arr[0]["status"], "running");
    }

    #[tokio::test]
    async fn process_job_marks_unknown_kind_failed_honestly() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("db");
        let row = db
            .create_job(
                "bbbbbbbbbbbbbbbb",
                "tavily_research",
                "tavily",
                r#"{"q":"x"}"#,
                3600,
            )
            .await
            .unwrap();
        assert_eq!(row.status, "running");

        process_job(db.clone(), "bbbbbbbbbbbbbbbb".to_string()).await;

        let after = db.get_job("bbbbbbbbbbbbbbbb").await.unwrap().unwrap();
        assert_eq!(after.status, "failed");
        assert!(
            after
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Unsupported job kind: tavily_research"),
            "honest dispatcher error: {:?}",
            after.error
        );
    }

    /// Parse a JSON response body (shared helper for this module's tests).
    async fn body_value(res: axum::response::Response) -> serde_json::Value {
        use http_body_util::BodyExt;
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).expect("json body")
    }
}
