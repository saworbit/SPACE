# Dependency Security Policy

SPACE enforces deterministic, auditable builds for every crate in the workspace. This document captures the lifecycle for selecting, pinning, reviewing, and updating third‑party code, with special focus on cryptography as we roll out Phase 3 encryption.

## Goals
- Prevent supply-chain compromise through version pinning, reproducible builds, and continuous auditing.
- Ensure cryptographic dependencies uphold constant-time guarantees and align with `ENCRYPTION_IMPLEMENTATION.md`.
- Minimise dependency bloat while keeping license posture Apache-2.0 / MIT compatible.

## Workflow Summary
1. **Plan** – classify the dependency change (new crate, feature toggle, version bump) and map its tier.
2. **Pin** – update `Cargo.toml` with the approved version, add reviewer/timestamp comment, and sync `Cargo.lock`.
3. **Verify** – run `cargo tree --edges normal,build,dev`, `cargo audit --deny warnings`, `cargo deny check`, and feature allowlist validation.
4. **Review** – attach artefacts to the PR template (section below) and capture rationale in meeting notes if material.
5. **Monitor** – Dependabot/Renovate and nightly audits raise issues when advisories or drift are detected.

## Dependency Tiers

| Tier | Scope | Policy | Examples |
| --- | --- | --- | --- |
| **Tier 0** | Cryptography, hashing, compression, serialization, memory scrubbing | Exact version pin (`=`) in `Cargo.toml`, CT compliance review, side-channel notes required | `blake3`, `aes`, `xts-mode`, `serde`, `zstd`, `zeroize`, `subtle` |
| **Tier 1** | Platform scaffolding, IO stacks, observability, CLI | Caret pin to specific minor (`^x.y.z`) with audit log reference; must be covered by `cargo audit` gating | `tokio`, `axum`, `clap`, `tower`, `tracing` |
| **Tier 2** | Dev-only, fuzzing, benchmarks | Wildcard or caret allowed, but include justification in PR and keep out of release artefacts | `proptest`, `rand`, `criterion`, fuzz targets |

Record tier assignments inside PR descriptions and keep the table up to date when new crates land.

## Pinning & Metadata
- Comments in `Cargo.toml` use the format `# YYYY-MM-DD <initials>: <rationale>` and must reference this policy (example: `see docs/dependency-security.md#tier-0`).
- Update `Cargo.lock` via `cargo update -p <crate>` or `cargo update` after manifest edits; the change **must** be part of the same commit.
- Maintain the feature allowlist in `[workspace.metadata.space.allowed-features]`. Violations are caught by `cargo xtask audit`.
- Feature flags that gate Phase 3 crypto (`experimental`, `pqc`, `tee`) remain opt-in until a security review graduates them.

## Audit & Automation
- **CI Workflows**
  - `security-audit`: runs `cargo xtask audit` (format/check/audit/deny/bloat) on every push and pull request.
  - `dependency-drift`: nightly; emits issues when advisories appear, transitive count exceeds 50, or manifests diverge from policy.
- **Tooling**
  - `cargo audit --deny warnings` blocks merges on high/critical advisories.
  - `cargo deny check bans licenses sources` enforces Apache/MIT compatibility.
  - `cargo bloat --crates` runs in release mode and posts size regressions to PR logs.
- Results from the last green run are mirrored in `docs/security/audit-status.json` and surfaced via Slack.

## Crypto Review Rubric
- Require constant-time primitives (`subtle::ConstantTimeEq`, no `PartialEq` on secret data).
- Verify upstream uses hardware acceleration safely (`cpufeatures` gating, no runtime feature toggles that alter timing).
- For new algorithms, capture proofs or references in `ENCRYPTION_IMPLEMENTATION.md` and list reviewers.
- Key derivation must use HKDF/PBKDF2 with TPM-backed master secrets; ensure `keymanager.rs` aligns with policy.

## PR Checklist (add to description)
```
- [ ] Tier classification + reviewer initials recorded
- [ ] cargo tree --edges normal,build,dev attached (before/after)
- [ ] cargo audit --deny warnings
- [ ] cargo deny check bans licenses sources
- [ ] cargo bloat --crates (release) regression inspected
- [ ] Feature allowlist validated (cargo xtask audit)
- [ ] docs/security/audit-status.json updated (if audit run)
```

## Meetings & Governance
- A monthly “Dependency Review” triad (Security, Crypto, Core Eng) logs decisions in `docs/security/meetings/YYYY-MM.md`.
- Critical advisories trigger a release freeze, mitigation plan, and post-mortem within 72 hours.
- Mirror Tier 0 crates in a private registry and verify checksums when running `cargo vendor`.

## Reference Artefacts
- `docs/security/audit-status.json` – latest audit run metadata (tool versions, report hashes).
- `docs/security/meetings/` – minutes for monthly reviews and emergency sessions.
- `CONTRIBUTING.md` – contributor-facing checklist and policy summary.

Questions or proposals for new dependencies should be raised via GitHub Discussions under **Security & Dependencies** with a link to this policy.
