use axum::{
    body::Body,
    extract::State,
    http::{header::AUTHORIZATION, Request},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::api::errors::ApiError;
use crate::api::models::{Claims, RequestContext, UserRole};
use crate::state::AppState;

/// Middleware that validates JWTs and injects the user context.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    req.extensions_mut().insert(RequestContext {
        request_id: request_id.clone(),
    });
    state.api_requests_total.inc();

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request};
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::sync::Mutex;

    // Env-var tests must run serially since they mutate process-global state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── is_public_path ────────────────────────────────────────────

    #[test]
    fn public_path_system_health() {
        assert!(is_public_path("/api/v1/system/health"));
        assert!(is_public_path("/v1/system/health"));
        assert!(is_public_path("/system/health"));
    }

    #[test]
    fn public_path_swagger_ui() {
        assert!(is_public_path("/swagger-ui"));
        assert!(is_public_path("/swagger-ui/index.html"));
    }

    #[test]
    fn public_path_api_doc() {
        assert!(is_public_path("/api-doc"));
        assert!(is_public_path("/api-doc/openapi.json"));
    }

    #[test]
    fn non_public_paths() {
        assert!(!is_public_path("/api/v1/mesh/peers"));
        assert!(!is_public_path("/api/v1/data/objects"));
        assert!(!is_public_path("/api/v1/gossip/stats"));
        assert!(!is_public_path("/"));
        assert!(!is_public_path(""));
    }

    // ── extract_bearer ────────────────────────────────────────────

    fn make_request_with_auth(value: &str) -> Request<Body> {
        Request::builder()
            .header(AUTHORIZATION, value)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn extract_bearer_uppercase_prefix() {
        let req = make_request_with_auth("Bearer my-token-123");
        let token = extract_bearer(&req).unwrap();
        assert_eq!(token, "my-token-123");
    }

    #[test]
    fn extract_bearer_lowercase_prefix() {
        let req = make_request_with_auth("bearer lower-token");
        let token = extract_bearer(&req).unwrap();
        assert_eq!(token, "lower-token");
    }

    #[test]
    fn extract_bearer_missing_header() {
        let req = Request::builder().body(Body::empty()).unwrap();
        let err = extract_bearer(&req).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing Authorization"), "got: {msg}");
    }

    #[test]
    fn extract_bearer_wrong_scheme() {
        let req = make_request_with_auth("Basic dXNlcjpwYXNz");
        let err = extract_bearer(&req).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid bearer"), "got: {msg}");
    }

    // ── assert_role ───────────────────────────────────────────────

    fn test_claims(role: UserRole) -> Claims {
        Claims {
            sub: "test-user".to_string(),
            role,
            exp: usize::MAX,
            iat: None,
            scope: None,
            iss: None,
        }
    }

    #[test]
    fn assert_role_allows_matching_role() {
        let claims = test_claims(UserRole::Admin);
        assert!(assert_role(&claims, &[UserRole::Admin]).is_ok());
    }

    #[test]
    fn assert_role_allows_any_from_list() {
        let claims = test_claims(UserRole::Viewer);
        assert!(assert_role(
            &claims,
            &[UserRole::Admin, UserRole::Editor, UserRole::Viewer]
        )
        .is_ok());
    }

    #[test]
    fn assert_role_rejects_non_matching_role() {
        let claims = test_claims(UserRole::Viewer);
        let err = assert_role(&claims, &[UserRole::Admin]).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("insufficient permissions"), "got: {msg}");
    }

    #[test]
    fn assert_role_rejects_empty_allowed_list() {
        let claims = test_claims(UserRole::Admin);
        assert!(assert_role(&claims, &[]).is_err());
    }

    // ── jwt_secret ────────────────────────────────────────────────

    #[test]
    fn jwt_secret_from_jwt_secret_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        std::env::set_var("JWT_SECRET", "test-secret-123");
        let secret = jwt_secret().unwrap();
        assert_eq!(secret, b"test-secret-123");
        std::env::remove_var("JWT_SECRET");
    }

    #[test]
    fn jwt_secret_from_space_jwt_secret_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        std::env::set_var("SPACE_JWT_SECRET", "space-secret-456");
        let secret = jwt_secret().unwrap();
        assert_eq!(secret, b"space-secret-456");
        std::env::remove_var("SPACE_JWT_SECRET");
    }

    #[test]
    fn jwt_secret_from_gossip_signing_key_hex() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        std::env::set_var("GOSSIP_SIGNING_KEY", "deadbeef");
        let secret = jwt_secret().unwrap();
        assert_eq!(secret, vec![0xde, 0xad, 0xbe, 0xef]);
        std::env::remove_var("GOSSIP_SIGNING_KEY");
    }

    #[test]
    fn jwt_secret_priority_jwt_secret_first() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        std::env::set_var("JWT_SECRET", "first");
        std::env::set_var("SPACE_JWT_SECRET", "second");
        let secret = jwt_secret().unwrap();
        assert_eq!(secret, b"first");
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
    }

    #[test]
    fn jwt_secret_skips_empty_values() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        std::env::set_var("JWT_SECRET", "");
        std::env::set_var("SPACE_JWT_SECRET", "fallback");
        let secret = jwt_secret().unwrap();
        assert_eq!(secret, b"fallback");
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
    }

    // ── decode_token ──────────────────────────────────────────────

    fn make_token(claims: &Claims, secret: &[u8]) -> String {
        encode(
            &Header::default(),
            claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    #[test]
    fn decode_token_valid() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        let secret = b"test-decode-secret";
        std::env::set_var("JWT_SECRET", "test-decode-secret");

        let original = test_claims(UserRole::Editor);
        let token = make_token(&original, secret);
        let decoded = decode_token(&token).unwrap();
        assert_eq!(decoded.sub, "test-user");
        assert_eq!(decoded.role, UserRole::Editor);
        std::env::remove_var("JWT_SECRET");
    }

    #[test]
    fn decode_token_invalid_signature() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        std::env::set_var("JWT_SECRET", "correct-secret");

        let claims = test_claims(UserRole::Viewer);
        let token = make_token(&claims, b"wrong-secret");
        let err = decode_token(&token).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid or expired"), "got: {msg}");
        std::env::remove_var("JWT_SECRET");
    }

    #[test]
    fn decode_token_expired() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        let secret = b"expiry-test-secret";
        std::env::set_var("JWT_SECRET", "expiry-test-secret");

        let mut claims = test_claims(UserRole::Admin);
        claims.exp = 0; // expired in the past
        let token = make_token(&claims, secret);
        let err = decode_token(&token).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid or expired"), "got: {msg}");
        std::env::remove_var("JWT_SECRET");
    }

    #[test]
    fn decode_token_garbage_input() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("JWT_SECRET");
        std::env::remove_var("SPACE_JWT_SECRET");
        std::env::remove_var("GOSSIP_SIGNING_KEY");

        std::env::set_var("JWT_SECRET", "some-secret");
        let err = decode_token("not.a.valid.jwt").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid or expired"), "got: {msg}");
        std::env::remove_var("JWT_SECRET");
    }

    // ── try_dev_override ──────────────────────────────────────────

    #[test]
    fn try_dev_override_no_env_var() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("SPACE_DEV_GOD_TOKEN");
        assert!(try_dev_override("anything").is_none());
    }

    #[test]
    fn try_dev_override_empty_env_var() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPACE_DEV_GOD_TOKEN", "");
        assert!(try_dev_override("anything").is_none());
        std::env::remove_var("SPACE_DEV_GOD_TOKEN");
    }

    #[test]
    fn try_dev_override_matching_token() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPACE_DEV_GOD_TOKEN", "god-mode-token");

        if cfg!(debug_assertions) {
            let claims = try_dev_override("god-mode-token").expect("should return dev claims");
            assert_eq!(claims.sub, "god");
            assert_eq!(claims.role, UserRole::Admin);
            assert_eq!(claims.iss.as_deref(), Some("space-dev-auth"));
        } else {
            assert!(try_dev_override("god-mode-token").is_none());
        }
        std::env::remove_var("SPACE_DEV_GOD_TOKEN");
    }

    #[test]
    fn try_dev_override_non_matching_token() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPACE_DEV_GOD_TOKEN", "god-mode-token");
        assert!(try_dev_override("wrong-token").is_none());
        std::env::remove_var("SPACE_DEV_GOD_TOKEN");
    }
}
