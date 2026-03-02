# Release Policy

## Versioning

SPACE follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html):

- **MAJOR** (x.0.0) -- breaking API or wire-protocol changes
- **MINOR** (0.x.0) -- new features, backward-compatible
- **PATCH** (0.0.x) -- bug fixes, security patches

## Release Codenames

Each minor release carries a codename drawn from celestial objects --
alphabetically ordered, matching SPACE's cosmic theme.

| Version | Codename | Status |
|---------|----------|--------|
| v0.1.x | **Andromeda** | Planned |
| v0.2.x | **Bootes** | -- |
| v0.3.x | **Cassiopeia** | -- |
| v0.4.x | **Draco** | -- |
| v0.5.x | **Eridanus** | -- |
| v0.6.x | **Fornax** | -- |
| v0.7.x | **Gemini** | -- |
| v0.8.x | **Hydra** | -- |
| v0.9.x | **Io** | -- |
| v1.0.x | **Jupiter** | -- |

## Support Windows

| Tier | Window | What's included |
|------|--------|----------------|
| **Active** | Current + previous minor | Bug fixes, security patches, backports |
| **Security-only** | Two releases back | Critical/High security fixes only |
| **EOL** | Older | No updates; upgrade recommended |

Example: when v0.3.0 (Cassiopeia) ships, v0.2.x (Bootes) gets full support
and v0.1.x (Andromeda) gets security-only.

## Release Process

1. **Freeze.** Feature work stops on the release branch.
2. **Changelog.** `CHANGELOG.md` is finalized for the version.
3. **Tag.** `git tag v0.X.0` triggers the release workflow.
4. **CI gate.** The release workflow runs the full test suite before publishing.
5. **Publish.** Crates are published to crates.io; `spacectl` binary is attached
   to the GitHub Release.
6. **Announce.** Release notes are auto-generated from merged PRs.

## Backport Policy

See [backport issue template](../.github/ISSUE_TEMPLATE/backport.yml).

- **Security fixes**: Always backported to all supported branches.
- **Critical bugs**: Backported to the active support branch.
- **Features**: Never backported; upgrade to the latest minor.

Label a merged PR with `backport:release/v0.X.x` to trigger an automatic
cherry-pick PR via the backport workflow.
