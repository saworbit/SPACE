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

## Security Hardening Summary

The following security measures are implemented in the current codebase:

### Input Validation
- **Path traversal protection**: All object API endpoints (`upload`, `download`, `HEAD`) reject null bytes, backslashes, and `..` path components before any filesystem operations.
- **Deserialization size limits**: Replication frames are capped at 16 MiB before `bincode::deserialize` is called, preventing out-of-memory denial-of-service attacks.

### Secret Management
- **No hardcoded secrets**: JWT signing keys use random ephemeral generation in debug builds with a warning log. The dev god-token requires the `SPACE_DEV_GOD_TOKEN` environment variable to be explicitly set.
- **Signing key entropy**: Default orchestrator signing keys use OS-provided entropy (`/dev/urandom` on Linux) instead of zeroed byte arrays.
- **Master key isolation**: Production master keys are loaded from `SPACE_MASTER_KEY` (env) or `SPACE_MASTER_KEY_FILE` (file path), never hardcoded.

### Resilience
- **Mutex poisoning recovery**: Locks across 6 crates use `unwrap_or_else(|e| e.into_inner())` to recover data from poisoned mutexes rather than propagating panics through the system.
- **Descriptive panic messages**: All remaining `expect()` calls include context about what operation failed and suggested remediation.

### Transport Security
- **TLS configuration**: `TlsConfig` struct supports env-based mTLS configuration (`SPACE_TLS_CA_CERT`, `SPACE_TLS_CERT`, `SPACE_TLS_KEY`) for inter-node transport. A warning is logged when TLS is not configured.
- **Integrity verification**: All replicated segments include BLAKE3-MAC tags verified using constant-time comparison.

### Encryption
- **XTS-AES-256**: Per-segment encryption with deterministic tweaks preserving deduplication.
- **BLAKE3-MAC**: Keyed integrity tags on every segment, validated on read.
- **Key rotation**: Version-tracked key derivation with BLAKE3-KDF.
- **Memory zeroization**: Keys are zeroized on drop via the `zeroize` crate.
- **Post-quantum readiness**: Optional ML-KEM/Kyber hybrid key wrapping (`advanced-security` feature).

### Known Limitations
- TLS stream wrapping is infrastructure-only (config + env loading); actual `tokio-rustls` integration is pending.
- Security features have not been professionally audited. Do not use in production without an independent review.
- The eBPF/SPIFFE gateway is experimental and needs validation.
- Post-quantum crypto (Kyber hybrid) is untested in production scenarios.
