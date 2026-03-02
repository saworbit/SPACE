## Description

Summary of change and relevant motivation.

Fixes # (issue)

## Type

- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Refactor / cleanup
- [ ] Docs / comments only

## Checklist

### Quality gates

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] Tests pass (`cargo test --workspace`)
- [ ] `cargo xtask audit` (if touching crates, deps, or features)
- [ ] Docs compile: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`

### Documentation

- [ ] I updated relevant docs (or no user-facing changes)
- [ ] CHANGELOG.md updated (for features, fixes, breaking changes)

### Security

- [ ] No hardcoded secrets, tokens, or keys introduced
- [ ] External inputs are validated (if applicable)
- [ ] No new `unsafe` blocks (or they are justified and documented)

### Dependency changes (skip if none)

- [ ] Tier identified (0 = crypto, 1 = infra, 2 = dev)
- [ ] `cargo audit --deny warnings` clean
- [ ] `cargo deny check bans licenses sources` clean
- [ ] Transitive dependency count still under limit (`cargo xtask drift`)
