#!/bin/bash
set -euo pipefail

# Secret must match the web-interface validator (default: "dev-secret")
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
