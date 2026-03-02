# Contributing to SPACE

Thank you for helping harden SPACE. This document highlights day-to-day expectations with an emphasis on dependency hygiene and security auditing.

## Licensing

SPACE is dual-licensed under **MIT OR Apache 2.0**. This means:

- **All contributions will be licensed under the same terms** (MIT OR Apache 2.0)
- By submitting a pull request, you agree to license your contributions under both licenses
- Users can choose which license they prefer when using SPACE
- This follows the same approach as the Rust programming language and many other Rust projects

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for the full license texts.

## Developer Certificate of Origin (DCO)

All commits must carry a `Signed-off-by` line certifying the
[Developer Certificate of Origin](https://developercertificate.org/). This is
enforced by CI.

Add the sign-off automatically with:

```bash
git commit -s -m "feat(storage): add zero-copy read path"
```

If you forget, amend your most recent commit:

```bash
git commit --amend -s --no-edit
```

## Getting Started
- Install the latest stable Rust toolchain (`rustup default stable`).
- Run `cargo xtask audit` before opening a pull request to execute formatting, checks, and security tooling in one pass.
- Generate dependency artefacts with `cargo xtask graph` when introducing new crates or features; attach resulting files if reviewers request them.
- Install `cargo fuzz` (`cargo install cargo-fuzz`) to exercise the fuzz harnesses when touching encryption or compression code.
- Follow the coding standards in `docs/architecture.md` and module-specific guides such as `docs/implementation/ENCRYPTION_IMPLEMENTATION.md`.

## 🤖 Automated Quality Gates

To maintain the high reliability standards of SPACE, the following automated checks must pass before merging:

| Check | Description | Command to Run Locally |
|-------|-------------|------------------------|
| **Fuzzing** | Checks for edge-case crashes | `cargo fuzz run <target>` |
| **Typos** | Validates spelling | `typos` (Install via `cargo install typos-cli`) |
| **Links** | Checks for broken URLs | `lychee .` (Install via `cargo install lychee`) |
| **Docs** | Ensures documentation compiles | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` |
| **Conventional Commits** | Enforces semantic PR titles | Use `feat:`, `fix:`, etc. in PR titles |
| **Benchmarks** | Checks for performance regressions | `cargo bench` |

### 🔍 Conventional Commits
All Pull Requests must follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.
* **Good:** `feat(storage): implement zero-copy read path`
* **Bad:** `added read path`

## Dependency Changes
Any modification to `Cargo.toml`, `Cargo.lock`, or enabled features must satisfy the workflow in `docs/dependency-security.md`.

**Checklist (include in PR description)**
- [ ] Identify Tier (0/1/2) for each change and record reviewer initials with date in `Cargo.toml` comment.
- [ ] Attach `cargo tree --edges normal,build,dev` diff (before/after).
- [ ] Run `cargo audit --deny warnings`.
- [ ] Run `cargo deny check bans licenses sources`.
- [ ] Run `cargo bloat --crates --release` and record notable regressions.
- [ ] Run `cargo xtask audit` (enforces feature allowlist, fmt, clippy, tests).
- [ ] Run `cargo xtask graph` if the dependency graph changed and archive the generated artefacts.
- [ ] Update `docs/security/audit-status.json` if this PR contains the latest successful audit run.

Pull requests lacking the artefacts above will be blocked until they comply.

## Review Expectations
- Validate dependency tiering and ensure comments follow the `YYYY-MM-DD <initials>` format.
- Confirm CI `security-audit` workflow succeeded and review the posted summaries.
- Reject PRs that introduce prohibited licenses or push the transitive dependency count beyond 50 without an approved waiver.
- Triage Dependabot PRs weekly (configurable via `.github/dependabot.yml`); do not merge without full audit artefacts.

## Dependabot Triage

When triaging Dependabot PRs, follow the policy in [`docs/dependency-security.md`](docs/dependency-security.md#dependabot-triage-policy):

1. **Green CI, patch bump** — merge directly after a quick review.
2. **Major version bumps** — check for breaking API changes. Dependabot cannot fix code; create a manual migration branch if needed.
3. **Ecosystem-coupled crates** (libp2p, axum+leptos+tower) — close individual PRs and coordinate a single migration.
4. **Verify crate legitimacy** — check `lib.rs` for protest/squatter crates and inspect transitive dependency diffs for supply-chain substitutions.
5. **Stale PRs** (changes already on main) — close promptly.

## Secure Coding Practices
- **No hardcoded secrets.** Never commit API keys, tokens, or signing keys. Use environment variables or file-based providers. Debug fallbacks must use random ephemeral values.
- **Validate all external input.** Object key paths must reject null bytes, backslashes, and `..` traversal components. Deserialize untrusted data only after size-limit checks.
- **Prefer poison-safe locks.** Use `.unwrap_or_else(|e| e.into_inner())` for `Mutex`/`RwLock` instead of `.unwrap()` to avoid cascading panics from poisoned locks. Use `.map_err()` when the caller can surface the error.
- **Minimize lock scope.** Acquire keys or shared state, clone, and drop the lock guard before performing expensive operations (encryption, MAC verification, I/O).
- **Descriptive expect messages.** Every `expect()` call must describe what operation failed and suggest remediation (e.g., `"failed to open audit log; check path and permissions"`).
- **Use `&'static str` for fixed strings.** Functions returning a fixed set of strings (like MIME type detection) should return `&'static str` to avoid per-call heap allocations.

## Fuzz & Side-channel Checks
- After modifying cryptography or compression, run `cargo fuzz run encrypt_roundtrip` to smoke-test the fuzz harness.
- Avoid data-dependent branching or early returns on sensitive comparisons; rely on helpers wired with `subtle::ConstantTimeEq`.

## Security Escalations
- Critical advisories require a release freeze, mitigation plan, and post-mortem within 72 hours.
- File emergency findings under `docs/security/meetings/<YYYY-MM>.md` and link to the GitHub issue or advisory.

For questions, open a GitHub Discussion tagged **Security & Dependencies** or ping the #space-security channel on Slack.
