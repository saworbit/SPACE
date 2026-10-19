# CI

Pull requests and pushes to `main` run `.github/workflows/ci.yml`: a license field check and `cargo test --workspace` on Ubuntu. Markdown-only changes skip that run.

`.github/workflows/release.yml` runs the same tests and builds `spacectl` when a `v*` tag is pushed, then attaches the binary to a GitHub release. The workspace crates are `publish = false`, so a tag does not publish to crates.io.

Dependabot still opens weekly cargo and GitHub Actions updates.
