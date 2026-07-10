# Contributing to Sigil

Thanks for helping improve Sigil. The project is a TUI-first Rust coding agent,
so changes should preserve the terminal user experience, provider-neutral
kernel contracts, and auditable session and tool behavior.

## Before You Start

- Use a public issue for bugs, feature proposals, and behavior discussions.
- For security vulnerabilities, follow [SECURITY.md](SECURITY.md) instead of
  opening a public issue.
- Read the [developer documentation index](dev/docs/index.md), especially the
  code and engineering standards, before changing code.
- Discuss broad architecture, public contract, persistence, permission, or TUI
  workflow changes before investing in a large patch.

## Development Workflow

1. Fork or clone the repository and branch from the current `main` branch.
2. Keep each change focused and preserve existing crate dependency direction.
3. Put Rust unit-test implementations in the matching `tests/*_tests.rs` file;
   production modules should only contain the test-module declaration.
4. Update user or developer documentation when behavior, configuration,
   commands, architecture, or engineering rules change.
5. Run the smallest quality gate that proves the change:

   ```bash
   ./scripts/check-touched.sh --tier quick
   ```

   Use `--tier standard` for session, event, mutation, verification,
   permission, tool, provider, or TUI runner changes. Use `--tier full` before
   release-sized or broad cross-crate changes.
6. Use a clear Conventional Commit subject and open a pull request that
   explains the user-visible outcome, risk, and validation performed.

## Pull Request Checklist

- The change is scoped and contains no unrelated cleanup.
- New behavior has meaningful tests; existing tests were not weakened.
- TUI changes keep state, events, controls, and help text synchronized.
- Provider-specific details remain inside the provider crate.
- Session and control-state changes remain append-only and recoverable.
- Documentation links and commands resolve in the current repository.
- The pull request lists every validation command that was run.

By contributing, you agree that your contribution is licensed under the
project's [MIT License](LICENSE).
