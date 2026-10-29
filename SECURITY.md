# Security Policy

## Reporting a Vulnerability

**Do not file a public issue for security vulnerabilities.**

Use GitHub's [private vulnerability reporting](https://github.com/saworbit/space/security/advisories/new)
or email security@adaptive-storage.dev.

We aim to acknowledge reports within 48 hours and provide a fix timeline within
7 days.

## Severity Classification & Response

| Severity | Examples | Embargo | Target Fix |
|----------|---------|---------|------------|
| **Critical** | Remote code execution, key material leak, auth bypass | Up to 90 days | 72 hours |
| **High** | Privilege escalation, data corruption, crypto weakness | Up to 60 days | 7 days |
| **Medium** | Denial of service, information disclosure | Up to 30 days | 14 days |
| **Low** | Minor info leak, hardening gap | No embargo | Next release |

## Embargo Policy

For Critical and High severity issues:

1. Fixes are developed in a private branch (not pushed to `main` until disclosure).
2. The reporter is consulted on the Coordinated Release Date (CRD).
3. Maximum embargo period is 90 days from acknowledgment.
4. Disclosure dates avoid Fridays and public holidays.
5. Once the fix is released, a security advisory is published with CVE (if applicable).

## Supported Versions

Only the latest `main` branch receives security updates.

## What this branch protects, and what it doesn't

This describes the post-reset `main` branch. None of it has been professionally audited; don't use SPACE in production without an independent review.

### Encryption at rest

- Segments can be encrypted with XTS-AES-256 and authenticated with a 16-byte BLAKE3 keyed MAC. The MAC is checked in constant time before decryption.
- Per-version keys are derived from a 32-byte master key with HKDF-HMAC-SHA256 and zeroized on drop. See [ENCRYPTION_IMPLEMENTATION.md](docs/implementation/ENCRYPTION_IMPLEMENTATION.md).
- `spacectl create` and S3 PUT encrypt by default when `SPACE_MASTER_KEY` is set. `spacectl create --policy-file` picks another policy.
- The master key comes from `SPACE_MASTER_KEY` (64 hex characters). `SPACE_MASTER_KEY_FILE` isn't supported; the library API does have a `FileKeyProvider`.
- A malformed `SPACE_MASTER_KEY` is a startup error. With no key set, writes whose policy asks for encryption fail.
- Encryption is deterministic so that dedup keeps working. Identical segments produce identical ciphertext, which reveals that they're equal.
- Key rotation isn't persisted across restarts ([#98](https://github.com/saworbit/SPACE/issues/98)).

### Integrity

- Every segment is verified on read: encrypted ones by MAC, unencrypted ones by content hash.
- Whether a segment counts as encrypted is a flag in an unauthenticated JSON sidecar. A capsule whose policy requires encryption refuses a segment marked unencrypted.
- Deep scrub verifies content hashes and MACs when it runs, but nothing runs it automatically ([#97](https://github.com/saworbit/SPACE/issues/97)).

### Network exposure

- `spacectl serve-s3` has no authentication. It listens on 127.0.0.1 unless started with `--bind`, and sends no CORS headers, so web pages on other origins can't call it. Anyone who can reach the port can read, write and delete objects.
- There's no TLS.
- S3 request and response bodies are buffered in memory, so large or concurrent transfers can exhaust it ([#99](https://github.com/saworbit/SPACE/issues/99)).

### Code practices

- Lock acquisitions in `storage`, `pipeline`, `protocol-s3` and `capsule-registry` mostly recover from poisoned locks with `unwrap_or_else(|e| e.into_inner())`. `nvram-sim` still uses `.unwrap()`.
- Cryptographic and serialization dependencies are pinned to exact versions in the root `Cargo.toml`, and `Cargo.lock` is committed.

### Removed in the reset

These belonged to crates removed in the 2026-10-19 reset and are on `archive/pre-reset-2026-10`:

- object API path validation;
- replication frame size limits;
- JWT and token auth, and orchestrator signing keys;
- inter-node TLS;
- the eBPF gateway;
- the post-quantum key wrap.
