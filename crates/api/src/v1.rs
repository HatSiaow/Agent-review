use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use domain::ReplyDraft;
use http::HeaderMap;
use http::header;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::ActingUser;
use crate::problem::ApiError;
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
        .route("/reviews/:id", get(get_review))
        .route("/reviews/:id/skip", post(skip_review))
        .route("/reviews/:id/unskip", post(unskip_review))
        .route("/reviews/:id/regenerate", post(regenerate_review))
        .route("/drafts", get(list_drafts))
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

fn sign_session_cookie(session_id: Uuid) -> Result<String, ApiError> {
    use hmac::Mac as _;
    let secret = std::env::var("APP_SESSION_SECRET").map_err(|_| ApiError::ServiceUnavailable)?;
    if secret.trim().len() < 32 {
        return Err(ApiError::ServiceUnavailable);
    }
    let mut mac =
        hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| ApiError::ServiceUnavailable)?;
    mac.update(session_id.as_bytes());
    let sig = mac.finalize().into_bytes();
    Ok(format!("{session_id}.{}", hex::encode(sig)))
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
    let Some(auth) = store.get_user_auth_by_email(email).await? else {
        return Err(ApiError::Unauthorized);
    };

    let parsed_hash = PasswordHash::new(&auth.password_hash).map_err(|_| ApiError::Unauthorized)?;
    argon2::Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Unauthorized)?;

    let now = OffsetDateTime::now_utc();
    let session = domain::Session {
        id: Uuid::new_v4(),
        user_id: auth.user.id,
        created_at: now,
        expires_at: now + time::Duration::days(14),
    };
    store.create_session(session.clone()).await?;

    let csrf_token = Uuid::new_v4().to_string();
    let session_value = sign_session_cookie(session.id)?;

    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        format!(
            "session={session_value}; Path=/; HttpOnly; SameSite=Lax"
        )
        .parse()
        .map_err(|_| ApiError::ServiceUnavailable)?,
    );
    headers.append(
        header::SET_COOKIE,
        format!("csrf_token={csrf_token}; Path=/; SameSite=Lax")
            .parse()
            .map_err(|_| ApiError::ServiceUnavailable)?,
    );

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

    let mut out = HeaderMap::new();
    out.append(
        header::SET_COOKIE,
        "session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"
            .parse()
            .map_err(|_| ApiError::ServiceUnavailable)?,
    );
    Ok(out)
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
        return Err(ApiError::BadRequest("idempotent error replay not supported"));
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

async fn list_reviews(State(store): State<Store>, _user: ActingUser) -> Json<Vec<ReviewListItem>> {
    let reviews = store.list_reviews().await;
    let out = reviews
        .into_iter()
        .map(|(review, active_draft)| ReviewListItem {
            review,
            active_draft,
        })
        .collect();
    Json(out)
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

async fn list_drafts(State(store): State<Store>, _user: ActingUser) -> Json<Vec<ReplyDraft>> {
    Json(store.list_drafts().await)
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
    if let Some(cached) = maybe_idempotent_success::<ApproveResponse>(&store, &headers).await?
    {
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
    if let Some(cached) = maybe_idempotent_success::<Vec<ReplyDraft>>(&store, &headers).await?
    {
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
    if let Some(cached) = maybe_idempotent_success::<Vec<ReplyDraft>>(&store, &headers).await?
    {
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
        std::env::set_var("APP_SESSION_SECRET", "test-test-test-test-test-test-test-test-1234");
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

    fn auth_headers(builder: http::request::Builder, session: &str, csrf: &str) -> http::request::Builder {
        builder
            .header("x-csrf-token", csrf)
            .header(http::header::COOKIE, format!("session={session}; csrf_token={csrf}"))
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
        let draft =
            domain::ReplyDraft::new_pending(review_id, "Thanks!".into(), "en".into());
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
}
