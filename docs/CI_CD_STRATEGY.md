# CI/CD Strategy

This document describes SPACE's comprehensive CI/CD pipeline and automation strategy.

## 📋 Overview

SPACE uses a multi-layered CI/CD approach to ensure code quality, security, and reliability:

- **Fast feedback** - Basic checks run quickly on every PR (~5-7 min)
- **Comprehensive validation** - Deep security and quality checks (on main only)
- **Automated maintenance** - Dependency updates and monitoring
- **Cross-platform testing** - Verify compatibility across OS and architectures

### 💰 CI Optimization for GitHub Free Tier

To minimize CI minutes and parallel runs on the free tier:

- ✅ **Consolidated workflows**: Combined ci.yml, quality.yml, license-check.yml → pr-checks.yml
- ✅ **Strategic triggers**: Expensive checks (MSRV, cross-platform, security-audit) run on main only
- ✅ **Path filters**: Skip runs when only docs/markdown change
- ✅ **Scheduled checks**: MSRV, cross-platform, security-audit run weekly

**Result**: Reduced PR runs from **7 parallel workflows** → **2 parallel workflows** (~70% reduction)

## 🔄 CI/CD Workflows

### Core Workflows (Run on PRs)

| Workflow | Purpose | Duration | Runs On |
|----------|---------|----------|---------|
| **pr-checks.yml** | Fast feedback (fmt, clippy, test, docs, licenses) | ~5-7 min | Every PR (consolidated) |
| **semantic.yml** | Validate PR titles (conventional commits) | ~30 sec | Every PR |

**Optimization**: Consolidated ci.yml, quality.yml, and license-check.yml into pr-checks.yml to reduce parallel runs from 4 → 1 workflow.

### Security Workflows

| Workflow | Purpose | Duration | Runs On |
|----------|---------|----------|---------|
| **security-audit.yml** | Comprehensive security audit via xtask | ~10-15 min | Push to main / Weekly / Manual |
| **codeql.yml** | CodeQL security scanning | ~15-20 min | Push to main |
| **fuzzing.yml** | Fuzz testing for crypto/compression | ~30-60 min | Scheduled |

**Optimization**: Removed security-audit from PRs (runs on main only) to save ~10-15 min per PR.

### Quality & Performance

| Workflow | Purpose | Duration | Runs On |
|----------|---------|----------|---------|
| **coverage.yml** | Code coverage reporting | ~10-15 min | Push to main |
| **benchmark.yml** | Performance benchmarking | ~5-10 min | Push to main (path filtered) |

**Optimization**: Benchmark only runs when layout-engine changes.

### Compatibility Testing

| Workflow | Purpose | Duration | Runs On |
|----------|---------|----------|---------|
| **cross-platform.yml** | Test on Linux/Windows/macOS | ~15-20 min | Push to main / Weekly / Manual |
| **msrv.yml** | Verify Rust 1.83+ compatibility | ~5-10 min | Push to main / Weekly / Manual |

**Optimization**:
- Removed cross-platform and MSRV from PRs (run on main + weekly) to save ~20-25 min per PR
- ARM64 testing removed (emulation takes 30-60+ minutes) - test manually or use native ARM runners

### Maintenance

| Workflow | Purpose | Duration | Runs On |
|----------|---------|----------|---------|
| **dependency-drift.yml** | Monitor dependency updates | ~5 min | Scheduled |
| **release.yml** | Automated releases | ~10-15 min | Git tags |

## 🤖 Dependabot Configuration

### Cargo Dependencies
- **Schedule**: Weekly on Monday @ 3am UTC
- **PR Limit**: 10 concurrent PRs
- **Grouping Strategy**:
  - `crypto-stack` - All cryptography dependencies (aes, blake3, etc.)
  - `infra` - Infrastructure (tokio, axum, tracing)
  - `serde-stack` - Serialization dependencies
  - `compression` - Compression libraries (lz4, zstd)
- **Ignored**:
  - `bincode >=3.0.0` - Protest/squatter crate (not a real release)
  - `libp2p-*` major bumps - Must be coordinated as a set (see [dependency-security.md](dependency-security.md#known-ecosystem-constraints))
  - `windows-sys` patch/minor - Frequent churn, low risk
- **Triage Policy**: See [dependency-security.md](dependency-security.md#dependabot-triage-policy)

### GitHub Actions
- **Schedule**: Weekly on Monday @ 4am UTC
- **Auto-updates**: All GitHub Actions dependencies

## 🛠️ XTask Integration

The `cargo xtask audit` command runs comprehensive checks:

```bash
cargo xtask audit
```

**What it does:**
1. `cargo fmt --check` - Verify formatting
2. `cargo check` - Compile checks
3. `cargo test` - Run test suite
4. Feature allowlist validation
5. `cargo tree` - Dependency analysis
6. `cargo audit` - Security vulnerability scan
7. `cargo deny` - License and ban checks
8. `cargo bloat` - Binary size analysis

## 📊 CI/CD vs XTask Overlap

**Question**: Do CI and security-audit duplicate work?

**Answer**: Intentional redundancy for different purposes:

- **ci.yml** (Fast)
  - Goal: Quick feedback to developers
  - Runs: fmt, clippy, test
  - Duration: ~3-5 minutes
  - When: Every push/PR

- **security-audit.yml** (Comprehensive)
  - Goal: Deep validation before merge
  - Runs: Full `xtask audit` suite
  - Duration: ~10-15 minutes
  - When: Push/PR (can be made optional for draft PRs)

**Benefits:**
- Developers get fast feedback on basic issues
- Security team gets comprehensive validation
- Can merge with confidence after both pass

## 🔒 Security Posture

### Multi-Layer Security

1. **Dependency Scanning**
   - `cargo audit` - CVE database
   - `cargo deny` - License compliance
   - Dependabot - Automated updates

2. **Code Analysis**
   - CodeQL - Semantic code analysis
   - Clippy - Rust linter
   - Format checking

3. **Runtime Testing**
   - Fuzz testing (crypto/compression)
   - Unit tests
   - Integration tests

4. **License Compliance**
   - Verify MIT OR Apache-2.0 dual licensing
   - Check all dependencies are compatible
   - Validate license files exist

## 📈 Best Practices

### For Contributors

1. **Before opening PR**:
   ```bash
   cargo xtask audit
   ```

2. **PR titles** must follow [Conventional Commits](https://www.conventionalcommits.org/):
   - `feat(storage): add new feature`
   - `fix(encryption): resolve bug`
   - `docs: update README`
   - `deps: update tokio to 1.48`

3. **License compliance**:
   - All contributions are dual-licensed (MIT OR Apache-2.0)
   - New dependencies must be MIT/Apache compatible

### For Maintainers

1. **Review Dependabot PRs** (see [triage policy](dependency-security.md#dependabot-triage-policy)):
   - Green CI + patch bump: merge after quick review
   - Major bumps: check for breaking API changes, create manual migration branch if needed
   - Ecosystem-coupled crates (libp2p, axum+leptos): coordinate as a single migration
   - Verify crate legitimacy (check `lib.rs`, transitive dep diffs)

2. **Monitor security workflows**:
   - Address `cargo audit` warnings immediately
   - Review CodeQL findings
   - Check fuzz test results

3. **Release process**:
   - Tag releases following semver
   - Automated release workflow handles publishing

## 🎯 Workflow Triggers

### On Every PR
- pr-checks.yml (consolidated)
- semantic.yml

### On Push to Main
- pr-checks.yml
- security-audit.yml
- codeql.yml
- coverage.yml
- benchmark.yml (path filtered)
- msrv.yml
- cross-platform.yml

### Scheduled
- dependency-drift.yml (weekly)
- fuzzing.yml (daily)
- msrv.yml (weekly)
- cross-platform.yml (weekly)

### Manual
- security-audit.yml (can trigger with custom toolchain)

## 🔧 Maintenance

### Weekly Tasks (Automated)
- Dependabot updates (Monday 3am UTC)
- GitHub Actions updates (Monday 4am UTC)
- MSRV verification
- Cross-platform testing

### Monthly Tasks (Manual)
- Review security-audit summaries
- Update MSRV if needed
- Review dependency drift reports

## 📚 Related Documentation

- [CONTRIBUTING.md](../CONTRIBUTING.md) - Contributor guidelines
- [dependency-security.md](dependency-security.md) - Dependency management
- [SECURITY.md](../SECURITY.md) - Security policy

---

**Last Updated**: 2024-12-26

**Questions?** Open a GitHub Discussion tagged **CI/CD** or **Security & Dependencies**.
