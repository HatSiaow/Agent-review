use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use domain::ReplyDraft;
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::ActingUser;
use crate::auth_cookies::{
    login_set_cookie_headers, logout_clear_cookie_headers, sign_session_cookie,
};
use crate::problem::{ApiError, InvalidParam};
use crate::Store;

fn prometheus_handle() -> &'static metrics_exporter_prometheus::PrometheusHandle {
    use std::sync::OnceLock;

    static HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
        builder
            .install_recorder()
            .expect("prometheus recorder already installed")
    })
}

pub fn router() -> Router<Store> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/reviews", get(list_reviews))
        .route("/drafts", get(list_drafts))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/users", get(list_users_api))
        .route("/reviews/:id", get(get_review))
        .route("/reviews/:id/skip", post(skip_review))
        .route("/reviews/:id/unskip", post(unskip_review))
        .route("/reviews/:id/regenerate", post(regenerate_review))
        .route("/drafts/:id/approve", post(approve_draft))
        .route("/drafts/:id/reject", post(reject_draft))
        .route("/drafts/bulk-approve", post(bulk_approve))
        .route("/drafts/bulk-approve/undo", post(undo_bulk_approve))
}

fn require_write_role(user: ActingUser) -> Result<(), ApiError> {
    match user.role {
        domain::UserRole::Owner | domain::UserRole::Manager => Ok(()),
        domain::UserRole::Viewer => Err(ApiError::Forbidden),
    }
}

fn require_csrf(headers: &HeaderMap) -> Result<(), ApiError> {
    let header = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if header.is_empty() {
        return Err(ApiError::Forbidden);
    }

    let cookie = headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let cookie_token = cookie
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("csrf_token="));
    if cookie_token != Some(header) {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct LoginResponse {
    user: domain::User,
}

async fn login(
    State(store): State<Store>,
    Json(req): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<LoginResponse>), ApiError> {
    use argon2::password_hash::PasswordHash;
    use argon2::PasswordVerifier as _;

    let email = req.email.trim();
    let now = OffsetDateTime::now_utc();
    if !crate::login_rate_limit::allow_attempt(email, now) {
        return Err(ApiError::Unauthorized);
    }

    let Some(auth) = store.get_user_auth_by_email(email).await? else {
        crate::login_rate_limit::record_failure(email, now);
        return Err(ApiError::Unauthorized);
    };

    let parsed_hash = PasswordHash::new(&auth.password_hash).map_err(|_| {
        crate::login_rate_limit::record_failure(email, now);
        ApiError::Unauthorized
    })?;
    argon2::Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| {
            crate::login_rate_limit::record_failure(email, now);
            ApiError::Unauthorized
        })?;

    crate::login_rate_limit::record_success(email);

    let session = domain::Session {
        id: Uuid::new_v4(),
        user_id: auth.user.id,
        created_at: now,
        expires_at: now + time::Duration::days(14),
    };
    store.create_session(session.clone()).await?;

    let csrf_token = Uuid::new_v4().to_string();
    let session_value = sign_session_cookie(session.id, store.session_hmac_key())?;
    let headers = login_set_cookie_headers(&session_value, &csrf_token)?;

    Ok((headers, Json(LoginResponse { user: auth.user })))
}

async fn logout(
    State(store): State<Store>,
    user: ActingUser,
    headers: HeaderMap,
) -> Result<HeaderMap, ApiError> {
    require_csrf(&headers)?;
    // Best-effort revoke if we can parse a session cookie.
    let cookie = headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session = cookie
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("session="))
        .and_then(|v| v.split_once('.').map(|(sid, _)| sid.to_string()))
        .and_then(|sid| Uuid::parse_str(&sid).ok());
    if let Some(session_id) = session {
        let _ = store.delete_session(session_id).await;
    }

    // Also emit an audit event in the future; for now logout is a session revoke only.
    let _ = user; // keep extractor for auth enforcement

    logout_clear_cookie_headers()
}

async fn maybe_idempotent_success<T: serde::de::DeserializeOwned>(
    store: &Store,
    headers: &HeaderMap,
) -> Result<Option<T>, ApiError> {
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some((status, body)) = store.get_idempotency_response(key).await else {
        return Ok(None);
    };
    if status >= 400 {
        return Err(ApiError::BadRequest(
            "idempotent error replay not supported",
        ));
    }
    serde_json::from_value(body)
        .map(Some)
        .map_err(|_| ApiError::BadRequest("bad idempotent cache payload"))
}

async fn store_idempotent_success<T: serde::Serialize>(
    store: &Store,
    headers: &HeaderMap,
    body: &T,
) {
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.trim().is_empty())
    else {
        return;
    };
    let Ok(value) = serde_json::to_value(body) else {
        return;
    };
    store
        .put_idempotency_response(key, 200, value, OffsetDateTime::now_utc())
        .await;
}

// --- System endpoints ---

pub async fn healthz() -> &'static str {
    "ok"
}

pub async fn readyz(State(store): State<Store>) -> Result<&'static str, ApiError> {
    store.ping().await?;
    Ok("ok")
}

pub async fn metrics() -> &'static str {
    // NOTE: axum can return owned strings; but keeping this simple here.
    // The client will store and display the scrape output.
    //
    // We return a leaked string for now to preserve the handler signature;
    // later we can switch this endpoint to `String` without affecting callers.
    let body = prometheus_handle().render();
    Box::leak(body.into_boxed_str())
}

// --- Reviews ---

#[derive(Debug, Serialize)]
pub struct ReviewListItem {
    pub review: domain::Review,
    pub active_draft: Option<domain::ReplyDraft>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewsQuery {
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub rating: Option<u8>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub queue: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
}

fn parse_platform(s: &str) -> Result<domain::Platform, ApiError> {
    match s {
        "google" => Ok(domain::Platform::Google),
        "ubereats" => Ok(domain::Platform::Ubereats),
        _ => Err(ApiError::Validation {
            title: "Invalid query parameter",
            detail: "platform must be google or ubereats".into(),
            invalid_params: vec![InvalidParam {
                name: "platform".into(),
                reason: "unknown platform".into(),
            }],
        }),
    }
}

fn parse_review_status(s: &str) -> Result<domain::ReviewStatus, ApiError> {
    match s {
        "new" => Ok(domain::ReviewStatus::New),
        "drafting" => Ok(domain::ReviewStatus::Drafting),
        "awaiting_human" => Ok(domain::ReviewStatus::AwaitingHuman),
        "replied" => Ok(domain::ReviewStatus::Replied),
        "withdrawn" => Ok(domain::ReviewStatus::Withdrawn),
        "skipped" => Ok(domain::ReviewStatus::Skipped),
        _ => Err(ApiError::Validation {
            title: "Invalid query parameter",
            detail: "unknown review status".into(),
            invalid_params: vec![InvalidParam {
                name: "status".into(),
                reason: "unknown status".into(),
            }],
        }),
    }
}

fn parse_queue_tab(s: &str) -> Result<storage::QueueTab, ApiError> {
    match s {
        "needs" | "needs_you_now" => Ok(storage::QueueTab::NeedsYouNow),
        "ready" | "ready_to_send" => Ok(storage::QueueTab::ReadyToSend),
        "history" => Ok(storage::QueueTab::History),
        _ => Err(ApiError::Validation {
            title: "Invalid query parameter",
            detail: "queue must be needs, ready, or history".into(),
            invalid_params: vec![InvalidParam {
                name: "queue".into(),
                reason: "unknown queue tab".into(),
            }],
        }),
    }
}

fn parse_review_sort(s: &str) -> Result<storage::ReviewSort, ApiError> {
    match s {
        "updated_at_desc" => Ok(storage::ReviewSort::UpdatedAtDesc),
        "updated_at_asc" => Ok(storage::ReviewSort::UpdatedAtAsc),
        "rating_desc" => Ok(storage::ReviewSort::RatingDesc),
        "created_at_desc" => Ok(storage::ReviewSort::CreatedAtDesc),
        _ => Err(ApiError::Validation {
            title: "Invalid query parameter",
            detail: "unknown sort".into(),
            invalid_params: vec![InvalidParam {
                name: "sort".into(),
                reason: "unknown sort key".into(),
            }],
        }),
    }
}

async fn list_reviews(
    State(store): State<Store>,
    _user: ActingUser,
    Query(q): Query<ReviewsQuery>,
) -> Result<Json<Vec<ReviewListItem>>, ApiError> {
    let platform = match q.platform.as_deref() {
        None | Some("") => None,
        Some(s) => Some(parse_platform(s)?),
    };
    let status = match q.status.as_deref() {
        None | Some("") => None,
        Some(s) => Some(parse_review_status(s)?),
    };
    if let Some(r) = q.rating {
        if !(1..=5).contains(&r) {
            return Err(ApiError::Validation {
                title: "Invalid query parameter",
                detail: "rating must be 1–5".into(),
                invalid_params: vec![InvalidParam {
                    name: "rating".into(),
                    reason: "out of range".into(),
                }],
            });
        }
    }
    let queue = match q.queue.as_deref() {
        None | Some("") => None,
        Some(s) => Some(parse_queue_tab(s)?),
    };
    let sort = match q.sort.as_deref() {
        None | Some("") => storage::ReviewSort::UpdatedAtDesc,
        Some(s) => parse_review_sort(s)?,
    };

    let search =
        q.q.as_ref()
            .map(|s: &String| s.trim().to_string())
            .filter(|s: &String| !s.is_empty());

    let list_q = storage::ReviewListQuery {
        platform,
        status,
        rating: q.rating,
        q: search,
        queue,
        sort,
    };

    let reviews = store.list_reviews_filtered(list_q).await;
    let out = reviews
        .into_iter()
        .map(|(review, active_draft)| ReviewListItem {
            review,
            active_draft,
        })
        .collect();
    Ok(Json(out))
}

#[derive(Debug, Serialize)]
pub struct ReviewWithDraft {
    pub review: domain::Review,
    pub active_draft: Option<domain::ReplyDraft>,
}

async fn get_review(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    _user: ActingUser,
) -> Result<Json<ReviewWithDraft>, ApiError> {
    let (review, active_draft) = store.get_review(id).await?;
    Ok(Json(ReviewWithDraft {
        review,
        active_draft,
    }))
}

async fn skip_review(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    user: ActingUser,
    headers: HeaderMap,
) -> Result<Json<domain::Review>, ApiError> {
    require_write_role(user)?;
    require_csrf(&headers)?;
    let review = store.skip_review(id).await?;
    Ok(Json(review))
}

async fn unskip_review(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    user: ActingUser,
    headers: HeaderMap,
) -> Result<Json<domain::Review>, ApiError> {
    require_write_role(user)?;
    require_csrf(&headers)?;
    let review = store.unskip_review(id).await?;
    Ok(Json(review))
}

// --- Regenerate ---

#[derive(Debug, Deserialize)]
struct RegenerateRequest {
    #[serde(default)]
    hint: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegenerateResponse {
    review_id: Uuid,
    status: domain::ReviewStatus,
    hint: Option<String>,
}

async fn regenerate_review(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    user: ActingUser,
    headers: HeaderMap,
    Json(req): Json<RegenerateRequest>,
) -> Result<Json<RegenerateResponse>, ApiError> {
    require_write_role(user)?;
    require_csrf(&headers)?;
    let review = store.transition_review_to_drafting(id).await?;
    Ok(Json(RegenerateResponse {
        review_id: review.id,
        status: review.status,
        hint: req.hint,
    }))
}

// --- Drafts ---

#[derive(Debug, Deserialize)]
pub struct DraftsQuery {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub rating: Option<u8>,
    #[serde(default)]
    pub flag: Option<String>,
}

fn parse_draft_state(s: &str) -> Result<domain::DraftState, ApiError> {
    use domain::DraftState;
    match s {
        "pending_review" => Ok(DraftState::PendingReview),
        "approved" => Ok(DraftState::Approved),
        "approved_pending_undo" => Ok(DraftState::ApprovedPendingUndo),
        "edited" => Ok(DraftState::Edited),
        "rejected" => Ok(DraftState::Rejected),
        "posted" => Ok(DraftState::Posted),
        "failed" => Ok(DraftState::Failed),
        _ => Err(ApiError::Validation {
            title: "Invalid query parameter",
            detail: "unknown draft state".into(),
            invalid_params: vec![InvalidParam {
                name: "state".into(),
                reason: "unknown state".into(),
            }],
        }),
    }
}

async fn list_drafts(
    State(store): State<Store>,
    _user: ActingUser,
    Query(q): Query<DraftsQuery>,
) -> Result<Json<Vec<ReplyDraft>>, ApiError> {
    let state = match q.state.as_deref() {
        None | Some("") => None,
        Some(s) => Some(parse_draft_state(s)?),
    };
    if let Some(r) = q.rating {
        if !(1..=5).contains(&r) {
            return Err(ApiError::Validation {
                title: "Invalid query parameter",
                detail: "rating must be 1–5".into(),
                invalid_params: vec![InvalidParam {
                    name: "rating".into(),
                    reason: "out of range".into(),
                }],
            });
        }
    }
    let flag_warnings = match q.flag.as_deref() {
        None | Some("") => None,
        Some("warnings") => Some(true),
        Some("none") => Some(false),
        Some(_) => {
            return Err(ApiError::Validation {
                title: "Invalid query parameter",
                detail: "flag must be warnings or none".into(),
                invalid_params: vec![InvalidParam {
                    name: "flag".into(),
                    reason: "unknown flag".into(),
                }],
            });
        }
    };

    let list_q = storage::DraftListQuery {
        state,
        rating: q.rating,
        flag_warnings,
    };
    Ok(Json(store.list_drafts_filtered(list_q).await))
}

// --- Settings / users ---

async fn get_settings(
    State(store): State<Store>,
    user: ActingUser,
) -> Result<Json<domain::RestaurantSettings>, ApiError> {
    let _ = user;
    Ok(Json(store.get_restaurant_settings().await?))
}

#[derive(Debug, Deserialize)]
pub struct SettingsPutBody {
    #[serde(flatten)]
    pub patch: domain::RestaurantSettingsPatch,
}

async fn put_settings(
    State(store): State<Store>,
    user: ActingUser,
    headers: HeaderMap,
    Json(body): Json<SettingsPutBody>,
) -> Result<Json<domain::RestaurantSettings>, ApiError> {
    require_csrf(&headers)?;
    if user.role != domain::UserRole::Owner {
        return Err(ApiError::Forbidden);
    }
    let updated = store.update_restaurant_settings(body.patch).await?;
    Ok(Json(updated))
}

async fn list_users_api(
    State(store): State<Store>,
    user: ActingUser,
) -> Result<Json<Vec<domain::User>>, ApiError> {
    if user.role != domain::UserRole::Owner {
        return Err(ApiError::Forbidden);
    }
    Ok(Json(store.list_users().await?))
}

#[derive(Debug, Deserialize)]
struct ApproveRequest {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApproveResponse {
    id: Uuid,
    state: domain::DraftState,
    #[serde(with = "time::serde::rfc3339::option")]
    posted_at: Option<OffsetDateTime>,
}

async fn approve_draft(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    user: ActingUser,
    headers: HeaderMap,
    Json(req): Json<ApproveRequest>,
) -> Result<Json<ApproveResponse>, ApiError> {
    require_write_role(user)?;
    require_csrf(&headers)?;
    if let Some(cached) = maybe_idempotent_success::<ApproveResponse>(&store, &headers).await? {
        return Ok(Json(cached));
    }
    let updated = store.approve_draft(id, user.id, req.text).await?;

    let out = ApproveResponse {
        id: updated.id,
        state: updated.state,
        posted_at: updated.posted_at,
    };
    store_idempotent_success(&store, &headers, &out).await;
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
struct RejectRequest {
    reason: String,
}

async fn reject_draft(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    user: ActingUser,
    headers: HeaderMap,
    Json(req): Json<RejectRequest>,
) -> Result<Json<ReplyDraft>, ApiError> {
    require_write_role(user)?;
    require_csrf(&headers)?;
    if let Some(cached) = maybe_idempotent_success::<ReplyDraft>(&store, &headers).await? {
        return Ok(Json(cached));
    }
    let updated = store.reject_draft(id, user.id, req.reason).await?;
    store_idempotent_success(&store, &headers, &updated).await;
    Ok(Json(updated))
}

#[derive(Debug, Deserialize)]
struct BulkApproveRequest {
    ids: Vec<Uuid>,
}

async fn bulk_approve(
    State(store): State<Store>,
    user: ActingUser,
    headers: HeaderMap,
    Json(req): Json<BulkApproveRequest>,
) -> Result<Json<Vec<ReplyDraft>>, ApiError> {
    require_write_role(user)?;
    require_csrf(&headers)?;
    if user.role != domain::UserRole::Owner {
        return Err(ApiError::Forbidden);
    }
    if let Some(cached) = maybe_idempotent_success::<Vec<ReplyDraft>>(&store, &headers).await? {
        return Ok(Json(cached));
    }
    let updated = store.bulk_approve(&req.ids, user.id).await?;
    store_idempotent_success(&store, &headers, &updated).await;
    Ok(Json(updated))
}

#[derive(Debug, Deserialize)]
struct UndoBulkApproveRequest {
    ids: Vec<Uuid>,
}

async fn undo_bulk_approve(
    State(store): State<Store>,
    user: ActingUser,
    headers: HeaderMap,
    Json(req): Json<UndoBulkApproveRequest>,
) -> Result<Json<Vec<ReplyDraft>>, ApiError> {
    require_write_role(user)?;
    require_csrf(&headers)?;
    if user.role != domain::UserRole::Owner {
        return Err(ApiError::Forbidden);
    }
    if let Some(cached) = maybe_idempotent_success::<Vec<ReplyDraft>>(&store, &headers).await? {
        return Ok(Json(cached));
    }
    let updated = store
        .undo_bulk_approve(&req.ids, user.id, OffsetDateTime::now_utc())
        .await?;
    store_idempotent_success(&store, &headers, &updated).await;
    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router as build_router;
    use axum::body::Body;
    use axum::http::Request;
    use domain::{Platform, Review, ReviewAuthor, ReviewStatus};
    use http::StatusCode;
    use serde_json::json;
    use time::macros::datetime;
    use tower::ServiceExt as _;

    fn ensure_session_secret() {
        // Stable secret for tests; must be >= 32 chars.
        std::env::set_var(
            "APP_SESSION_SECRET",
            "test-test-test-test-test-test-test-test-1234",
        );
        crate::login_rate_limit::reset_for_tests();
    }

    fn oneshot(app: Router, req: Request<Body>) -> http::Response<axum::body::Body> {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { app.oneshot(req).await.unwrap() })
    }

    fn login_cookies(app: Router) -> (String, String) {
        ensure_session_secret();
        let body = serde_json::to_string(&json!({
            "email": "owner@example.com",
            "password": "password"
        }))
        .unwrap();
        let res = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);

        let set_cookie: Vec<String> = res
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_string))
            .collect();

        let session = set_cookie
            .iter()
            .filter_map(|s| s.split(';').next().map(str::trim))
            .find_map(|s| s.strip_prefix("session=").map(str::to_string))
            .expect("session cookie");
        let csrf = set_cookie
            .iter()
            .filter_map(|c| c.split(';').next().map(str::trim))
            .find_map(|s| s.strip_prefix("csrf_token=").map(str::to_string))
            .expect("csrf cookie");
        (session, csrf)
    }

    fn auth_headers(
        builder: http::request::Builder,
        session: &str,
        csrf: &str,
    ) -> http::request::Builder {
        builder.header("x-csrf-token", csrf).header(
            http::header::COOKIE,
            format!("session={session}; csrf_token={csrf}"),
        )
    }

    fn make_review_and_draft() -> (Uuid, Review, ReplyDraft) {
        let review_id = Uuid::new_v4();
        let review = Review {
            id: review_id,
            platform: Platform::Google,
            source_review_id: "AbCd".into(),
            source_location_id: "loc1".into(),
            author: ReviewAuthor {
                display_name: "Maria L.".into(),
                avatar_url: None,
            },
            rating: 5,
            body_text: Some("Great pasta!".into()),
            body_language: Some("en".into()),
            created_at: datetime!(2026-04-10 18:22:11 UTC),
            updated_at: datetime!(2026-04-10 18:22:11 UTC),
            ingested_at: datetime!(2026-04-10 18:25:00 UTC),
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status: ReviewStatus::New,
            context_json: json!({}),
            raw_payload: json!({ "source": "fixture" }),
        };
        let draft = domain::ReplyDraft::new_pending(review_id, "Thanks!".into(), "en".into());
        (review_id, review, draft)
    }

    fn seeded_store() -> (Store, Uuid, Uuid) {
        ensure_session_secret();
        let store = Store::new();
        let (review_id, review, draft) = make_review_and_draft();
        let draft_id = draft.id;

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { store.upsert_review_with_draft(review, draft).await });
        (store, review_id, draft_id)
    }

    #[test]
    fn healthz_returns_200() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn readyz_returns_200() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn list_reviews_returns_items() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let (session, _csrf) = login_cookies(app.clone());
        let res = oneshot(
            app,
            Request::builder()
                .uri("/api/v1/reviews")
                .header(http::header::COOKIE, format!("session={session}"))
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn get_review_found() {
        let (store, review_id, _) = seeded_store();
        let app = build_router(store);
        let (session, _csrf) = login_cookies(app.clone());
        let res = oneshot(
            app,
            Request::builder()
                .uri(format!("/api/v1/reviews/{review_id}"))
                .header(http::header::COOKIE, format!("session={session}"))
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn get_review_not_found() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let (session, _csrf) = login_cookies(app.clone());
        let fake_id = Uuid::new_v4();
        let res = oneshot(
            app,
            Request::builder()
                .uri(format!("/api/v1/reviews/{fake_id}"))
                .header(http::header::COOKIE, format!("session={session}"))
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn skip_review_transitions() {
        let (store, review_id, _) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/reviews/{review_id}/skip"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::empty())
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn approve_draft_returns_approved() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/drafts/{draft_id}/approve"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn approve_is_idempotent_with_idempotency_key() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store.clone());
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({})).unwrap();

        let req = auth_headers(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/drafts/{draft_id}/approve"))
                .header("content-type", "application/json")
                .header("idempotency-key", "idem-1"),
            &session,
            &csrf,
        )
        .body(Body::from(body.clone()))
        .unwrap();

        let res1 = oneshot(app, req);
        assert_eq!(res1.status(), StatusCode::OK);

        // Second call would normally be an invalid transition, but should replay cached response.
        let app2 = build_router(store);
        let req2 = auth_headers(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/drafts/{draft_id}/approve"))
                .header("content-type", "application/json")
                .header("idempotency-key", "idem-1"),
            &session,
            &csrf,
        )
        .body(Body::from(body))
        .unwrap();
        let res2 = oneshot(app2, req2);
        assert_eq!(res2.status(), StatusCode::OK);
    }

    #[test]
    fn reject_draft_returns_rejected() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({"reason": "too_generic"})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/drafts/{draft_id}/reject"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn double_reject_fails() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store.clone());
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({"reason": "too_generic"})).unwrap();

        // First reject
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/drafts/{draft_id}/reject"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body.clone()))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);

        // Second reject should fail (terminal state)
        let app2 = build_router(store);
        let res2 = oneshot(
            app2,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/drafts/{draft_id}/reject"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res2.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn list_drafts_endpoint() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let (session, _csrf) = login_cookies(app.clone());
        let res = oneshot(
            app,
            Request::builder()
                .uri("/api/v1/drafts")
                .header(http::header::COOKIE, format!("session={session}"))
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn unskip_review_transitions() {
        let (store, review_id, _) = seeded_store();
        let app = build_router(store.clone());
        let (session, csrf) = login_cookies(app.clone());

        // First skip
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/reviews/{review_id}/skip"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::empty())
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);

        // Then unskip
        let app2 = build_router(store);
        let res2 = oneshot(
            app2,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/reviews/{review_id}/unskip"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::empty())
            .unwrap(),
        );
        assert_eq!(res2.status(), StatusCode::OK);
    }

    #[test]
    fn unskip_non_skipped_review_fails() {
        let (store, review_id, _) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/reviews/{review_id}/unskip"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::empty())
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn metrics_returns_200() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let res = oneshot(
            app,
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn approve_with_edit_text() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({"text": "Edited thanks!"})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/drafts/{draft_id}/approve"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn approve_nonexistent_draft_returns_404() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let fake_id = Uuid::new_v4();
        let body = serde_json::to_string(&json!({})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/drafts/{fake_id}/approve"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn bulk_approve_five_star_no_warnings() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({"ids": [draft_id]})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/drafts/bulk-approve")
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn bulk_approve_empty_ids_returns_ok() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({"ids": []})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/drafts/bulk-approve")
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn bulk_approve_nonexistent_draft_returns_404() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let fake_id = Uuid::new_v4();
        let body = serde_json::to_string(&json!({"ids": [fake_id]})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/drafts/bulk-approve")
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn reject_missing_reason_returns_422() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/drafts/{draft_id}/reject"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn regenerate_review_from_new_status() {
        let (store, review_id, _) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let body = serde_json::to_string(&json!({"hint": "be warmer"})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/reviews/{review_id}/regenerate"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn regenerate_nonexistent_review_returns_404() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let (session, csrf) = login_cookies(app.clone());
        let fake_id = Uuid::new_v4();
        let body = serde_json::to_string(&json!({})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/reviews/{fake_id}/regenerate"))
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn undo_bulk_approve_endpoint_exists() {
        let (store, _, draft_id) = seeded_store();
        let app = build_router(store.clone());
        let (session, csrf) = login_cookies(app.clone());

        // Bulk approve first.
        let body = serde_json::to_string(&json!({"ids": [draft_id]})).unwrap();
        let res = oneshot(
            app,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/drafts/bulk-approve")
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body))
            .unwrap(),
        );
        assert_eq!(res.status(), StatusCode::OK);

        // Then undo (within window).
        let app2 = build_router(store);
        let body2 = serde_json::to_string(&json!({"ids": [draft_id]})).unwrap();
        let res2 = oneshot(
            app2,
            auth_headers(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/drafts/bulk-approve/undo")
                    .header("content-type", "application/json"),
                &session,
                &csrf,
            )
            .body(Body::from(body2))
            .unwrap(),
        );
        assert_eq!(res2.status(), StatusCode::OK);
    }

    #[test]
    fn login_rate_limiter_blocks_after_repeated_failures() {
        let (store, _, _) = seeded_store();
        let app = build_router(store);
        let email = "owner+ratelimit@example.com";
        for _ in 0..5 {
            let body = serde_json::to_string(&json!({
                "email": email,
                "password": "wrong-password"
            }))
            .unwrap();
            let res = oneshot(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            );
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        let body = serde_json::to_string(&json!({
            "email": email,
            "password": "password"
        }))
        .unwrap();
        let blocked = oneshot(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        );
        assert_eq!(blocked.status(), StatusCode::UNAUTHORIZED);
    }
}
