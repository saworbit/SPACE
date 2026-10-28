# Contributing to SPACE

Thank you for helping harden SPACE. This document highlights day-to-day expectations with an emphasis on dependency hygiene and security auditing.

## Current Focus: v0.2 "Core Capsule"

SPACE is a single-node capsule store. See [MVP_SCOPE.md](MVP_SCOPE.md) and [RESET.md](RESET.md). Contributions should stay inside that core. The mesh, extra protocol views, and other experimental crates are on `archive/pre-reset-2026-10` and are not being revived on this branch.

### Good First Issues

These are high-impact areas where new contributors can make meaningful progress:

1. **Expand property-based tests** — `crates/capsule-registry/tests/proptest_pipeline.rs` covers round-trip, dedup, content separation, and segment-boundary cases. Open invariants: MAC verification fails on tampered ciphertext, compression entropy detection skips random data
2. **Pipeline benchmarks** — Add local benchmarks for the segment write path. There is no benchmark workflow on this branch.
3. **Documentation alignment** — Pages under `docs/` still describe the pre-reset tree. Prefer updates that match what `cargo test --workspace` actually runs.
4. **`spacectl` UX** — Add progress bars for large operations, improve error messages, add `--json` output mode
5. **Metrics** — Nothing emits metrics yet. Add them: per-stage latency histograms, dedup hit rates, GC effectiveness
6. **Structured error types** — Replace string errors with typed errors that include recovery guidance
7. **Background GC** — Help make garbage collection automatic with tunable aggressiveness

## Licensing

SPACE is dual-licensed under **MIT OR Apache 2.0**. This means:

- **All contributions will be licensed under the same terms** (MIT OR Apache 2.0)
- By submitting a pull request, you agree to license your contributions under both licenses
- Users can choose which license they prefer when using SPACE
- This follows the same approach as the Rust programming language and many other Rust projects

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for the full license texts.

## Getting Started
- Install the latest stable Rust toolchain (`rustup default stable`). The committed `Cargo.lock` needs Rust 1.89 or newer.
- Run `cargo test --workspace` before opening a pull request. That is what CI runs.
- Property tests live in `crates/capsule-registry/tests/proptest_pipeline.rs`. Run them with the workspace tests.
- [ARCHITECTURE.md](ARCHITECTURE.md) and [RESET.md](RESET.md) describe this branch. The pre-reset design docs are on `archive/pre-reset-2026-10`.

## Automated checks

CI on pull requests runs the checks below (see `.github/workflows/ci.yml`).

| Check | Command |
|-------|---------|
| Formatting | `cargo fmt --all -- --check` |
| Clippy | `cargo clippy --workspace --all-targets -- -D warnings` |
| Workspace tests | `cargo test --workspace` |
| Build on rust-version | `cargo +1.89 check --workspace --locked` |
| License field | `license = "MIT OR Apache-2.0"` in the root `Cargo.toml` |

## Dependency Changes
Any modification to `Cargo.toml`, `Cargo.lock`, or enabled features must satisfy the workflow in `docs/dependency-security.md`.

**Checklist (include in PR description)**
- [ ] Say why the dependency is needed. Pin cryptography and serialization crates to an exact version (`=x.y.z`), like the existing ones in the root `Cargo.toml`.
- [ ] Attach `cargo tree --edges normal,build,dev` diff (before/after).
- [ ] Run `cargo test --workspace`.
- [ ] Run `cargo audit` and `cargo deny check` when a dependency changes.

There is no `xtask` crate on this branch.

## Review Expectations
- Check that new cryptography and serialization dependencies are pinned to exact versions.
- Confirm `cargo test --workspace` succeeded.
- Reject PRs that introduce prohibited licenses.
- Triage Dependabot PRs weekly (configurable via `.github/dependabot.yml`); do not merge without full audit artefacts.

## Dependabot Triage

When triaging Dependabot PRs (see also [`docs/dependency-security.md`](docs/dependency-security.md)):

1. **Green CI, patch bump** — merge directly after a quick review.
2. **Major version bumps** — check for breaking API changes. Dependabot cannot fix code; create a manual migration branch if needed.
3. **Coupled crates** (axum, tonic, tokio) — close individual PRs and coordinate a single migration when the bump is breaking.
4. **Verify crate legitimacy** — check `lib.rs` for protest/squatter crates and inspect transitive dependency diffs for supply-chain substitutions.
5. **Stale PRs** (changes already on main) — close promptly.

## Secure Coding Practices
- **No hardcoded secrets.** Never commit API keys, tokens, or signing keys. Use environment variables or file-based providers. Debug fallbacks must use random ephemeral values.
- **Validate all external input.** Object key paths must reject null bytes, backslashes, and `..` traversal components. Deserialize untrusted data only after size-limit checks.
- **Prefer poison-safe locks.** Use `.unwrap_or_else(|e| e.into_inner())` for `Mutex`/`RwLock` instead of `.unwrap()` to avoid cascading panics from poisoned locks. Use `.map_err()` when the caller can surface the error.
- **Minimize lock scope.** Acquire keys or shared state, clone, and drop the lock guard before performing expensive operations (encryption, MAC verification, I/O).
- **Descriptive expect messages.** Every `expect()` call must describe what operation failed and suggest remediation (e.g., `"failed to open audit log; check path and permissions"`).
- **Use `&'static str` for fixed strings.** Functions returning a fixed set of strings (like MIME type detection) should return `&'static str` to avoid per-call heap allocations.

## Crypto changes
- After modifying cryptography or compression, run `cargo test --workspace`, including `crates/capsule-registry/tests/proptest_pipeline.rs`.
- Avoid data-dependent branching or early returns on sensitive comparisons; rely on helpers wired with `subtle::ConstantTimeEq`.

## Security Escalations
- Critical advisories require a release freeze, mitigation plan, and post-mortem within 72 hours.
- File emergency findings under `docs/security/meetings/<YYYY-MM>.md` and link to the GitHub issue or advisory.

For questions, open a GitHub Discussion tagged **Security & Dependencies** or ping the #space-security channel on Slack.
