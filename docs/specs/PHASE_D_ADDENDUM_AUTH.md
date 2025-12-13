# SPACE Architecture & Design Specification

## Title: Phase D Addendum: Authentication & Token Management

- **Target Components:** `crates/web-interface`, `crates/spacectl`, `scripts`
- **Status:** Approved for Implementation
- **Prerequisites:** Phase D (Views) in progress
- **Related Documents:** `docs/specs/PHASE_D_VIEWS_AND_FEDERATION.md`

---

## 1. Executive Summary

### Problem
Phase D introduces powerful control plane capabilities (`spacectl project`, `federate`). These APIs must be protected, but developers need a frictionless way to generate valid JWTs for local testing and CI without running a full OIDC provider.

### Solution
- **Standardized Schema:** Define the specific JWT claims required by SPACE (`sub`, `scope`, `role`).
- **Dev Tooling:** A `scripts/dev_auth.sh` utility to mint valid tokens using a shared secret.
- **Mock Seeding:** A development-mode override in the web interface to accept a known “God Token” for quick local testing.

---

## 2. JWT Schema Specification

### Algorithms
- **Development:** HS256 (HMAC)
- **Production:** RS256 (RSA) – future rollout

### Required Claims

| Claim | Example | Description |
|-------|---------|-------------|
| `iss` | `space-dev-auth` | Issuer identity |
| `sub` | `user-123` | Subject (User or Service Account UUID) |
| `exp` | `1735689600` | Expiration (Unix timestamp) |
| `role` | `admin` | Authorization level (`admin`, `editor`, `viewer`) |
| `scope` | `capsule:read capsule:write` | Fine-grained permissions (optional today) |

### Example Payload
```json
{
  "iss": "space-dev-auth",
  "sub": "dev-user-01",
  "role": "admin",
  "exp": 9999999999
}
```

---

## 3. Implementation: Token Generation Utility

### `scripts/dev_auth.sh`
```bash
#!/bin/bash
set -euo pipefail

# Secret must match the server (default: "dev-secret")
SECRET="${SPACE_JWT_SECRET:-dev-secret}"
ISSUER="${SPACE_JWT_ISSUER:-space-dev-auth}"
SUBJECT="${SPACE_JWT_SUBJECT:-dev-admin}"
ROLE="${SPACE_JWT_ROLE:-admin}"
EXP="${SPACE_JWT_EXP:-9999999999}"

header='{"alg":"HS256","typ":"JWT"}'
payload=$(cat <<EOF
{"iss":"${ISSUER}","sub":"${SUBJECT}","role":"${ROLE}","exp":${EXP}}
EOF
)

base64_url_encode() {
  openssl enc -base64 -A | tr '+/' '-_' | tr -d '='
}

sign() {
  echo -n "$1" | openssl dgst -sha256 -hmac "$SECRET" -binary | base64_url_encode
}

b64_header=$(echo -n "$header" | base64_url_encode)
b64_payload=$(echo -n "$payload" | base64_url_encode)
signature=$(sign "${b64_header}.${b64_payload}")

echo "${b64_header}.${b64_payload}.${signature}"
```

---

## 4. Implementation: Server-Side Verification

### Development “God Token”
- In debug builds, the web interface accepts a fixed bearer token for rapid testing:
  - Header: `Authorization: Bearer space-god-token`
  - Override: `SPACE_DEV_GOD_TOKEN` to set a custom value
  - Injected claims: `sub = "god"`, `role = "admin"`, `exp = usize::MAX`

### JWT Validation
- HS256 using `JWT_SECRET` or `SPACE_JWT_SECRET` (development default: `dev-secret`).
- Claims enforced:
  - `role` must map to `admin` / `editor` / `viewer`
  - Expiration is validated by `jsonwebtoken`

---

## 5. Updates to `spacectl`

- CLI accepts a bearer token via `--token` or `SPACE_AUTH_TOKEN`.
- Future HTTP calls should set `Authorization: Bearer <token>` using this value (reserved for upcoming control-plane actions).

---

## 6. Integration Guide

1) **Generate Token**  
```bash
./scripts/dev_auth.sh > .token
```

2) **Run Server**  
```bash
JWT_SECRET=dev-secret cargo run -p web-interface
```

3) **Use CLI**  
```bash
export SPACE_AUTH_TOKEN=$(cat .token)
spacectl ... # future control-plane calls will reuse this token
```

4) **Quick Mock Testing (Dev only)**  
Send `Authorization: Bearer space-god-token` to bypass signing during local debug runs.

---

## 7. Next Steps
- Consider RS256 support with JWKS for production.
- Expand `scope` enforcement in handlers once fine-grained authorization is required.
