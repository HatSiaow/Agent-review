use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Serialize)]
pub struct ProblemDetails<'a> {
    #[serde(rename = "type")]
    pub ty: &'a str,
    pub title: &'a str,
    pub status: u16,
    pub code: &'a str,
    pub instance: &'a str,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,
    #[error("invalid transition")]
    InvalidTransition,
    #[error("unauthorized")]
    Unauthorized,
    #[error("bad request: {0}")]
    BadRequest(&'static str),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidTransition => StatusCode::CONFLICT,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::InvalidTransition => "invalid_transition",
            Self::Unauthorized => "unauthorized",
            Self::BadRequest(_) => "bad_request",
        }
    }

    fn ty_url(&self) -> &'static str {
        match self {
            Self::NotFound => "https://agent-review/errors/not-found",
            Self::InvalidTransition => "https://agent-review/errors/invalid-transition",
            Self::Unauthorized => "https://agent-review/errors/unauthorized",
            Self::BadRequest(_) => "https://agent-review/errors/bad-request",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let title = match &self {
            Self::NotFound => "Not found",
            Self::InvalidTransition => "Invalid state transition",
            Self::Unauthorized => "Unauthorized",
            Self::BadRequest(msg) => msg,
        };

        let body = ProblemDetails {
            ty: self.ty_url(),
            title,
            status: status.as_u16(),
            code: self.code(),
            instance: "",
        };
        (status, Json(body)).into_response()
    }
}

