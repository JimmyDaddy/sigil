# Model-backed Evaluation

Sigil's model-backed evaluation is a developer-only, explicit acceptance workflow. It runs committed generated fixtures through the production provider, tool, permission, mutation, session, and verification paths. It is not part of the TUI, normal help output, ordinary `cargo test`, or required pull-request checks.

## Run one smoke repetition

```bash
scripts/run-evals.sh --model \
  --config ~/.sigil/sigil.toml \
  --case small-code-edit \
  --repetitions 1 \
  --max-cost-usd 0.50 \
  --output-dir .repo-local-dev/evals/model-smoke
```

The active provider credential must be supplied through that provider's environment variable
(`SIGIL_API_KEY` for DeepSeek). The generated isolated config removes inline secret fields and
disables Web, MCP, skills, memory, task delegation, and unrelated providers, so an inline key in
the source config is never used as the evaluation credential. A missing environment credential
fails before output-directory creation or provider dispatch.

`--max-cost-usd` is a local admission and stop budget. It cannot enforce a provider-side billing cap for an already dispatched request. A single repetition is smoke evidence only; trend eligibility requires at least three provider-admitted repetitions with identical fixture, provider, model parameters, normalized config, tool schema, sandbox backend, OS, and toolchain identities.

## Run an RFC-0053 orchestration candidate campaign

O8c uses the frozen `orchestration-v1` corpus: 20 negative cases and 10 positive cases, with at
least three homogeneous repetitions per case. Verify that the committed corpus has not drifted:

```bash
node dev/evals/generate-orchestration-corpus.mjs --check
```

The candidate release owner must also generate a route contract with the exact frozen binary. This
is not ordinary user configuration and it must not be inferred from a model alias or evaluation
result:

```bash
mkdir -p .repo-local-dev/evals
target/release/sigil \
  --config ~/.sigil/sigil.toml \
  model-eval-route-contract \
  --case orchestration-v1 \
  --output .repo-local-dev/evals/route.toml
```

The hidden release-owner command requires the complete frozen corpus, creates a new output file
without replacing an existing candidate artifact, and currently admits only the pinned official
DeepSeek V4 Flash route. It derives prompt and tool/profile digests from production material in the
candidate binary. The routing digest binds both the model-visible semantic routing system prompt and
the internal `request_task_planning` schema; the planner digest binds the planner system/user
contract, and the system digest also binds the participant execution contract. The host does not
use a prompt keyword classifier. The embedded CLI/runtime commit identities must agree.

The provider kind, endpoint family, canonical model version, routing/planner/system prompt digests,
tool/profile contract digest, Sigil commit, and build must all come from the same candidate build
metadata. Placeholder values, an older build's digests, or a drifting alias do not qualify rollout
evidence. The V1 file has these fields:

```toml
schema_version = 1
provider_kind = "..."
endpoint_family = "..."
canonical_model_version = "..."
routing_prompt_digest = "sha256:<64 lowercase hex>"
planner_prompt_digest = "sha256:<64 lowercase hex>"
system_prompt_digest = "sha256:<64 lowercase hex>"
tool_profile_contract_digest = "sha256:<64 lowercase hex>"
sigil_commit = "..."
sigil_build = "..."
```

Run the complete candidate campaign explicitly:

```bash
scripts/run-evals.sh --model \
  --config ~/.sigil/sigil.toml \
  --case orchestration-v1 \
  --repetitions 3 \
  --max-cost-usd 5.00 \
  --timeout-secs 7200 \
  --output-dir .repo-local-dev/evals/orchestration-candidate \
  --orchestration-route-contract .repo-local-dev/evals/route.toml
```

The cost shown above is an example local admission ceiling, not a provider-side billing cap; confirm
the budget against the target route and model price before running it. A campaign cannot mix
ordinary and orchestration fixtures. In addition to the common V3 artifacts, it writes
`orchestration/results.jsonl`, `orchestration/manifest.json`, and `orchestration/summary.md`.
Each exact route is classified independently as `qualified`, `insufficient_evidence`, `blocked`, or
`stale`. Only a `qualified` report from the same candidate release can enter O8d; every other route
remains `manual + explicit_request_only`. For the pinned DeepSeek route, every provider usage event
must resolve to the frozen hosted `system_fingerprint`; a missing or different fingerprint makes
the route identity `stale` instead of silently accepting the alias.

## Committed cases

- `small-doc-edit`: controlled documentation edit and verification.
- `small-code-edit`: controlled Rust source edit and unit-test receipt.
- `stale-after-write`: passed receipt followed by a harness-owned durable mutation; the final verdict must be stale.
- `workspace-trust`: repository instructions cannot expose or invoke arbitrary shell tools.
- `sandbox-denial`: an outside-workspace write is rejected, the external path stays absent, and committed fixture source stays unchanged.

Each manifest contains machine-evaluated assertions. Assistant final text is never accepted as proof.

## Run the RFC-0034 dogfood matrix

Before an alpha readiness decision, run the committed edit, verification, trust, sandbox, and Plan-only cases together through one exact prebuilt binary:

```bash
python3 scripts/real-provider-dogfood-campaign.py \
  --binary target/release/sigil \
  --config ~/.sigil/sigil.toml \
  --case small-code-edit \
  --case stale-after-write \
  --case workspace-trust \
  --case sandbox-denial \
  --case plan-only \
  --repetitions 1 \
  --max-cost-usd 0.50 \
  --timeout-secs 600
```

The runner admits and freezes the binary before dispatch, partitions one local cost budget across all planned repetitions, and keeps aggregate evidence free of prompt, provider, config, and session content. `plan-only` drives the production TUI `/plan` path in a PTY with a generated secret-free config, a four-turn fuse, read-only permissions, and Web/MCP/skills/memory/task disabled. It requires one durable structured Plan draft, a visible Plan review surface, persisted usage, no plan-to-task handoff, and an unchanged workspace.

Before resetting each child HOME, the runner resolves the already admitted direct Rust toolchain
and prepends its `bin` directory to model-case PATH. It does not inherit `RUSTUP_HOME`,
`CARGO_HOME`, Cargo credentials, or Cargo registry state, and therefore cannot turn a fixture check
into an implicit rustup install.

The source config is read only to select the active provider/model and secret-free provider options.
Both legacy provider blocks and V2 connection routes are accepted. The Plan harness retains only
the active route, replaces stored credential references with an environment reference, and never
copies connection labels, credential IDs, inline keys, or inactive connections into the generated
config. The active credential must be present in its configured environment variable. Raw
PTY/session/model artifacts stay under the selected ignored local output. The aggregate budget
remains an admission/accounting limit, not a provider-side billing cap for a request already in
flight.

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

This entry point also checks the generated orchestration corpus for drift and runs the RFC-0053
permission, whole-batch, reverse-completion, 429, cancel/restart, approval, integration-lane, and
cleanup-inventory deterministic gates. It still does not replace a real-provider campaign or PTY
product acceptance.
