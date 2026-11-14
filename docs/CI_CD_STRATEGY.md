# CI and releases

Everything runs on GitHub Actions. Every action is pinned to a full commit SHA, with the version in a comment; Dependabot keeps the pins current in one weekly pull request. Workflows get a read-only token unless a job needs more, and checkouts don't keep the token.

## Workflows

| Workflow | Runs on | What it does | Required to merge |
|----------|---------|--------------|-------------------|
| [`ci.yml`](../.github/workflows/ci.yml) | pull requests, pushes to `main`, merge queue, weekly, manual, and from `release.yml` | fmt, clippy (also with `storage/uring`), tests on Linux, Windows and macOS, `cargo check` of the libraries and binaries on `rust-version`, rustdoc with warnings denied, `cargo audit`, `cargo deny`, dependency review (pull requests only), `typos`, `actionlint` and `zizmor`, plus checks that the license field is `BUSL-1.1` and the Dockerfile builds on `rust-version` | yes, through the `Build and test` job |
| [`codeql.yml`](../.github/workflows/codeql.yml) | pull requests, pushes to `main`, weekly | CodeQL analysis of the Rust code and of the workflows; results go to code scanning | no |
| [`scorecard.yml`](../.github/workflows/scorecard.yml) | pushes to `main`, weekly, branch protection changes | OpenSSF Scorecard; publishes the score shown in the README and uploads findings to code scanning | no |
| [`docker.yml`](../.github/workflows/docker.yml) | changes to the Dockerfile, `.dockerignore`, `Cargo.toml` or `Cargo.lock`; weekly | hadolint, `docker build`, and a smoke test that writes and reads a capsule as the image's non-root user | no |
| [`links.yml`](../.github/workflows/links.yml) | changes to Markdown, weekly | lychee over every Markdown file (ignores in `.lycheeignore`) | no, an outside site being down shouldn't block a merge |
| [`labeler.yml`](../.github/workflows/labeler.yml) | pull requests from this repository | labels the pull request by the paths it changes ([`.github/labeler.yml`](../.github/labeler.yml)) | no |
| [`release.yml`](../.github/workflows/release.yml) | `v*` tags; pull requests that change it (build only) | see below | no |

### The required check

Branch protection on `main` requires one check, `Build and test`. In `ci.yml` that name belongs to the `ci-ok` job, which waits for every other CI job and fails if any of them failed or was cancelled. A job added to `ci.yml` becomes required by adding it to `ci-ok`'s `needs`, with no change to the branch protection settings.

`ci.yml` has no paths filters, so the check reports on every pull request, Markdown-only ones included. A required check that never runs would leave the pull request unmergeable.

The weekly run catches what changes without a commit: new RustSec advisories, and new lints in a new stable Rust.

### Running the checks locally

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo audit --deny warnings
cargo deny --locked check
typos
actionlint
zizmor .
```

`cargo install cargo-audit cargo-deny typos-cli --locked` installs the Rust tools. actionlint and zizmor have their own installers.

## Releases

Pushing a tag `vX.Y.Z` (the version in the root `Cargo.toml`) runs `release.yml`:

1. The tag is checked against the workspace version.
2. The whole of `ci.yml` runs on the tagged commit.
3. `spacectl` is built with `--locked`, from a clean slate with no build cache, for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`. Each archive holds the binary, `LICENSE`, `NOTICE` and `README.md`.
4. Once all of that passes, a GitHub release is created with the archives, a `SHA256SUMS` file, the attestation bundle, and generated notes that link to `CHANGELOG.md`. A tag with a suffix, such as `v0.2.0-rc.1`, makes a pre-release.

Each archive has a signed build provenance attestation. To check that a download was built by this workflow from this repository:

```bash
gh attestation verify spacectl-v0.2.0-x86_64-unknown-linux-gnu.tar.gz --repo saworbit/SPACE
```

The release also carries the attestations as `spacectl-vX.Y.Z.sigstore.json`; pass it with `--bundle` to verify without asking GitHub.

The workspace crates are `publish = false`, so nothing goes to crates.io. No container image is published.

The Linux binaries are built on Ubuntu 24.04 and need glibc 2.39 or newer (Ubuntu 24.04, Debian 13, Fedora 40). On older systems, build from source or use the Docker image. The macOS and Windows binaries aren't code-signed, so Gatekeeper and SmartScreen warn on first run.

A pull request that changes `release.yml` runs the four builds without publishing anything, so a broken release workflow shows up before a tag needs it.

## Dependencies

Dependabot opens weekly pull requests for cargo crates, GitHub Actions and the Dockerfile's base images, after a seven-day cooldown on new versions (security updates don't wait). See [dependency-security.md](dependency-security.md) for how they're reviewed.
