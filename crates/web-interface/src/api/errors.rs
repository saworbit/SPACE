use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use thiserror::Error;
use tracing::error;
use uuid::Uuid;

use crate::api::models::{ApiErrorBody, ApiResponse, Meta};

/// Unified API error type mapped to HTTP responses.
#[derive(Error, Debug)]
pub enum ApiError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("internal server error: {0}")]
    Internal(String),
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::BadRequest(message.into())
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        ApiError::Unauthorized(message.into())
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        ApiError::Forbidden(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError::NotFound(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        ApiError::Internal(message.into())
    }

    fn as_tuple(&self) -> (StatusCode, &'static str, String) {
        match self {
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg.clone()),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, "FORBIDDEN", msg.clone()),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg.clone()),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "RESOURCE_NOT_FOUND", msg.clone()),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg.clone()),
            ApiError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_SERVER_ERROR",
                msg.clone(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.as_tuple();
        if status.is_server_error() {
            error!("API error: {}", message);
        }

        let request_id = Uuid::new_v4().to_string();
        let body = ApiResponse::<ApiErrorBody> {
            success: false,
            data: None,
            error: Some(ApiErrorBody {
                code: code.to_string(),
                message,
                request_id: request_id.clone(),
            }),
            meta: Some(Meta {
                trace_id: Some(request_id),
                ..Default::default()
            }),
        };

        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<Json<ApiResponse<T>>, ApiError>;

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        ApiError::Internal(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    // ── Constructor helpers ───────────────────────────────────────

    #[test]
    fn bad_request_constructor() {
        match ApiError::bad_request("invalid input") {
            ApiError::BadRequest(msg) => assert_eq!(msg, "invalid input"),
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[test]
    fn unauthorized_constructor() {
        match ApiError::unauthorized("no token") {
            ApiError::Unauthorized(msg) => assert_eq!(msg, "no token"),
            other => panic!("expected Unauthorized, got: {other:?}"),
        }
    }

    #[test]
    fn forbidden_constructor() {
        match ApiError::forbidden("access denied") {
            ApiError::Forbidden(msg) => assert_eq!(msg, "access denied"),
            other => panic!("expected Forbidden, got: {other:?}"),
        }
    }

    #[test]
    fn not_found_constructor() {
        match ApiError::not_found("missing resource") {
            ApiError::NotFound(msg) => assert_eq!(msg, "missing resource"),
            other => panic!("expected NotFound, got: {other:?}"),
        }
    }

    #[test]
    fn internal_constructor() {
        match ApiError::internal("something broke") {
            ApiError::Internal(msg) => assert_eq!(msg, "something broke"),
            other => panic!("expected Internal, got: {other:?}"),
        }
    }

    // ── as_tuple status codes ────────────────────────────────────

    #[test]
    fn as_tuple_unauthorized() {
        let (status, code, _) = ApiError::Unauthorized("x".into()).as_tuple();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(code, "UNAUTHORIZED");
    }

    #[test]
    fn as_tuple_forbidden() {
        let (status, code, _) = ApiError::Forbidden("x".into()).as_tuple();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "FORBIDDEN");
    }

    #[test]
    fn as_tuple_bad_request() {
        let (status, code, _) = ApiError::BadRequest("x".into()).as_tuple();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "BAD_REQUEST");
    }

    #[test]
    fn as_tuple_not_found() {
        let (status, code, _) = ApiError::NotFound("x".into()).as_tuple();
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "RESOURCE_NOT_FOUND");
    }

    #[test]
    fn as_tuple_conflict() {
        let (status, code, _) = ApiError::Conflict("x".into()).as_tuple();
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "CONFLICT");
    }

    #[test]
    fn as_tuple_internal() {
        let (status, code, _) = ApiError::Internal("x".into()).as_tuple();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(code, "INTERNAL_SERVER_ERROR");
    }

    // ── Display / Error trait ────────────────────────────────────

    #[test]
    fn display_format_includes_message() {
        let err = ApiError::NotFound("capsule 42".into());
        let msg = format!("{err}");
        assert!(msg.contains("capsule 42"), "got: {msg}");
        assert!(msg.contains("not found"), "got: {msg}");
    }

    #[test]
    fn display_format_for_each_variant() {
        let cases = [
            (ApiError::Unauthorized("a".into()), "unauthorized"),
            (ApiError::Forbidden("b".into()), "forbidden"),
            (ApiError::BadRequest("c".into()), "bad request"),
            (ApiError::NotFound("d".into()), "not found"),
            (ApiError::Conflict("e".into()), "conflict"),
            (ApiError::Internal("f".into()), "internal server error"),
        ];
        for (err, expected_prefix) in cases {
            let msg = format!("{err}");
            assert!(
                msg.contains(expected_prefix),
                "expected '{expected_prefix}' in '{msg}'"
            );
        }
    }

    #[test]
    fn error_trait_implemented() {
        let err = ApiError::Internal("boom".into());
        let _: &dyn std::error::Error = &err;
    }

    // ── IntoResponse ─────────────────────────────────────────────

    #[tokio::test]
    async fn into_response_status_codes() {
        let cases: Vec<(ApiError, StatusCode)> = vec![
            (ApiError::unauthorized("u"), StatusCode::UNAUTHORIZED),
            (ApiError::forbidden("f"), StatusCode::FORBIDDEN),
            (ApiError::bad_request("b"), StatusCode::BAD_REQUEST),
            (ApiError::not_found("n"), StatusCode::NOT_FOUND),
            (ApiError::internal("i"), StatusCode::INTERNAL_SERVER_ERROR),
        ];

        for (err, expected_status) in cases {
            let response = err.into_response();
            assert_eq!(response.status(), expected_status);
        }
    }

    #[tokio::test]
    async fn into_response_body_structure() {
        let err = ApiError::BadRequest("bad field".into());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["success"].as_bool(), Some(false));
        assert!(json["data"].is_null());
        assert_eq!(json["error"]["code"].as_str(), Some("BAD_REQUEST"));
        assert_eq!(json["error"]["message"].as_str(), Some("bad field"));
        assert!(json["error"]["request_id"].as_str().is_some());
        assert!(json["meta"]["trace_id"].as_str().is_some());
    }

    #[tokio::test]
    async fn into_response_conflict_body() {
        let err = ApiError::Conflict("duplicate key".into());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"].as_str(), Some("CONFLICT"));
        assert_eq!(json["error"]["message"].as_str(), Some("duplicate key"));
    }

    // ── From<anyhow::Error> ──────────────────────────────────────

    #[test]
    fn from_anyhow_error() {
        let anyhow_err = anyhow::anyhow!("unexpected failure");
        let api_err: ApiError = anyhow_err.into();
        match api_err {
            ApiError::Internal(msg) => assert!(msg.contains("unexpected failure")),
            other => panic!("expected Internal, got: {other:?}"),
        }
    }

    // ── ApiResult type alias ─────────────────────────────────────

    #[test]
    fn api_result_type_compiles() {
        fn _dummy() -> ApiResult<String> {
            Err(ApiError::not_found("nope"))
        }
        assert!(_dummy().is_err());
    }
}
