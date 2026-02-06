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

    // In debug builds, generate a random ephemeral secret and warn loudly.
    // This avoids a hardcoded default that could be exploited if debug builds leak.
    if cfg!(debug_assertions) {
        warn!("JWT secret not configured; using random ephemeral secret (debug build only). Set JWT_SECRET for stable tokens.");
        let mut secret = vec![0u8; 32];
        // Use system randomness; fall back to timestamp-based seed if unavailable.
        if let Ok(bytes) =
            std::fs::read("/dev/urandom").map(|b| b.into_iter().take(32).collect::<Vec<_>>())
        {
            if bytes.len() == 32 {
                secret = bytes;
            }
        } else {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            std::time::SystemTime::now().hash(&mut h);
            std::process::id().hash(&mut h);
            let hash = h.finish().to_le_bytes();
            secret[..8].copy_from_slice(&hash);
        }
        return Ok(secret);
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
    // Require SPACE_DEV_GOD_TOKEN to be explicitly set — no hardcoded default.
    let god = match std::env::var("SPACE_DEV_GOD_TOKEN") {
        Ok(val) if !val.is_empty() => val,
        _ => return None,
    };
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
