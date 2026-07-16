# RFC-0036 Code Intelligence Public API Hardening V1

状态：in progress / R36.0-R36.4

创建日期：2026-07-16

基线：

- Code-quality finding: `.repo-local-dev/review/sigil-comprehensive-project-review-2026-07-10.md` P3-1
- Architecture baseline: [Sigil Rust Agent Core Technical Solution](../sigil-rust-agent-core-technical-solution.md)
- Predecessor: [RFC-0035 TUI Orchestration Boundary Hardening V1](0035-tui-orchestration-boundary-hardening-v1.md)

## 1. Summary

`sigil-code-intel` is an internal capability crate consumed in production only by
`sigil-runtime`, but its crate root currently exposes most implementation modules. This makes LSP
framing, discovery, edit planning, cache and workspace helpers look like supported cross-crate API.
It also prevents a meaningful missing-documentation gate: the RFC baseline produces 311
`missing_docs` errors.

This RFC replaces module-level exposure with a documented crate-root façade. Runtime consumers
use only that façade; implementation modules become private; the genuinely supported types and
functions receive boundary-focused rustdoc; and CI proves the crate with `-D missing-docs`.

## 2. Goals

1. Define one explicit crate-root API for context collection, service DTOs, tool registration and
   Doctor planning.
2. Make LSP framing, cache, discovery, edit, language, prepared-mutation and workspace internals
   private implementation details.
3. Move every production consumer from module-path imports to the crate-root façade.
4. Add useful rustdoc for each supported public type, field, constructor and operation.
5. Enforce the boundary with a repeatable rustdoc gate in CI.

## 3. Non-goals

- No code-intelligence product, tool, provider-visible result or TUI behavior change.
- No LSP startup, trust, timeout, mutation, RepoMap or Context V1 semantic change.
- No new crate, dependency, command, configuration field or durable event.
- No broad file split based only on line count.
- No compatibility guarantee for the previously accidental module paths; Sigil has not published
  `sigil-code-intel` as a stable external library.
- No version, tag or release action.

## 4. Supported façade

The crate-root façade contains four groups:

1. Context and RepoMap: `CodeContextBuilder`, context snapshot/row types,
   `RepoMapLite`/options/rows and `build_repo_map_lite`.
2. Service: `CodeIntelligenceService`, response/status/query DTOs, code symbol/location/diagnostic
   DTOs and prepared edit-plan summary types required by service methods.
3. Tool registration: the default and workspace-trust-aware registration functions.
4. Doctor planning: `EffectiveServerPlan`, `PlannedServerStatus`, `config_enabled` and
   `effective_server_plan`.

Types returned by a public method remain nameable from the crate root. Low-level LSP, discovery,
workspace path, cache and edit helpers do not remain public merely because internal tests use them.

## 5. Documentation contract

Public documentation must explain responsibility and safety boundaries, not restate field names.
It must state when an operation is bounded, request-local, cache-only, trust-gated or fallible.
Error-bearing public functions include an `Errors` section where the failure conditions are not
obvious from the signature.

The crate root enables `#![deny(missing_docs)]` only after accidental exports are removed and the
supported façade is documented. Local `allow(missing_docs)` escapes are not part of the design.

## 6. Implementation slices

1. R36.0: RFC, public-consumer inventory, baseline rustdoc evidence and execution ledger.
2. R36.1: private implementation modules, crate-root façade and runtime import migration.
3. R36.2: façade rustdoc and crate-level missing-documentation enforcement.
4. R36.3: CI rustdoc gate and workflow contract validation.
5. R36.4: full affected-scope validation, code-quality audit and completeness audit.

## 7. Acceptance criteria

- Production crates do not import `sigil_code_intel::<module>::...` paths.
- Supported code-intelligence API is available from `sigil_code_intel::...`.
- Implementation modules are not public from the crate root.
- `cargo rustdoc -p sigil-code-intel -- -D missing-docs` passes with no lint suppression.
- `sigil-code-intel` and `sigil-runtime` tests and strict Clippy pass.
- CI runs the rustdoc gate when code-intel, runtime, manifests or the workflow changes.
- Public user documentation is unchanged because the user workflow is unchanged.
- Architecture and execution ledgers accurately describe the façade.

## 8. Validation

```bash
cargo fmt --all --check
cargo check -p sigil-code-intel -p sigil-runtime
cargo test -p sigil-code-intel -- --format terse
cargo test -p sigil-runtime context -- --format terse
cargo test -p sigil-runtime code_intel -- --format terse
cargo rustdoc -p sigil-code-intel -- -D missing-docs
cargo clippy -p sigil-code-intel -p sigil-runtime --all-targets -- -D warnings
./scripts/check-touched.sh --scope base --base <r36-baseline> --tier standard
git diff --check
```

## 9. Progress

- R36.0 complete. Baseline `cargo rustdoc -p sigil-code-intel -- -D missing-docs`
  exits 101 with 311 missing-documentation diagnostics. Production source inspection found only
  `sigil-runtime` imports, currently through context, service and workspace module paths plus the
  crate-root registration function. The required fallback local decomposition audit found no
  behavior or durable-contract dependency: the façade can preserve the service/context/Doctor
  types while making low-level modules private. No sub-agent review was run because current agent
  policy only permits sub-agents when the user explicitly requests them.
- R36.1 complete. All implementation modules are private; context, service, tool-registration and
  Doctor-planning contracts are re-exported from the crate root; and `sigil-runtime` no longer
  imports code-intel module paths. The narrower reachability exposed five dead internal items:
  unused cache removal and discovery states were removed, the test-only LSP encoder is now
  test-only, and the redundant server-list helper test now exercises the supported plan result.
  Code-intel 144 tests and runtime context/code-intel focused suites pass.
