//! Session + CSRF cookie helpers shared by JSON API and HTML login.

use http::HeaderMap;
use http::header;
use uuid::Uuid;

use crate::problem::ApiError;

pub fn sign_session_cookie(session_id: Uuid, session_hmac_key: &[u8]) -> Result<String, ApiError> {
    use hmac::Mac as _;
    if session_hmac_key.len() < 32 {
        return Err(ApiError::ServiceUnavailable);
    }
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(session_hmac_key)
        .map_err(|_| ApiError::ServiceUnavailable)?;
    mac.update(session_id.as_bytes());
    let sig = mac.finalize().into_bytes();
    Ok(format!("{session_id}.{}", hex::encode(sig)))
}

fn secure_suffix() -> &'static str {
    if std::env::var("APP_COOKIE_SECURE").ok().as_deref() == Some("1") {
        "; Secure"
    } else {
        ""
    }
}

/// `Set-Cookie` headers for a new session + CSRF token (browser flows).
pub fn login_set_cookie_headers(session_value: &str, csrf_token: &str) -> Result<HeaderMap, ApiError> {
    let sec = secure_suffix();
    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        format!("session={session_value}; Path=/; HttpOnly; SameSite=Lax{sec}")
            .parse()
            .map_err(|_| ApiError::ServiceUnavailable)?,
    );
    headers.append(
        header::SET_COOKIE,
        format!("csrf_token={csrf_token}; Path=/; SameSite=Lax{sec}")
            .parse()
            .map_err(|_| ApiError::ServiceUnavailable)?,
    );
    Ok(headers)
}

pub fn logout_clear_cookie_headers() -> Result<HeaderMap, ApiError> {
    let sec = secure_suffix();
    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        format!("session=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax{sec}")
            .parse()
            .map_err(|_| ApiError::ServiceUnavailable)?,
    );
    headers.append(
        header::SET_COOKIE,
        format!("csrf_token=; Path=/; Max-Age=0; SameSite=Lax{sec}")
            .parse()
            .map_err(|_| ApiError::ServiceUnavailable)?,
    );
    Ok(headers)
}
