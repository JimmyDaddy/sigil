# Model-backed Evaluation

Sigil's model-backed evaluation is a developer-only, explicit acceptance workflow. It runs committed generated fixtures through the production provider, tool, permission, mutation, session, and verification paths. It is not part of the TUI, normal help output, ordinary `cargo test`, or required pull-request checks.

## Run one smoke repetition

```bash
scripts/run-evals.sh --model \
  --config ~/.config/sigil/config.toml \
  --case small-code-edit \
  --repetitions 1 \
  --max-cost-usd 0.50 \
  --output-dir .repo-local-dev/evals/model-smoke
```

The active provider credential must be supplied through its normal environment or secret source. The generated isolated config removes inline secret fields and disables Web, MCP, skills, memory, task delegation, and unrelated providers.

`--max-cost-usd` is a local admission and stop budget. It cannot enforce a provider-side billing cap for an already dispatched request. A single repetition is smoke evidence only; trend eligibility requires at least three provider-admitted repetitions with identical fixture, provider, model parameters, normalized config, tool schema, sandbox backend, OS, and toolchain identities.

## Committed cases

- `small-doc-edit`: controlled documentation edit and verification.
- `small-code-edit`: controlled Rust source edit and unit-test receipt.
- `stale-after-write`: passed receipt followed by a harness-owned durable mutation; the final verdict must be stale.
- `workspace-trust`: repository instructions cannot expose or invoke arbitrary shell tools.
- `sandbox-denial`: an outside-workspace write is rejected, the external path stays absent, and committed fixture source stays unchanged.

Each manifest contains machine-evaluated assertions. Assistant final text is never accepted as proof.

## Artifacts

The output directory is created once and contains:

- `results.jsonl`: schema V3 source of truth, one record per repetition;
- `manifest.json`: campaign counts, cost, and exact trend buckets;
- `summary.md`: human-readable projection;
- per-run generated workspace, secret-free config, and V2 durable session.

Non-accepted runs remain inspectable through their session artifact path and structured mismatch reasons. Never commit generated campaign directories or credentials.

## Deterministic mode

Use the fake-provider conformance suite when no model call is required:

```bash
scripts/run-evals.sh --deterministic
```

Deterministic results prove local contracts; they must not be reported as real-model success rates.

