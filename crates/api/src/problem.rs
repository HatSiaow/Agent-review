//! [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457) Problem Details for HTTP APIs.
//!
//! Responses use `Content-Type: application/problem+json`. The `instance` field is populated from
//! the request URI when [`crate::request_ctx::REQUEST_PATH`] is set (see `lib.rs` middleware).

use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

use crate::request_ctx::REQUEST_PATH;

#[derive(Debug, Clone, Serialize)]
pub struct InvalidParam {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub ty: String,
    pub title: String,
    pub status: u16,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub instance: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalid_params: Vec<InvalidParam>,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,
    #[error("invalid transition")]
    InvalidTransition,
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("bad request: {0}")]
    BadRequest(&'static str),

    /// Validation / malformed input with optional field-level hints (RFC 9457 extension).
    #[error("validation failed: {title}")]
    Validation {
        title: &'static str,
        detail: String,
        invalid_params: Vec<InvalidParam>,
    },

    #[error("service unavailable")]
    ServiceUnavailable,
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidTransition => StatusCode::CONFLICT,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::BadRequest(_) | Self::Validation { .. } => StatusCode::BAD_REQUEST,
            Self::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::InvalidTransition => "invalid_transition",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::BadRequest(_) => "bad_request",
            Self::Validation { .. } => "validation_error",
            Self::ServiceUnavailable => "service_unavailable",
        }
    }

    fn ty_url(&self) -> &'static str {
        match self {
            Self::NotFound => "https://agent-review/errors/not-found",
            Self::InvalidTransition => "https://agent-review/errors/invalid-transition",
            Self::Unauthorized => "https://agent-review/errors/unauthorized",
            Self::Forbidden => "https://agent-review/errors/forbidden",
            Self::BadRequest(_) => "https://agent-review/errors/bad-request",
            Self::Validation { .. } => "https://agent-review/errors/validation-error",
            Self::ServiceUnavailable => "https://agent-review/errors/service-unavailable",
        }
    }

    fn instance(&self) -> String {
        REQUEST_PATH
            .try_with(|p| p.clone())
            .unwrap_or_default()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let (title, detail, invalid): (String, Option<String>, Vec<InvalidParam>) = match &self {
            Self::NotFound => ("Not found".into(), None, vec![]),
            Self::InvalidTransition => ("Invalid state transition".into(), None, vec![]),
            Self::Unauthorized => ("Unauthorized".into(), None, vec![]),
            Self::Forbidden => ("Forbidden".into(), None, vec![]),
            Self::BadRequest(msg) => ((*msg).to_string(), None, vec![]),
            Self::Validation {
                title,
                detail,
                invalid_params,
            } => ((*title).to_string(), Some(detail.clone()), invalid_params.to_vec()),
            Self::ServiceUnavailable => ("Service unavailable".into(), None, vec![]),
        };

        let body = ProblemDetails {
            ty: self.ty_url().to_string(),
            title,
            status: status.as_u16(),
            code: self.code().to_string(),
            detail,
            instance: self.instance(),
            invalid_params: invalid,
        };
        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(body),
        )
            .into_response()
    }
}
