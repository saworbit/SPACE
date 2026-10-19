# Dependencies

The workspace pins cryptography crates in the root `Cargo.toml`. `Cargo.lock` is committed.

When a dependency changes:

- Run `cargo test --workspace`.
- Run `cargo audit` and `cargo deny check` locally if those tools are installed.

Dependabot opens weekly cargo and GitHub Actions updates. `bincode` 3.x stays ignored because that release is a protest crate.
