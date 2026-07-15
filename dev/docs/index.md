# Sigil Developer Documentation

This index is the starting point for contributors and maintainers. User-facing
installation, configuration, and TUI guidance remains under `docs/`; the files
below explain how Sigil is engineered and why its internal boundaries exist.

## Engineering Rules

- [Code standards](../governance/code-standards.md): Rust style, crate
  boundaries, error handling, tests, and minimum checks.
- [Engineering standards](../governance/engineering-standards.md): change
  workflow, quality-gate selection, documentation sync, and TUI product rules.
- [Repository agent instructions](../../AGENTS.md): the short repository-wide
  constraints that apply before any code change.
- [Contributing guide](../../CONTRIBUTING.md): public contribution and pull
  request workflow.
- [Security policy](../../SECURITY.md): private vulnerability reporting and
  supported versions.

## Architecture and Direction

- [Rust agent core technical solution](sigil-rust-agent-core-technical-solution.md):
  current crate ownership, provider-neutral contracts, event/session model, and
  TUI-first architecture.
- [Capability roadmap](sigil-capability-roadmap.md): frozen capability baseline
  and the evidence gates that govern later work.
- [TUI mouse interaction design](sigil-tui-mouse-interaction-design.md): mouse
  input, hit testing, terminal behavior, and interaction constraints.

## RFCs and Execution Status

- [Formal RFC index](rfcs/): durable event, mutation, verification, execution,
  context, task, projection, extension, recovery, protocol, and productization
  designs committed to the repository.
- Active execution slices are tracked in
  `.repo-local-dev/rfcs/STATUS.md` inside maintainer workspaces. That board is
  intentionally local-only; formal design decisions must be reflected in the
  committed RFCs and governance documents rather than relying on the local
  status board as a public artifact.
- [Archived developer material](archive/README.md): historical reports retained
  for context, not current requirements.

## Release and Distribution

- [Model-backed evaluation](model-evaluation.md): explicit provider-backed
  acceptance, bounded cost admission, evidence, and report artifacts.
- [真实模型评测](model-evaluation.zh-CN.md)：显式真实模型验收、成本准入、证据与报告产物。
- [Release process](release-process.md): release tags, multi-platform archives,
  provenance, Homebrew and npm packaging, release notes, and release checks.
- [Release workflow](../../.github/workflows/release.yml): executable CI release
  definition.

When implementation and an older design note disagree, verify the current code
and governance documents first, then update the affected committed document in
the same change.
