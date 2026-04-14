use axum::http::HeaderMap;
use axum::{async_trait, extract::FromRequestParts};
use http::request::Parts;
use uuid::Uuid;

use crate::problem::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActingUser {
    pub id: Uuid,
    pub role: domain::UserRole,
}

#[async_trait]
impl<S> FromRequestParts<S> for ActingUser
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        extract_from_headers(&parts.headers)
    }
}

fn extract_from_headers(headers: &HeaderMap) -> Result<ActingUser, ApiError> {
    let user_id = headers
        .get("x-user-id")
        .ok_or(ApiError::Unauthorized)?
        .to_str()
        .map_err(|_| ApiError::Unauthorized)?;
    let id = Uuid::parse_str(user_id).map_err(|_| ApiError::Unauthorized)?;

    let role = headers
        .get("x-user-role")
        .ok_or(ApiError::Unauthorized)?
        .to_str()
        .map_err(|_| ApiError::Unauthorized)?;

    let role = match role {
        "owner" => domain::UserRole::Owner,
        "manager" => domain::UserRole::Manager,
        "viewer" => domain::UserRole::Viewer,
        _ => return Err(ApiError::Unauthorized),
    };

    Ok(ActingUser { id, role })
}
