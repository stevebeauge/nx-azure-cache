# Mutual trust between PRs, writes reserved to CI

Small internal team: a PR may read what another PR wrote, and no barrier is set up against cache poisoning between PRs. The separate write service connection with a branch check, considered at first, is dropped. The CREEP risk (CVE-2025-36852: an artifact forged in a PR served to a trusted build) is accepted between PRs, and contained by three rules.

1. **Only CI writes**: developer machines are reader **Identities**, and Azure RBAC enforces it. The reason is reproducibility, not distrust: an artifact produced on a Windows machine and served to Linux CI opens the door to stale hits (line endings, paths, environment).
2. **Release pipelines never read the cache**: a `release` never ships a `dist` coming from the cache. It is a constraint on consumers, restated in the tool's docs.
3. A nightly run without cache, on the consuming Workspaces' side, compares verdicts and outputs. It is outside this tool.

Decided on 2026-10-01.
