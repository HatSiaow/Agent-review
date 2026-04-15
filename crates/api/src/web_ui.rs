//! Server-rendered HTML (Askama). Queue uses the same `list_reviews_filtered` path as the JSON API.

use askama::Template;
use axum::extract::Query;
use axum::extract::State;
use axum::http::header;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Form;
use axum::Router;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth_cookies::{
    login_set_cookie_headers, logout_clear_cookie_headers, sign_session_cookie,
};
use crate::problem::ApiError;
use crate::Store;

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    csrf_token: String,
    error: String,
}

#[derive(Template)]
#[template(path = "queue.html")]
struct QueuePage {
    user_email: String,
    csrf_token: String,
    tab: String,
    platform_filter: String,
    rating_filter: String,
    q: String,
    rows: Vec<QueueRow>,
}

pub struct QueueRow {
    pub id: Uuid,
    pub platform: String,
    pub author: String,
    pub rating: u8,
    pub status: String,
    pub body: String,
    pub draft_state: String,
    pub draft_text: String,
    pub draft_warn: bool,
}

#[derive(Template)]
#[template(path = "review_detail.html")]
struct ReviewDetailPage {
    platform: String,
    author: String,
    rating: u8,
    status: String,
    body: String,
    draft_state: String,
    draft_text: String,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    csrf_token: String,
    saved: bool,
    restaurant_name: String,
    cuisine_style: String,
    context_line: String,
    voice_tone: String,
    signature_dishes: String,
    opening_hours_text: String,
    notifier_quiet_hours: String,
}

#[derive(Template)]
#[template(path = "users.html")]
struct UsersPage {
    users: Vec<UserRow>,
}

struct UserRow {
    email: String,
    role: String,
}

#[derive(Debug, Deserialize)]
pub struct QueueQuery {
    #[serde(default)]
    pub tab: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub rating: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub email: String,
    pub password: String,
    pub csrf: String,
}

#[derive(Debug, Deserialize)]
pub struct SettingsForm {
    pub restaurant_name: String,
    pub cuisine_style: String,
    pub context_line: String,
    pub voice_tone: String,
    pub signature_dishes: String,
    pub opening_hours_text: String,
    pub notifier_quiet_hours: String,
    pub csrf: String,
}

#[derive(Debug, Deserialize)]
struct LogoutForm {
    csrf: String,
}

#[derive(Debug, Deserialize)]
struct SettingsSavedQuery {
    saved: Option<bool>,
}

fn parse_cookie(header_val: &str, name: &str) -> Option<String> {
    header_val
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

fn csrf_ok(headers: &HeaderMap, form_csrf: &str) -> bool {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    parse_cookie(cookie, "csrf_token").as_deref() == Some(form_csrf)
}

async fn session_user(store: &Store, headers: &HeaderMap) -> Option<domain::User> {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_val = parse_cookie(cookie, "session")?;
    let sid = session_val
        .split_once('.')
        .and_then(|(s, _)| Uuid::parse_str(s).ok())?;
    let now = OffsetDateTime::now_utc();
    store
        .get_session_user(sid, now)
        .await
        .ok()
        .flatten()
        .map(|(_session, user)| user)
}

fn queue_tab_from_query(tab: Option<&str>) -> storage::QueueTab {
    match tab.unwrap_or("needs") {
        "ready" => storage::QueueTab::ReadyToSend,
        "history" => storage::QueueTab::History,
        _ => storage::QueueTab::NeedsYouNow,
    }
}

async fn page_queue(
    State(store): State<Store>,
    headers: HeaderMap,
    Query(q): Query<QueueQuery>,
) -> Result<Response, ApiError> {
    let Some(user) = session_user(&store, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    let csrf = parse_cookie(
        headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "csrf_token",
    )
    .unwrap_or_default();

    let tab_key = q.tab.as_deref().unwrap_or("needs");
    let queue_tab = queue_tab_from_query(q.tab.as_deref());

    let platform = q.platform.as_deref().and_then(|s| match s {
        "google" => Some(domain::Platform::Google),
        "ubereats" => Some(domain::Platform::Ubereats),
        _ => None,
    });

    let rating = q
        .rating
        .as_deref()
        .and_then(|s: &str| s.parse::<u8>().ok())
        .filter(|r| (1..=5).contains(r));

    let search =
        q.q.as_ref()
            .map(|s: &String| s.trim().to_string())
            .filter(|s: &String| !s.is_empty());

    let list_q = storage::ReviewListQuery {
        platform,
        status: None,
        rating,
        q: search,
        queue: Some(queue_tab),
        sort: storage::ReviewSort::UpdatedAtDesc,
    };

    let items = store.list_reviews_filtered(list_q).await;
    let rows: Vec<QueueRow> = items
        .into_iter()
        .map(|(review, d)| {
            let (draft_state, draft_text, draft_warn) = match d {
                Some(dr) => (dr.state.to_string(), dr.text.clone(), dr.has_warnings()),
                None => (String::new(), String::new(), false),
            };
            QueueRow {
                id: review.id,
                platform: review.platform.to_string(),
                author: review.author.display_name.clone(),
                rating: review.rating,
                status: review.status.to_string(),
                body: review.body_text.clone().unwrap_or_default(),
                draft_state,
                draft_text,
                draft_warn,
            }
        })
        .collect();

    let page = QueuePage {
        user_email: user.email,
        csrf_token: csrf,
        tab: tab_key.to_string(),
        platform_filter: q.platform.clone().unwrap_or_default(),
        rating_filter: q.rating.clone().unwrap_or_default(),
        q: q.q.clone().unwrap_or_default(),
        rows,
    };
    Ok(Html(page.render().map_err(|_| ApiError::ServiceUnavailable)?).into_response())
}

async fn page_review_detail(
    State(store): State<Store>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Response, ApiError> {
    let Some(_) = session_user(&store, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };

    let (review, draft) = store.get_review(id).await?;
    let (draft_state, draft_text) = match draft {
        Some(d) => (d.state.to_string(), d.text),
        None => (String::new(), String::new()),
    };

    let page = ReviewDetailPage {
        platform: review.platform.to_string(),
        author: review.author.display_name,
        rating: review.rating,
        status: review.status.to_string(),
        body: review.body_text.unwrap_or_default(),
        draft_state,
        draft_text,
    };
    Ok(Html(page.render().map_err(|_| ApiError::ServiceUnavailable)?).into_response())
}

async fn page_settings_get(
    State(store): State<Store>,
    headers: HeaderMap,
    Query(q): Query<SettingsSavedQuery>,
) -> Result<Response, ApiError> {
    let Some(user) = session_user(&store, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    if user.role != domain::UserRole::Owner {
        return Err(ApiError::Forbidden);
    }
    let csrf = parse_cookie(
        headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "csrf_token",
    )
    .unwrap_or_default();

    let s = store.get_restaurant_settings().await?;
    let page = SettingsPage {
        csrf_token: csrf,
        saved: q.saved.unwrap_or(false),
        restaurant_name: s.restaurant_name,
        cuisine_style: s.cuisine_style,
        context_line: s.context_line,
        voice_tone: s.voice_tone.unwrap_or_default(),
        signature_dishes: s.signature_dishes.join(", "),
        opening_hours_text: s.opening_hours_text.unwrap_or_default(),
        notifier_quiet_hours: s.notifier_quiet_hours.unwrap_or_default(),
    };
    Ok(Html(page.render().map_err(|_| ApiError::ServiceUnavailable)?).into_response())
}

async fn page_settings_post(
    State(store): State<Store>,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Result<Response, ApiError> {
    let Some(user) = session_user(&store, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    if user.role != domain::UserRole::Owner {
        return Err(ApiError::Forbidden);
    }
    if !csrf_ok(&headers, &form.csrf) {
        return Err(ApiError::Forbidden);
    }

    let dishes: Vec<String> = form
        .signature_dishes
        .split(',')
        .map(|s: &str| s.trim().to_string())
        .filter(|s: &String| !s.is_empty())
        .collect();

    let patch = domain::RestaurantSettingsPatch {
        restaurant_name: Some(form.restaurant_name),
        cuisine_style: Some(form.cuisine_style),
        context_line: Some(form.context_line),
        voice_tone: Some(if form.voice_tone.trim().is_empty() {
            None
        } else {
            Some(form.voice_tone)
        }),
        signature_dishes: Some(dishes),
        opening_hours_text: Some(if form.opening_hours_text.trim().is_empty() {
            None
        } else {
            Some(form.opening_hours_text)
        }),
        notifier_quiet_hours: Some(if form.notifier_quiet_hours.trim().is_empty() {
            None
        } else {
            Some(form.notifier_quiet_hours)
        }),
    };
    store.update_restaurant_settings(patch).await?;
    Ok(Redirect::to("/settings?saved=true").into_response())
}

async fn page_users(State(store): State<Store>, headers: HeaderMap) -> Result<Response, ApiError> {
    let Some(user) = session_user(&store, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    if user.role != domain::UserRole::Owner {
        return Err(ApiError::Forbidden);
    }

    let users = store.list_users().await?;
    let users: Vec<UserRow> = users
        .into_iter()
        .map(|u| UserRow {
            email: u.email,
            role: u.role.to_string(),
        })
        .collect();

    let page = UsersPage { users };
    Ok(Html(page.render().map_err(|_| ApiError::ServiceUnavailable)?).into_response())
}

async fn login_form_get(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if session_user(&store, &headers).await.is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    let csrf = Uuid::new_v4().to_string();
    let mut resp_headers = HeaderMap::new();
    let sec = if std::env::var("APP_COOKIE_SECURE").ok().as_deref() == Some("1") {
        "; Secure"
    } else {
        ""
    };
    resp_headers.append(
        header::SET_COOKIE,
        format!("csrf_token={csrf}; Path=/; SameSite=Lax{sec}")
            .parse()
            .map_err(|_| ApiError::ServiceUnavailable)?,
    );
    let body = LoginPage {
        csrf_token: csrf.clone(),
        error: String::new(),
    }
    .render()
    .map_err(|_| ApiError::ServiceUnavailable)?;
    Ok((resp_headers, Html(body)).into_response())
}

async fn login_form_post(
    State(store): State<Store>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Response, ApiError> {
    if !csrf_ok(&headers, &form.csrf) {
        return Err(ApiError::Forbidden);
    }
    use argon2::password_hash::PasswordHash;
    use argon2::PasswordVerifier as _;

    let email = form.email.trim();
    let now = OffsetDateTime::now_utc();
    let csrf = parse_cookie(
        headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "csrf_token",
    )
    .unwrap_or_default();

    if !crate::login_rate_limit::allow_attempt(email, now) {
        let body = LoginPage {
            csrf_token: csrf.clone(),
            error: "Invalid email or password".into(),
        }
        .render()
        .map_err(|_| ApiError::ServiceUnavailable)?;
        return Ok((axum::http::StatusCode::BAD_REQUEST, Html(body)).into_response());
    }

    let Some(auth) = store.get_user_auth_by_email(email).await? else {
        crate::login_rate_limit::record_failure(email, now);
        let body = LoginPage {
            csrf_token: csrf.clone(),
            error: "Invalid email or password".into(),
        }
        .render()
        .map_err(|_| ApiError::ServiceUnavailable)?;
        return Ok((axum::http::StatusCode::BAD_REQUEST, Html(body)).into_response());
    };

    let parsed_hash = match PasswordHash::new(&auth.password_hash) {
        Ok(v) => v,
        Err(_) => {
            crate::login_rate_limit::record_failure(email, now);
            let body = LoginPage {
                csrf_token: csrf.clone(),
                error: "Invalid email or password".into(),
            }
            .render()
            .map_err(|_| ApiError::ServiceUnavailable)?;
            return Ok((axum::http::StatusCode::BAD_REQUEST, Html(body)).into_response());
        }
    };
    if argon2::Argon2::default()
        .verify_password(form.password.as_bytes(), &parsed_hash)
        .is_err()
    {
        crate::login_rate_limit::record_failure(email, now);
        let body = LoginPage {
            csrf_token: csrf.clone(),
            error: "Invalid email or password".into(),
        }
        .render()
        .map_err(|_| ApiError::ServiceUnavailable)?;
        return Ok((axum::http::StatusCode::BAD_REQUEST, Html(body)).into_response());
    }

    crate::login_rate_limit::record_success(email);
    let session = domain::Session {
        id: Uuid::new_v4(),
        user_id: auth.user.id,
        created_at: now,
        expires_at: now + time::Duration::days(14),
    };
    store.create_session(session.clone()).await?;

    let csrf_new = Uuid::new_v4().to_string();
    let session_value = sign_session_cookie(session.id, store.session_hmac_key())?;
    let headers_out = login_set_cookie_headers(&session_value, &csrf_new)?;
    Ok((headers_out, Redirect::to("/")).into_response())
}

async fn logout_post(
    State(store): State<Store>,
    headers: HeaderMap,
    Form(form): Form<LogoutForm>,
) -> Result<Response, ApiError> {
    if !csrf_ok(&headers, &form.csrf) {
        return Err(ApiError::Forbidden);
    }
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session = parse_cookie(cookie, "session")
        .and_then(|v| v.split_once('.').map(|(sid, _)| sid.to_string()))
        .and_then(|sid| Uuid::parse_str(&sid).ok());
    if let Some(session_id) = session {
        let _ = store.delete_session(session_id).await;
    }
    let h = logout_clear_cookie_headers()?;
    Ok((h, Redirect::to("/login")).into_response())
}

pub fn router() -> Router<Store> {
    Router::new()
        .route("/", get(page_queue))
        .route("/login", get(login_form_get).post(login_form_post))
        .route("/logout", post(logout_post))
        .route("/reviews/:id", get(page_review_detail))
        .route("/settings", get(page_settings_get).post(page_settings_post))
        .route("/users", get(page_users))
}
