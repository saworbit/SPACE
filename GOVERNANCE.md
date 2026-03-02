# Governance

SPACE is currently maintained by a single author. This document establishes
lightweight governance so expectations are clear from day one and the project
can scale without a retroactive power struggle.

## Roles

| Role | Responsibilities | Current holders |
|------|-----------------|-----------------|
| **Maintainer** | Final merge authority, release cuts, security response, architecture decisions | @saworbit |
| **Reviewer** | Code review, triage, mentoring contributors | (open -- contribute regularly to be nominated) |
| **Contributor** | Anyone who opens a PR, files an issue, or improves docs | You, hopefully |

## Decision-making

1. **Consensus first.** Proposals are discussed in GitHub Issues or Discussions.
   The maintainer aims for consensus but retains final call on unresolved items.
2. **Architecture Decision Records.** Significant design choices are documented
   in `docs/` (e.g., `docs/architecture.md`, phase specs). Changes to these
   require a PR and review, not unilateral edits.
3. **Lazy consensus for small changes.** Bug fixes, typo corrections, and minor
   refactors can be merged by any reviewer after CI passes, unless they touch
   security-sensitive code (`crates/encryption/`, `SECURITY.md`,
   `docs/security/`).

## Path to Reviewer

A contributor may be nominated as a reviewer after demonstrating:

- 5+ merged PRs across at least 2 crates
- Familiarity with the project's dependency tiering and audit workflow
- Constructive code review on others' PRs

Reviewers are added to CODEOWNERS for the crates they know best.

## Multi-maintainer Transition

When the project reaches multiple maintainers (from different organizations if
applicable), governance will evolve:

- Maintainers require approval from at least one other maintainer for merges
  into security-sensitive paths.
- Release cuts require sign-off from at least two maintainers.
- No single organization may hold more than 50% of maintainer seats.

## Code of Conduct

All participants are expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
Violations are handled by the maintainer (or, in the multi-maintainer phase,
by majority decision).

## Amending This Document

Changes to governance require a PR with at least 7 days of open comment period
before merge.
