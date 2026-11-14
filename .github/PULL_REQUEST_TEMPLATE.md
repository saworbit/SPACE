## What and why

<!-- What this changes, and the problem it solves. -->

Fixes #

## Type

- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change (on-disk format, CLI, S3 behaviour or public API)
- [ ] Refactor or cleanup
- [ ] Docs only
- [ ] Build, CI or dependencies

## Checklist

CI runs all of these; running them first saves a round trip.

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
- [ ] `typos`
- [ ] CHANGELOG.md has an entry, unless nothing user-facing changed
- [ ] Docs match the change

### Security

- [ ] No secrets, tokens or keys added
- [ ] External input (S3 requests, files, journal records) is validated
- [ ] No new `unsafe`, or it is justified in a comment

### Dependency changes (skip if none)

- [ ] Why the dependency is needed; crypto and serialization crates pinned with `=`
- [ ] `cargo audit --deny warnings` and `cargo deny check` pass
- [ ] `cargo tree --edges normal,build,dev` diff attached (before and after)
