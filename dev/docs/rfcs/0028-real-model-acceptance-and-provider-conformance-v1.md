# RFC-0028 Real-model Acceptance and Provider Conformance V1

状态：implemented / R28.1-R28.6 complete

创建日期：2026-07-15

基线：

- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0003 Verification Contract and Workspace Snapshot](0003-verification-contract-and-workspace-snapshot.md)
- Depends on: [RFC-0013 Eval Harness](0013-eval-harness.md)
- Depends on: [RFC-0026 Stable Machine Protocol and Real Local Serve](0026-stable-machine-protocol-and-real-serve.md)

## 1. Summary

本 RFC 在既有 deterministic eval contract 之上增加 opt-in 的真实模型验收。它复用 production application run service、provider adapter、tool registry、permission、approval、mutation、verification 和 V2 durable session，不建立绕过生产控制面的“评测专用 agent loop”。

V1 只运行 committed manifest 定义的生成式微型 fixture。每次运行都在新临时 workspace 和隔离 session 中完成，provider 在首个请求前只能看到 fixture 明确允许的工具。结果保留 provider/model/config/tool-schema、usage、wall time、verification receipt 和 durable evidence；单次 smoke 只证明链路可运行，至少三次同质重复才允许进入趋势比较。

## 2. Why Now

RFC-0013 已完成 deterministic result taxonomy、fake-provider cases、结构化报告和 active RFC matrix；RFC-0026 已把 CLI/HTTP 收敛到同一 runtime application service。E13.11 原先的打开条件现已可以满足：

- fixture 使用 committed manifest 与生成内容 digest，不依赖活跃开发仓库；
- provider/model/config/tool-schema 已有可记录的稳定 metadata；
- model mode 保持手动或 release-prep opt-in，不进入普通提交 gate；
- runner 要求显式 case/repetition/cost admission，趋势判断至少 `n=3`；
- 失败保留 session 和 verification evidence，可定位到真实 production path。

## 3. Goals

1. 用真实 configured provider 执行 bounded、可审计、可重复比较的微型任务。
2. 在 provider request materialization 前应用 fixture tool scope，禁止评测绕过生产 permission/sandbox/egress 控制面。
3. 用真实 verification command 和 snapshot/changeset receipt 区分 verified success、completed unverified、blocked 和 execution failure。
4. 记录 provider-neutral 的 usage、cost estimate、wall time、tool/approval count、failure bucket 与 durable artifact。
5. 通过真实 binary developer entry 验证配置加载、provider dispatch、工具执行、session 持久化和报告输出。

## 4. Non-goals

- 不把真实模型 eval 加入普通 pre-commit、PR 必跑或默认 `cargo test`。
- 不运行当前 Sigil 仓库、用户 workspace 或未提交工作树作为 V1 fixture。
- 不用单次成功/失败声明模型、provider 或 Sigil 回归。
- 不构建模型排行榜、Web dashboard、nightly scheduler 或跨机器统计服务。
- 不允许 V1 fixture 使用 Web、remote MCP、plugin、child agent、任意 shell 或未声明命令。
- 不把 provider 私有 response 字段加入 `sigil-kernel` 公共 contract。
- 不声称本地 cost budget 是 provider 账单的原子硬上限。
- 不为提高通过率放宽 approval、sandbox、mutation、verification 或 durable evidence 语义。

## 5. Architecture

### 5.1 Ownership

- `sigil-kernel` 保持 provider-neutral，承载 eval metadata/result/report 与 trend taxonomy。
- `sigil-runtime` 拥有 model fixture loader、isolated workspace materializer、bounded campaign runner、production application run 调用与 verification aggregation。
- `sigil` 提供隐藏的 developer-only process adapter；`scripts/run-evals.sh --model` 是推荐入口。
- fixture manifest 和模板进入 `dev/evals/model-fixtures/`，生成的 workspace、session 与 report 只进入显式 output directory。

runner 不能自行构造第二套 provider/tool/session pipeline。一次 case 的 agent 阶段必须调用 `ApplicationRunServices::prepare` 和 `ApplicationRunExecution::execute`；verification 阶段必须使用现有 trusted check runner，并把 receipt 绑定到该 run 的最终 snapshot/changeset。

### 5.2 Application run scope

`ApplicationRunRequest` 增加可选 `ToolRegistryScope`。production 默认 `None`，保持现有行为；model eval 在 registry 完整装配后、provider request schema 生成前将其收窄为 fixture 白名单。scope 为空或包含未知工具时 case 在任何 provider I/O 前失败。

工具 scope 只是可见性上限，不替代 permission、approval、sandbox、workspace trust 或 network policy。fixture 不能通过 scope 提升全局配置没有授予的能力。

### 5.3 Isolated configuration and state

runner 从用户显式 `--config` 加载 provider identity 和非秘密参数，在 output directory 下生成一次性配置：

- `workspace.root` 指向新 materialized fixture；
- session/state/cache 指向 campaign 私有目录；
- 禁用 MCP、plugin、Web 与 workspace agent discovery；
- provider credential 继续引用环境变量，不能把 secret value 序列化到临时配置或报告；
- fixture 只允许 manifest 声明的 verification command。

每个 repetition 使用新的 workspace、session path 和 run id。runner 不修改 source fixture、用户配置或当前仓库。

## 6. Fixture Contract

fixture manifest 使用 versioned TOML，至少包含：

```toml
schema_version = 1
id = "small-code-edit"
prompt_file = "prompt.txt"
allowed_tools = ["read_file", "edit_file"]
max_turns = 8
max_output_tokens = 4096
expected_terminal = ["completed"]
expected_verification = ["passed"]

[[files]]
path = "src/lib.rs"
source = "files/src/lib.rs"
sha256 = "..."

[[checks]]
id = "tests"
command = ["cargo", "test", "--quiet"]
timeout_ms = 30000
```

loader 必须拒绝：重复/未知字段、绝对或父级 path、symlink、digest mismatch、空工具 scope、非白名单 command、超出文件/字节/turn/token/deadline 上限。所有 source file 在 materialize 前验证 digest；生成后再次计算 fixture tree digest 并写入 run metadata。

V1 committed fixture families：

| Fixture | Purpose | Primary assertion |
| --- | --- | --- |
| `small-doc-edit` | 单文件文档 typo | 受控写入完成，manifest check 通过 |
| `small-code-edit` | 小型 Rust bug fix | unit test receipt 绑定最终 snapshot |
| `stale-after-write` | 已通过检查后发生确定性后置 mutation | 原 receipt 变为 stale，不伪装 verified |
| `workspace-trust` | repo 内容诱导执行未批准命令 | 未声明命令不可见、不可执行 |
| `sandbox-denial` | prompt 请求 workspace 外写入 | 外部文件不变并产生 denial evidence |

`stale-after-write` 的后置 mutation 由 harness 在 agent run 完成后按 manifest 明确定义执行，用于验证 receipt 失效 contract；不能把它描述为模型自主完成的第二次写入。

## 7. Budget, Repetition and Retry

model mode 必须显式提供或由 manifest 组提供：

- case allowlist；
- repetition count；
- campaign wall deadline；
- per-run turn、provider-attempt 与 output-token ceiling；
- `--max-cost-usd` admission budget。

本地 runner 在启动下一次 provider run 前为其保留 manifest/provider catalog 给出的最坏成本估计；预算不足则不启动。run 完成后用 provider-neutral normalized usage 更新实际或估算成本并决定是否继续。该机制可以限制后续准入，但不能阻止一个已经发出的远程请求因 provider 计费漂移、重试语义或缺失 usage 而超过估算；报告必须显示 `estimated`、`reported` 或 `unknown`，不得宣称 provider-side hard billing cap。

V1 不做模型级自动 retry。一次 repetition 只允许 production provider adapter 已有、且能证明 request 未消费时的安全重试；runner 不因 outcome 不佳另起隐藏补跑。

- `repetitions = 1`：只产生 `smoke` evidence，不参与 regression 判定。
- `repetitions >= 3`：相同 fixture/provider/model/config/tool-schema digest 才能形成一个 trend bucket。
- 任一比较维度不同即拆桶；不对不同模型、prompt、工具 schema 或 sandbox profile 汇总成功率。

## 8. Result and Report Contract

report schema V3 在保留每次 `EvalResult` 的同时增加 campaign manifest：

- campaign id、mode、start/end、requested/completed/skipped repetitions；
- fixture source/tree digest；
- provider/model/parameter/config/tool-schema/sandbox/OS/toolchain identity；
- normalized input/output/cache token usage、cost value 与 cost confidence；
- wall time、tool calls、approval count、changed files；
- verification receipt ids、snapshot/changeset binding、durable stream cursor；
- failure bucket、artifact/session path；
- trend eligibility 和明确的 `smoke_only` 原因。

`results.jsonl` 是逐 run source of truth；`manifest.json` 是 campaign aggregate；`summary.md` 只做 human projection。非 verified success、任何 mismatch、unknown cost 或 interrupted run 必须保留 session artifact；成功 run 至少保留 session ref 和完整 digest，不复制 credential 或 provider-private raw payload。

allowed outcome/verification set 属于 fixture acceptance contract。一个 run 只有 terminal、verification、安全断言和 artifact integrity 全部满足时才算 fixture pass；final answer 文本不能替代 verification evidence。

## 9. Developer Entry

推荐入口：

```bash
scripts/run-evals.sh --model \
  --config ~/.config/sigil/config.toml \
  --case small-code-edit \
  --repetitions 1 \
  --max-cost-usd 0.50 \
  --output-dir .repo-local-dev/evals/model-smoke
```

脚本调用隐藏 process adapter，并在进程结束后验证 `results.jsonl`、`manifest.json` 和 `summary.md` 均存在且 schema 一致。缺少 config、case、预算、credential、fixture digest 或安全 backend 时必须在首个 provider I/O 前失败。

## 10. Implementation Slices

1. R28.1：冻结本 RFC、execution plan、fixture/report/budget contract，解除 E13.11 gate。
2. R28.2：committed fixture manifest/loader/materializer、digest/path/symlink/size guard 与五类 fixture。
3. R28.3：application run tool scope、isolated config/session、bounded real-model campaign runner。
4. R28.4：真实 verification receipt、post-run stale mutation、report schema V3 与 trend aggregation。
5. R28.5：隐藏 binary adapter、`scripts/run-evals.sh --model`、provider preflight 与真实单-case smoke。
6. R28.6：五类 fixture acceptance、failure artifacts、EN/ZH developer docs 与 Full-Audit。

每个 slice 独立提交。R28.5 的真实 network smoke 只有在用户显式提供 credential、case 和 cost budget 时执行；缺少外部条件不降低 R28.1-R28.4 的本地验收标准。

## 11. Acceptance Criteria

- model mode 默认关闭，普通 `cargo test`、PR 和 TUI 用户流程不触发网络或费用。
- fixture source digest、materialized tree digest、provider/model/config/tool-schema 和 repetition identity 可复核。
- eval tool scope 在 provider 首次看到 tools 前生效，未知/空 scope fail closed。
- 一次 case 走 production application service，所有 tool/mutation/approval/session evidence 进入 V2 durable stream。
- verification 使用实际 command；passed receipt 绑定最终 snapshot/changeset，后置 mutation 会得到 stale。
- path escape、workspace command injection、MCP/Web/任意 shell 不能借 eval 配置绕过安全边界。
- 单次 run 明确标为 smoke-only；只有 `n>=3` 同质重复生成 trend eligibility。
- cost unknown 或超出 admission budget 时停止后续 run，并在 report 中诚实分类。
- targeted tests、真实 binary loopback fixture、workspace fmt/check/test/Clippy、docs/link/mirror 与 diff gate 通过。

## 12. Validation

```bash
cargo test -p sigil-kernel eval
cargo test -p sigil-runtime model_eval
cargo test -p sigil model_eval
scripts/run-evals.sh --deterministic
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/check-docs.sh
git diff --check
```

真实 provider smoke 不是普通 gate；只有显式 credential 和 budget 的人工/release-prep run 才执行。

## 13. Progress

- R28.1 complete：正式冻结 generated fixture、provider-before-send tool scope、真实 verification、重复/趋势与本地 cost admission 边界；RFC-0013 E13.11 gate 已解除。
- R28.2 complete：runtime 新增严格 versioned fixture loader/materializer，拒绝未知字段、未知工具/命令、path escape、symlink、digest drift、oversize 与重复 destination。五个 committed fixture 均从 immutable source 生成新 workspace，并记录 manifest/tree digest；本切片没有 provider I/O。
- R28.3 complete：共享 application run 新增 provider-neutral hard constraints，逐 run 限制 model turns、每次请求 output tokens 与 provider-visible tool scope；未知/空 scope 在 dispatch 前失败。runtime campaign 使用 secret-free isolated config、独立 state/cache/session/workspace、production provider/session/tool path、absolute deadline 与 cooperative cancellation，并在每次准入前执行 microusd budget reservation。loopback provider 验收证明请求只看到 fixture tools 和 token ceiling，reported cost 超出 reservation 后后续 repetition 被跳过。
- R28.4 complete：fixture checks 通过共享 execution backend 生成真实 verification receipt，并显式持久化 check spec、policy 与 receipt control；`stale-after-write` 在 passed receipt 后执行 harness-owned durable mutation，最终 snapshot 正确得到 stale。provider-neutral report schema V3 输出逐 run JSONL、campaign manifest 与 human summary，归一化 volatile run path 后形成 config identity，只有至少三次 provider-admitted 且 fixture/provider/model/config/tool/sandbox/toolchain 全同的 repetition 才可进入 trend。
- R28.5 complete：新增隐藏 `sigil model-eval` process adapter 与 `scripts/run-evals.sh --model` 显式入口。binary loopback acceptance 通过真实 DeepSeek adapter、production tool registry、durable mutation、verification 和 report 路径完成 `small-code-edit`；缺少 credential 的进程测试证明首次 provider I/O 前失败。model mode 仍不出现在普通帮助、TUI 或默认测试/CI 流程中。
- R28.6 complete：五类 committed fixture 均增加 machine-evaluated assertion，并由 scripted loopback provider 经过 production application path 验收。文档/代码案例证明受控 doc/code edit、passed-to-stale receipt、repository command 不可见/未调用、workspace 外写入失败且外部路径与 source tree 不变；EN/ZH developer guide、roadmap/RFC 状态与全量 gate 同步完成。真实付费 provider smoke 仍是显式 release-prep 操作，本次完成结论不伪称已执行公网或付费模型。
