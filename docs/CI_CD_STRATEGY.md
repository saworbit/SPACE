# CI and releases

Everything runs on GitHub Actions. Every action is pinned to a full commit SHA, with the version in a comment; Dependabot keeps the pins current in one weekly pull request. Workflows get a read-only token unless a job needs more, and checkouts don't keep the token.

## Workflows

| Workflow | Runs on | What it does | Required to merge |
|----------|---------|--------------|-------------------|
| [`ci.yml`](../.github/workflows/ci.yml) | pull requests, pushes to `main`, merge queue, weekly, manual, and from `release.yml` | fmt, clippy (also with `storage/uring`), tests on Linux, Windows and macOS, `cargo check` of the libraries and binaries on `rust-version`, rustdoc with warnings denied, `cargo audit`, `cargo deny`, dependency review (pull requests only), `typos`, `actionlint` and `zizmor`, plus a check that the license field is `BUSL-1.1`; on a pull request that changes `release.yml`, `release-build.yml` or the `Dockerfile`, the release builds as a dry run | yes, through the `Build and test` job |
| [`codeql.yml`](../.github/workflows/codeql.yml) | pull requests, pushes to `main`, weekly | CodeQL analysis of the Rust code and of the workflows; results go to code scanning | no |
| [`scorecard.yml`](../.github/workflows/scorecard.yml) | pushes to `main`, weekly, branch protection changes | OpenSSF Scorecard; publishes the score shown in the README and uploads findings to code scanning | no |
| [`docker.yml`](../.github/workflows/docker.yml) | changes to the Dockerfile, `.dockerignore`, `Cargo.toml` or `Cargo.lock`; weekly | hadolint, `docker build` (on current stable Rust, from digest-pinned base images), a smoke test that writes and reads a capsule as the image's non-root user, and a check that `docker stop` ends `serve-s3` with exit status 0 well inside its timeout | no |
| [`links.yml`](../.github/workflows/links.yml) | changes to Markdown, weekly | lychee over every Markdown file (ignores in `.lycheeignore`) | no, an outside site being down shouldn't block a merge |
| [`labeler.yml`](../.github/workflows/labeler.yml) | pull requests from this repository | labels the pull request by the paths it changes ([`.github/labeler.yml`](../.github/labeler.yml)) | no |
| [`release.yml`](../.github/workflows/release.yml) | `v*` tags | see below | no |
| [`release-build.yml`](../.github/workflows/release-build.yml) | from `release.yml`, and from `ci.yml` as a dry run | builds the release archives | yes, as part of `Build and test` when it runs |

### The required check

Branch protection on `main` requires one check, `Build and test`. In `ci.yml` that name belongs to the `ci-ok` job, which waits for every other CI job and fails if any of them failed or was cancelled. A job added to `ci.yml` becomes required by adding it to `ci-ok`'s `needs`, with no change to the branch protection settings.

`ci.yml` has no paths filters, so the check reports on every pull request, Markdown-only ones included. A required check that never runs would leave the pull request unmergeable.

The release dry run is the one conditional job. A pull request that changes `.github/workflows/release.yml`, `.github/workflows/release-build.yml` or the `Dockerfile` (whose builder image the Linux release builds use) runs the four release builds, and `Build and test` fails if any of them fails. Other pull requests skip them, and `ci-ok` counts a skipped job as passed.

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
3. `spacectl` is built with `--locked`, from a clean slate with no build cache, for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc` and `aarch64-apple-darwin` ([`release-build.yml`](../.github/workflows/release-build.yml)). Each archive holds the binary, `LICENSE`, `NOTICE`, `README.md` and `TOOLCHAIN.txt` (the `rustc -vV` it was built with).
4. Once all of that passes, a GitHub release is created with the archives, a `SHA256SUMS` file, the attestation bundle, and generated notes that link to `CHANGELOG.md`. A tag with a suffix, such as `v0.2.0-rc.1`, makes a pre-release.

Each archive has a signed build provenance attestation. To check that a download was built by this workflow from this repository:

```bash
gh attestation verify spacectl-v0.2.0-x86_64-unknown-linux-gnu.tar.gz --repo saworbit/SPACE
```

The release also carries the attestations as `spacectl-vX.Y.Z.sigstore.json`; pass it with `--bundle` to verify without asking GitHub.

The workspace crates are `publish = false`, so nothing goes to crates.io. No container image is published.

The Linux binaries are built in the `Dockerfile`'s builder image (Debian 12), pinned by digest, and need glibc 2.34 or newer (Debian 12, Ubuntu 22.04, RHEL 9 and later). Each build runs the binary on Debian 12 and Ubuntu 22.04 before it's packaged. The Windows binary links the C runtime statically, so it needs no VC++ redistributable. The macOS and Windows binaries aren't code-signed, so Gatekeeper and SmartScreen warn on first run.

A pull request that changes how a release is built runs the four builds without publishing anything, as part of `Build and test` (see [The required check](#the-required-check)), so a broken release can't merge.

### Reproducing a release

The Linux archives are reproducible: building the same commit again gives the same bytes. The Rust toolchain is the builder image's, so it changes only when the image's digest does (the Windows and macOS builds install the same Rust version). The source is always at `/src` in the container, and `--remap-path-prefix` keeps the source and cargo home paths out of the binary. The archive is sorted, with owner and group 0, every timestamp set to the commit's time (`SOURCE_DATE_EPOCH`), and no name or time in the gzip header.

To check a Linux archive against its tag, on an x86_64 machine with Docker and GNU tar:

```bash
git checkout v0.2.0
image=$(sed -n 's/^FROM \(rust:[^ ]*\) AS builder$/\1/p' Dockerfile)
docker run --rm --volume "$PWD:/src" --workdir /src "$image" bash -c \
  'RUSTFLAGS="--remap-path-prefix=/src=. --remap-path-prefix=$CARGO_HOME=/cargo" \
   cargo build --release --locked -p spacectl --target x86_64-unknown-linux-gnu && rustc -vV > target/toolchain.txt'
name=spacectl-v0.2.0-x86_64-unknown-linux-gnu
mkdir -p "dist/$name"
cp LICENSE NOTICE README.md target/x86_64-unknown-linux-gnu/release/spacectl "dist/$name/"
cp target/toolchain.txt "dist/$name/TOOLCHAIN.txt"
tar -C dist --sort=name --format=gnu --owner=0 --group=0 --numeric-owner \
  --mtime="@$(git log -1 --format=%ct)" --mode=go-w -cf - "$name" | gzip -n > "dist/$name.tar.gz"
sha256sum "dist/$name.tar.gz"
```

The checksum should match the release's `SHA256SUMS`. If it doesn't, compare the binaries first: another `gzip` can change the archive's bytes without changing what's in it. The Windows and macOS archives aren't reproducible yet; check them with the attestation.

## Dependencies

Dependabot opens weekly pull requests for cargo crates, GitHub Actions and the digests of the Dockerfile's base images, after a seven-day cooldown on new versions. Dependabot security updates, when enabled in the repository settings, don't wait. See [dependency-security.md](dependency-security.md) for how they're reviewed.
