use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::api::errors::ApiError;
use crate::api::models::{Claims, RequestContext, UserRole};

/// Middleware that validates JWTs and injects the user context.
pub async fn auth_middleware(mut req: Request<Body>, next: Next) -> Result<Response, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    req.extensions_mut().insert(RequestContext {
        request_id: request_id.clone(),
    });

    if is_public_path(req.uri().path()) {
        return Ok(next.run(req).await);
    }

    let token = extract_bearer(&req)?;

    // In debug builds allow a dev override token for rapid local testing.
    if let Some(dev_claims) = try_dev_override(&token) {
        req.extensions_mut().insert(dev_claims);
        return Ok(next.run(req).await);
    }

    let claims = decode_token(&token)?;
    req.extensions_mut().insert(claims);

    Ok(next.run(req).await)
}

/// Ensure that the caller has one of the allowed roles.
pub fn assert_role(claims: &Claims, allowed: &[UserRole]) -> Result<(), ApiError> {
    if allowed.iter().any(|role| role == &claims.role) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "insufficient permissions for this action",
        ))
    }
}

fn extract_bearer(req: &Request<Body>) -> Result<String, ApiError> {
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("missing Authorization header"))?;

    let token = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .ok_or_else(|| ApiError::unauthorized("invalid bearer token format"))?;

    Ok(token.to_string())
}

fn decode_token(token: &str) -> Result<Claims, ApiError> {
    let secret = jwt_secret()?;
    let validation = Validation::new(Algorithm::HS256);
    decode::<Claims>(token, &DecodingKey::from_secret(&secret), &validation)
        .map(|data| {
            debug!("validated claims for subject {}", data.claims.sub);
            data.claims
        })
        .map_err(|err| {
            warn!("token validation failed: {err}");
            ApiError::unauthorized("invalid or expired token")
        })
}

fn jwt_secret() -> Result<Vec<u8>, ApiError> {
    let secret_vars = ["JWT_SECRET", "SPACE_JWT_SECRET"];
    for key in secret_vars {
        if let Ok(secret) = std::env::var(key) {
            if !secret.is_empty() {
                return Ok(secret.into_bytes());
            }
        }
    }

    if let Ok(hex_key) = std::env::var("GOSSIP_SIGNING_KEY") {
        if let Ok(bytes) = hex::decode(hex_key) {
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
    }

    if cfg!(debug_assertions) {
        return Ok(b"dev-secret".to_vec());
    }

    Err(ApiError::unauthorized(
        "JWT secret not configured; set JWT_SECRET or GOSSIP_SIGNING_KEY",
    ))
}

fn is_public_path(path: &str) -> bool {
    path.ends_with("/system/health")
        || path.starts_with("/swagger-ui")
        || path.starts_with("/api-doc")
}

fn try_dev_override(token: &str) -> Option<Claims> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let god =
        std::env::var("SPACE_DEV_GOD_TOKEN").unwrap_or_else(|_| "space-god-token".to_string());
    if token == god {
        return Some(Claims {
            sub: "god".to_string(),
            role: UserRole::Admin,
            exp: usize::MAX,
            iat: None,
            scope: None,
            iss: Some("space-dev-auth".to_string()),
        });
    }
    None
}
