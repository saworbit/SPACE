# CI

Pull requests run `.github/workflows/pr-checks.yml`: a license field check and `cargo test --workspace`.

`cross-platform.yml`, `msrv.yml`, and `coverage.yml` also run the workspace tests on `main`. `release.yml` runs the tests and builds `spacectl` when a `v*` tag is pushed.

There is no xtask, fuzz, or layout-engine benchmark workflow on this branch.
