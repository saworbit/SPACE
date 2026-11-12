# Dependencies

The workspace pins cryptography crates in the root `Cargo.toml`. `Cargo.lock` is committed.

When a dependency changes:

- Run `cargo test --workspace`.
- Run `cargo audit --deny warnings` and `cargo deny check`. CI runs both on every pull request.
- Ignore an advisory only when the dependency tree actually triggers it: add it to both `.cargo/audit.toml` and `deny.toml`, with the reason.

Dependabot opens weekly cargo and GitHub Actions updates. `bincode` major versions are ignored: it is the on-disk encoding for `space.db`, so a major bump needs a format migration (#113). 3.x is a protest crate in any case.
