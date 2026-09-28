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

O8c uses the frozen `orchestration-v1` corpus: 20 Chat, 15 PlanReview, and 15 DirectTask cases,
with at least three homogeneous repetitions per case. Verify that the committed corpus has not
drifted:

```bash
node dev/evals/generate-orchestration-corpus.mjs --check
```

The candidate release owner must also generate a route contract with the exact frozen binary. This
is not ordinary user configuration and it must not be inferred from a model alias or evaluation
result. The config must resolve its default model to the exact `deepseek-v4-flash` ID:

```bash
candidate_commit="$(git rev-parse --short=12 HEAD)"
SIGIL_RUNTIME_BUILD_GIT_HASH="$candidate_commit" cargo build --release -p sigil-release-tools \
  --bin sigil-model-eval \
  --bin sigil-model-eval-route-contract

target/release/sigil-model-eval-route-contract \
  --launch-cwd . \
  --config ~/.sigil/sigil.toml \
  --case orchestration-v1 \
  --provider-system-fingerprint <live-system-fingerprint> \
  --sigil-commit "$candidate_commit" \
  --output .repo-local-dev/evals/route.toml
```

The hidden release-owner command requires the complete frozen corpus, creates a new output file
without replacing an existing candidate artifact, and currently admits only the pinned official
DeepSeek V4 Flash route. It derives prompt and tool/profile digests from production material in the
candidate binary. The routing digest binds the model-visible ordinary conversation contract and the
current `request_plan_review`, `start_task`, continuation, and pending-Plan tool schemas. The
direct-Task prompt digest binds the direct execution contract; there is no separate planner prompt
or planner schema. The host does not use a prompt keyword classifier. The embedded CLI/runtime
commit identities must agree.

`<live-system-fingerprint>` must come from a release-owner provider probe against the same official
route. It is an explicit observation, not a tokenizer-parity constant or a model-alias inference.
The campaign re-derives the complete contract from the exact candidate binary, config, corpus,
prompts, and tool profiles before provider dispatch; only that exact DeepSeek candidate receives the
evaluation-only DirectTask capability needed to qualify a rollout before its sidecar exists.

The provider kind, endpoint family, canonical model version, routing/direct-task/system prompt digests,
tool/profile contract digest, Sigil commit, and build must all come from the same candidate build
metadata. Placeholder values, an older build's digests, or a drifting alias do not qualify rollout
evidence. The V1 file has these fields:

```toml
schema_version = 1
provider_kind = "..."
endpoint_family = "..."
canonical_model_version = "..."
routing_prompt_digest = "sha256:<64 lowercase hex>"
direct_task_prompt_digest = "sha256:<64 lowercase hex>"
system_prompt_digest = "sha256:<64 lowercase hex>"
tool_profile_contract_digest = "sha256:<64 lowercase hex>"
sigil_commit = "..."
sigil_build = "..."
```

Run the complete candidate campaign explicitly:

```bash
SIGIL_MODEL_EVAL_BIN=target/release/sigil-model-eval scripts/run-evals.sh --model \
  --config ~/.sigil/sigil.toml \
  --case orchestration-v1 \
  --repetitions 3 \
  --max-cost-usd 5.00 \
  --timeout-secs 10800 \
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

Fixtures may expect a `paused` terminal when the correct model action is to request durable user input. Pair that terminal with the `user_input_pending` assertion so an interrupted request is accepted only when the session actually retains an unanswered request; a plain pause does not satisfy it.

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
Only the current connection schema is accepted. The Plan harness retains only
the active route, replaces stored credential references with an environment reference, and never
copies connection labels, credential IDs, inline keys, or inactive connections into the generated
config. The active credential must be present in its configured environment variable. Raw
PTY/session/model artifacts stay under the selected ignored local output. The aggregate budget
remains an admission/accounting limit, not a provider-side billing cap for a request already in
flight.

## Fixed repository regressions and multiple turns

`dev/evals/model-fixtures/repository-v1` freezes 20 fault-injected tasks using actual repository
modules, with source commit and file/check hashes. These controlled source subsets do not measure
general success on full-repository historical issues.

Schema 1 accepts optional `[[followup_prompts]]` entries containing `prompt_file` and
`prompt_sha256`. Initial and follow-up prompts share the existing 1 MiB fixture source budget
with workspace files; each prompt remains bounded at 16 KiB. Fixed user turns reuse the admitted physical session and have
separate run IDs. The previous execution must join with a completed terminal before the next turn;
input requests, cancellation and failures do not automatically resume. All turns share the case's
remaining campaign deadline. Independent checks and assertions run after the final turn and can
verify that earlier requirements still hold.

A fixture's `checks.command` is explicit trusted direct argv, rather than a Cargo-only list.
Python and Node checks use the same managed verification, real receipts and before/after source
snapshots. Argument count/size and NUL bounds remain; the harness does not concatenate a shell
command. `allowed_tools` stays explicit, while the production registry checks actual availability
instead of a fixed six-tool list. Source hashes are checked on load and `file_unchanged` assertions
reject modified check files even when their commands report success. Assistant text is not proof.
The campaign deadline narrows the existing process timeout and awaits the actual result instead
of dropping a running verification future.

Compare engines using the same fixtures and harness. Record engine commit, harness patch SHA,
binary SHA, model, permissions, tools and case ordering separately. A baseline with an evaluation
harness patch is not an untouched old binary. Ablations require observed outcomes and an explicit
retain/remove decision.

## Artifacts

The output directory is created once and contains:

- `results.jsonl`: schema V3 source of truth, one record per repetition;
- `manifest.json`: campaign counts, cost, and exact trend buckets;
- `summary.md`: human-readable projection;
- `trajectory.jsonl`: per-turn identity, terminal, usage/cache, price sources, retained process-local stage timings, durable provider/tool/check observations, and separate budget reservation/accounting;
- `<fixture>-<repetition>.patch`: actual UTF-8 unified diffs for declared and audit-observed files, with before/after hashes in the side table; binary, oversized or unreadable files are explicitly incomplete;
- per-run generated workspace, secret-free config, and V2 durable session.

`known_usage_cost_usd` is the subtotal of observed usage with price evidence. It can omit an
unmeasured failed or cancelled request and is never a complete bill. `usage_coverage` associates
formal usage with exact durable physical-attempt terminal output references, session identity and
start-event correlation; event counts alone cannot prove coverage. Missing terminals, missing or
unassociated usage, incomplete streams and unobserved child-session usage keep the complete
`reported_or_priced_cost_usd` and report total unknown. Typed confirmed-no-model-consumption
attempts are counted separately; absence of usage is not evidence of zero cost. A complete usage
set still requires price evidence for every event. Snapshot-priced totals have `estimated`
confidence; they are not provider invoices or claims that a historical snapshot is current pricing.
Unavailable coverage observations produce unknown evidence without changing execution, patches or acceptance.
`budget_accounted_microusd` remains conservative budget accounting, at least the known priced
subtotal or the reservation, not an actual bill. Current tool audits cannot establish repeated reads of an identical path/range without an
intervening write, so `redundant_reads` is unknown. `human_interventions` and
`ineffective_repair_rounds` are also unknown: approval counts and failed checks alone do not prove
those facts. Derived trajectory artifacts grant no authority.

Each `turns[].stage_timings` side table consumes the existing bounded kernel diagnostic buffer
immediately before and after that actual turn. It retains only new observations from the same
process instance and exact run timing key, preventing another concurrent run from filling gaps.
Each fixed phase contains an `elapsed_us` array in observation order, or `null` when no matching
sample survived. Repeated provider attempts and tools remain separate samples; their durations
are not summed into a turn duration. The output contains phase names and durations, not timing
keys, process identity, absolute timestamps, prompt content, endpoints or credentials.

`snapshots_available = false` means the two snapshots could not establish a same-process window;
all phase values then remain `null`. `global_dropped_during_turn` is only the process-wide buffer
drop/eviction delta over that window. It cannot establish how many observations this run lost, or
whether its retained samples are complete. Missing observations never affect run admission,
acceptance or the next turn. No UI is sampled by this runner, so `first_feedback_frame` remains
`null`; provider first content is not UI paint. The original frozen campaigns are not backfilled.

Phase boundaries follow [prompt-response diagnostics](prompt-response-latency.md#分段诊断):
preparation includes its nested session/provider/tool/context work, provider stream/first-item/
first-content timings share the physical attempt's dispatch origin, and tool execution excludes
approval waiting. These overlapping values have different origins and must not be added together.
The existing turn wall time still measures its execution and final-turn verification; there is no
new independent verification-duration observation.

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

### Unchanged and failed trajectories

Runs with no file edits still publish a real zero-byte `.patch` and an empty file-diff list, retaining failed or paused outcomes. The release-output owner accepts empty files while preserving its fixed root, one-shot capsule, create-new behavior, and byte bounds. An empty patch is not a successful repair and must not prevent the remaining report from being exported.

An unclassified execution error is reported as `unknown`, not a model failure: the same execution boundary includes provider transport, runtime, tools, persistence and cleanup. Independently observed verification failures retain their own category. The report does not infer failure ownership from error text or expose raw errors that may contain private data.

### Independent long-history smoke

`long-session-v1/composer-handoff` keeps twelve fixed user turns in one session. It combines a planted defect in the repository's Desktop composer function with 149,235 bytes of synthetic maintenance history. The final independent check verifies the real function behavior, hashes of early/middle/final handoff values, and unchanged check/package files. Each prompt stays below the existing 16 KiB limit; no product budget is raised for this fixture.

This is separate from the frozen twenty-case `repository-v1` comparison. The model may externalize history in its replies or files, so passing demonstrates continued work with history, not unaided recall or a blind benchmark. Report actual turns, provider token usage and patch evidence. It does not qualify overflow or compaction unless those paths were actually exercised; normal model-eval disables compaction. Use the provider/application qualification for that boundary.

### Observable activity subsets

`trajectory.activity` supplements the three deliberately unknown broad metrics with facts from the
one observed durable session stream. It is `null` when observation fails. It never changes run
acceptance, tool admission, billing, or verification. Child sessions are not scanned;
`observed_child_thread_starts` flags observed delegation but is not a completeness certificate.
These subsets must not be relabelled as whole-task totals.

- `reads` counts completed `read_file` audit events and the subset with a unique assistant call,
  started/completed audit, typed requested offset/limit, actual applied caps, and complete
  policy-preserved V3 artifact in the same session. Identity coverage is split into
  `canonical_subject_reads` and `managed_logical_subject_reads`: the first has an audited canonical
  subject; the second requires the exact workspace logical selector hash and the actual managed-file
  access receipt retained in V3 facts. That receipt binds the borrowed workspace and operation;
  it does not expose or prove a canonical leaf/inode identity. The two identity kinds never
  cross-join. Ambiguous reused call IDs,
  missing bindings, truncated outputs and other scopes are excluded explicitly. Repeated output
  means the same identity-kind/binding/range and persisted content hash/size were observed more than once.
  Managed reads compare the same workspace logical selector; path aliases and replacement leaf files
  are not inferred from this metric. Missing or bounded-away access receipts remain excluded.
  `repeated_persisted_output_bytes` adds only the artifact's explicit persisted byte count for
  occurrences after the first. It is not physical disk I/O or file size. The projection reads no
  current workspace file or artifact body. Re-reading after intervening edits may be necessary;
  even identical output does not prove redundancy. Both repeat fields are `null` with zero
  qualified reads; a measured zero with qualified reads applies only to that subset.
- `decisions` counts exact accepted tool-approval and user-input decisions with an earlier matching
  durable request. Exact duplicate decisions are counted once; missing or conflicting bindings
  have a separate count. This records decisions, including automated decisions when those are
  actually audited. It cannot distinguish a person from automation or count unaudited human
  activity, so `human_interventions` remains `null` even for an auto-approve campaign.
- `repairs` counts a typed `Repair` selection tied to its exact pending feedback, and its next new
  feedback in the same Task, admission and execution attempt. `failed_rechecks_after_repair`
  counts only pairs with new failed receipts for at least one identical check, policy, workspace,
  verification scope, environment and sandbox binding. Both sets must match their respective
  feedback snapshot. Changed checks/policies, missing receipts and different attempts cannot
  establish a failed repeat. Unpaired selections and excluded pairs remain explicit; without a
  comparable pair the value is `null`. A selection followed by no further feedback could mean
  success, cancellation, interruption or missing evidence. This is not proof that an edit was
  ineffective, and does not convert an ordinary failed check into a repair round.

The original `redundant_reads`, `human_interventions` and `ineffective_repair_rounds` remain `null`.
No natural-language classification, synthetic human attribution or extra execution owner is
introduced. Reports expose counts and coverage, not paths, prompt text, decision IDs or output
bodies. Frozen historical campaign reports are not backfilled; run a new campaign to collect this
additional side table.

### 活动指标的可观察边界

`trajectory.activity` 只统计本次实际读取的一条持久 session 流；读取失败为 `null`，不会阻碍执行或改变费用与验收。
子会话不在统计范围内，已观察到的 child 启动次数仅是覆盖提示，不能代表整个任务树完整。

重复读取子指标要求唯一 call、真实 started/completed 审计、typed 范围及同 session 完整 V3 artifact 同时匹配。
身份覆盖分为 `canonical_subject_reads`（已有 canonical subject）和 `managed_logical_subject_reads`（workspace 逻辑选择器 hash 与 V3 中实际 managed-file access receipt 同时存在且匹配）。后者的 receipt 绑定借用 workspace 和操作，不等于物理 leaf/inode 身份；两种身份不会交叉去重。
受管读取只比较同一 workspace 的相同逻辑选择器，不推断路径别名或文件替换；receipt 缺失或被 bounded facts 截断仍计入排除项。
相同范围与持久化内容的重复观测单独计数；字节来自 descriptor 的 `persisted_bytes`，表示持久化安全输出字节，不是物理文件读取字节。
统计不会再读当前文件反推历史。缺少或歧义的绑定、截断输出均计入排除项；没有合格样本为未知，不能用零冒充。
相同内容也可能是必要的重读，因此不填写宽泛的 `redundant_reads`。

审批及用户输入子指标只统计与先前请求精确关联的已接受决定，重复审计去重；无法区分人操作与自动批准，因此 `human_interventions` 仍未知。
修复子指标只统计同 Task/admission/attempt 的显式 `Repair` 后，再次反馈中可比的同一 check 仍失败的事实。
新 receipt 必须晚于该选择，且分别对应前后 feedback 的 snapshot；策略、范围、环境、sandbox 与 check 身份都要匹配。
没有下次反馈不代表成功或失败；没有可比证据为未知。此指标不能证明具体修改无效，因此 `ineffective_repair_rounds` 仍未知。
所有结果都是可观察的严格子集，保留未配对与排除计数；不新增权限 owner，不用自然语言猜测原因，不回填旧实验。
