# CI

Pull requests and pushes to `main` run `.github/workflows/ci.yml` on Ubuntu: a license field check, `cargo fmt`, clippy, `cargo test --workspace`, a build on the minimum Rust version, and a dependency audit (`cargo audit --deny warnings` and `cargo deny check`). Markdown-only changes skip that run.

`.github/workflows/release.yml` runs the same tests and builds `spacectl` when a `v*` tag is pushed, then attaches the binary to a GitHub release. The workspace crates are `publish = false`, so a tag does not publish to crates.io.

Dependabot still opens weekly cargo and GitHub Actions updates.
