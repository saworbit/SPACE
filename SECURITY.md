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
- A store remembers the first key it was opened with, by fingerprint. Another key is a startup error. Without a key, its writes are refused and `serve-s3` won't start, unless `SPACE_ALLOW_PLAINTEXT=1` is set ([#157](https://github.com/saworbit/SPACE/issues/157)). `serve-s3` logs the key's fingerprint at startup.
- A malformed `SPACE_MASTER_KEY` is a startup error. With no key set, writes whose policy asks for encryption fail, and so do reads of encrypted capsules: `spacectl read` exits non-zero and S3 GET returns `InternalError`, rather than serving the ciphertext ([#155](https://github.com/saworbit/SPACE/issues/155)).
- Encryption is deterministic so that dedup keeps working. Identical segments produce identical ciphertext, which reveals that they're equal.
- Key rotation isn't persisted across restarts ([#98](https://github.com/saworbit/SPACE/issues/98)).

### Integrity

- Every segment is verified on read: encrypted ones by MAC, unencrypted ones by content hash.
- Whether a segment counts as encrypted is a flag in the segment's descriptor in the data log, protected by a checksum but not a MAC. A capsule whose policy requires encryption refuses a segment marked unencrypted.
- An S3 PUT whose body doesn't match its `Content-MD5`, `x-amz-checksum-*` or hex `x-amz-content-sha256` header is refused before anything is stored ([#196](https://github.com/saworbit/SPACE/issues/196)). A PUT asking for Object Lock, server-side encryption options, tags or ACLs is refused, since the server can't provide them.
- `serve-s3` scrubs the store in the background. A daily light scrub checks the checksums of each segment's record, in the data log or in its file under `SPACE_STORAGE_ROOT`. That catches media damage but not tampering, since the checksums aren't keyed. A weekly deep scrub verifies content hashes and MACs, the latter only when `SPACE_MASTER_KEY` is set. `spacectl scrub [--deep]` runs a pass on demand. When each segment was last checked is kept in `space.db`, so a restart doesn't reset the schedule.

### Network exposure

- `spacectl serve-s3` has no authentication. It listens on 127.0.0.1 and ::1 unless started with `--bind`, and warns at startup when it isn't on loopback. It answers only requests whose `Host` header names `localhost`, an IP address, or a name given with `--allow-host`, so a web page that points its own name at this machine (DNS rebinding) can't reach it. It refuses requests that carry an `Origin` header for any other origin than the one they're sent to, so a page on another site can't make the browser send one, not even a DeleteObjects POST, which a browser sends without asking first. It sends no CORS headers ([#200](https://github.com/saworbit/SPACE/issues/200)). Anyone who can reach the port with an S3 client can read, write and delete objects.
- There's no TLS.

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
