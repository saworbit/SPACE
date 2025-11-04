# Contributing to SPACE

Thank you for helping harden SPACE. This document highlights day-to-day expectations with an emphasis on dependency hygiene and security auditing.

## Getting Started
- Install the latest stable Rust toolchain (`rustup default stable`).
- Run `cargo xtask audit` before opening a pull request to execute formatting, checks, and security tooling in one pass.
- Follow the coding standards in `docs/architecture.md` and module-specific guides such as `ENCRYPTION_IMPLEMENTATION.md`.

## Dependency Changes
Any modification to `Cargo.toml`, `Cargo.lock`, or enabled features must satisfy the workflow in `docs/dependency-security.md`.

**Checklist (include in PR description)**
- [ ] Identify Tier (0/1/2) for each change and record reviewer initials with date in `Cargo.toml` comment.
- [ ] Attach `cargo tree --edges normal,build,dev` diff (before/after).
- [ ] Run `cargo audit --deny warnings`.
- [ ] Run `cargo deny check bans licenses sources`.
- [ ] Run `cargo bloat --crates --release` and record notable regressions.
- [ ] Run `cargo xtask audit` (enforces feature allowlist, fmt, clippy, tests).
- [ ] Update `docs/security/audit-status.json` if this PR contains the latest successful audit run.

Pull requests lacking the artefacts above will be blocked until they comply.

## Review Expectations
- Validate dependency tiering and ensure comments follow the `YYYY-MM-DD <initials>` format.
- Confirm CI `security-audit` workflow succeeded and review the posted summaries.
- Reject PRs that introduce prohibited licenses or push the transitive dependency count beyond 50 without an approved waiver.

## Security Escalations
- Critical advisories require a release freeze, mitigation plan, and post-mortem within 72 hours.
- File emergency findings under `docs/security/meetings/<YYYY-MM>.md` and link to the GitHub issue or advisory.

For questions, open a GitHub Discussion tagged **Security & Dependencies** or ping the #space-security channel on Slack.
