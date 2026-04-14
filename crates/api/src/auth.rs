use axum::{async_trait, extract::FromRequestParts};
use http::request::Parts;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::problem::ApiError;
use crate::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActingUser {
    pub id: Uuid,
    pub role: domain::UserRole,
}

#[async_trait]
impl FromRequestParts<Store> for ActingUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, store: &Store) -> Result<Self, Self::Rejection> {
        extract_from_cookie(parts, store).await
    }
}

fn session_secret() -> Result<Vec<u8>, ApiError> {
    let raw = std::env::var("APP_SESSION_SECRET").map_err(|_| ApiError::ServiceUnavailable)?;
    if raw.trim().len() < 32 {
        return Err(ApiError::ServiceUnavailable);
    }
    Ok(raw.into_bytes())
}

fn parse_cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

fn verify_session_cookie(value: &str) -> Result<Uuid, ApiError> {
    use hmac::Mac as _;

    let (sid, sig_hex) = value.split_once('.').ok_or(ApiError::Unauthorized)?;
    let sid = Uuid::parse_str(sid).map_err(|_| ApiError::Unauthorized)?;
    let sig = hex::decode(sig_hex).map_err(|_| ApiError::Unauthorized)?;

    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&session_secret()?)
        .map_err(|_| ApiError::ServiceUnavailable)?;
    mac.update(sid.as_bytes());
    let expected = mac.finalize().into_bytes();
    if sig.len() != expected.len() || subtle::ConstantTimeEq::ct_eq(sig.as_slice(), expected.as_slice()).unwrap_u8() != 1 {
        return Err(ApiError::Unauthorized);
    }
    Ok(sid)
}

async fn extract_from_cookie(parts: &Parts, store: &Store) -> Result<ActingUser, ApiError> {
    let cookie_header = parts
        .headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(value) = parse_cookie_value(cookie_header, "session") else {
        return Err(ApiError::Unauthorized);
    };
    let session_id = verify_session_cookie(&value)?;

    let now = OffsetDateTime::now_utc();
    let Some((_session, user)) = store.get_session_user(session_id, now).await? else {
        return Err(ApiError::Unauthorized);
    };

    // Sliding expiry: 14 days.
    let new_expires_at = now + time::Duration::days(14);
    let _ = store.touch_session(session_id, new_expires_at).await;

    Ok(ActingUser {
        id: user.id,
        role: user.role,
    })
}
