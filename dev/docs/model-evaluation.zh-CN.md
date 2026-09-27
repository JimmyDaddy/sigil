# 真实模型评测

Sigil 的真实模型评测是仅供开发者显式执行的验收流程。它让 committed generated fixture 经过生产 provider、tool、permission、mutation、session 和 verification 路径；它不属于 TUI、普通帮助、默认 `cargo test` 或 PR 必跑检查。

## 执行一次 smoke

```bash
scripts/run-evals.sh --model \
  --config ~/.sigil/sigil.toml \
  --case small-code-edit \
  --repetitions 1 \
  --max-cost-usd 0.50 \
  --output-dir .repo-local-dev/evals/model-smoke
```

active provider 的 credential 必须通过该 provider 对应的环境变量提供（DeepSeek 为
`SIGIL_API_KEY`）。生成的隔离配置会移除内联 secret 字段并关闭 Web、MCP、skills、memory
和非 active provider；默认关闭 task delegation。只有 fixture 显式声明
`agent_delegation = true` 并将 `spawn_agents` 放进工具 allowlist 时，评测才启用主动只读委派，
同时把任务路由固定为 `manual`，不测试或启用自动路由。因此 source config 中即使存在内联 AK，
也不会作为评测 credential 使用。缺少环境变量时 campaign 会在创建输出目录和 provider dispatch
前失败。

`--max-cost-usd` 只是本地准入与停止预算，不能对已经发出的请求形成 provider-side billing cap。单次 repetition 只属于 smoke evidence；只有至少三次 provider-admitted repetition 且 fixture、provider、模型参数、归一化配置、tool schema、sandbox backend、OS 与 toolchain identity 全部一致时，才能进入 trend。

## 执行 RFC-0053 orchestration 候选评测

O8c 使用冻结的 `orchestration-v1` corpus：20 个 Chat、15 个 PlanReview、15 个 DirectTask
case，每个 case 至少 3 次同质 repetition。执行前先确认 committed corpus 未漂移：

```bash
node dev/evals/generate-orchestration-corpus.mjs --check
```

候选 release owner 还必须先从同一官方 route 的 provider probe 取得真实
`system_fingerprint`。config 必须把默认模型解析为精确的 `deepseek-v4-flash`，然后在同一
release build 中生成 route-contract 与 campaign binary：

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

这个 fingerprint 是 release-owner 的显式 live observation，不能从 tokenizer parity 常量或
model alias 推导。route contract 不是普通配置，也不能从评测结果反推；其中 provider kind、
endpoint family、canonical model version、
routing/direct-Task/system prompt digest、tool/profile contract digest、Sigil commit 与 build 必须来自
同一候选构建的发布元数据。占位值、旧 build 的 digest 或可漂移 alias 会让报告失去 rollout
资格。routing digest 绑定模型可见的普通对话契约，以及当前的
`request_plan_review`、`start_task`、Task continuation 和 pending-Plan 工具 schema；direct-Task
digest 绑定 direct execution contract，不存在独立 planner prompt 或 schema；system digest
还绑定 participant execution contract；host 不使用 prompt 关键词分类器。campaign 在
provider dispatch 前会根据 exact candidate binary、config、完整 corpus、prompts 与 tool
profiles 重新推导并比对 contract；只有这个完全匹配的 DeepSeek candidate 才获得用于 sidecar
生成前资格验证的 evaluation-only DirectTask capability。文件使用以下 V1 字段：

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

然后显式运行完整候选 campaign：

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

上面的成本只是示例性的本地准入上限，不是 provider 侧账单封顶；运行前应按目标 route 和模型
价格重新确认。Campaign 不允许混合普通 fixture 与 orchestration fixture。除通用 V3 产物外，
还会生成 `orchestration/results.jsonl`、`orchestration/manifest.json` 和
`orchestration/summary.md`。每个 exact route 独立计算 `qualified`、`insufficient_evidence`、
`blocked` 或 `stale`；只有同一候选 release 的 `qualified` report 才能进入 O8d，其他 route 继续
保持 `manual + explicit_request_only`。

## 已提交案例

- `small-doc-edit`：受控文档编辑与 verification。
- `small-code-edit`：受控 Rust 源码编辑与 unit-test receipt。
- `stale-after-write`：先生成 passed receipt，再由 harness 执行 durable mutation；最终 verdict 必须为 stale。
- `workspace-trust`：仓库内指令不能暴露或调用任意 shell 工具。
- `sandbox-denial`：workspace 外写入被拒绝，外部路径保持不存在，committed fixture source 保持不变。
- `agent-collaboration-v1`：要求模型通过 `spawn_agents` 并行启动两个只读 `explore` agent；机器断言验证同一批次至少两个子 agent 均已完成且结果完整送达，父模型只读核对调用链并综合结果。

每个 manifest 都包含机器执行的 assertion；assistant final text 永远不能替代证明。

当正确行为是向用户请求持久化输入时，fixture 可以把预期终态设为 `paused`，并搭配
`user_input_pending` assertion。这样只有 session 确实保留未回答请求时才会通过；普通暂停不能满足该断言。

## 执行 RFC-0034 dogfood 矩阵

在作出 alpha readiness 结论前，使用同一个精确的预构建 binary，一次执行已提交的 edit、verification、trust、sandbox 与 Plan-only 案例：

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

Runner 在发出请求前准入并冻结 binary，把一个本地成本预算分配给全部计划 repetition，并保证 aggregate evidence 不包含 prompt、provider、config 或 session 内容。`plan-only` 通过 PTY 驱动 production TUI `/plan` 路径；其生成配置不含 secret，设置四轮保险丝与 read-only 权限，并关闭 Web、MCP、skills、memory 和 task。案例必须得到一个 durable structured Plan draft、可见的 Plan review surface 和持久化 usage，同时不得发生 plan-to-task handoff，workspace 也必须保持不变。

Runner 在重设各 child HOME 之前解析已经准入的 direct Rust toolchain，并把其 `bin` 目录放到
model case PATH 最前面。它不继承 `RUSTUP_HOME`、`CARGO_HOME`、Cargo credential 或 Cargo
registry state，因此 fixture check 不会隐式触发 rustup 安装。

Source config 只用于选择 active provider/model 和无 secret 的 provider 选项，只接受当前
connection schema。Plan harness 只保留 active route，把 stored credential
reference 替换为 environment reference，并且不会把 connection label、credential ID、inline key
或 inactive connection 复制到生成配置。Active credential 必须位于其配置的环境变量中。Raw
PTY/session/model artifact 只保留在显式选择且已忽略的本地 output。Aggregate budget 仍只是准入
与记账限制，不能充当已经发出请求的 provider-side billing cap。

## 固定仓库回归与同会话多轮

`dev/evals/model-fixtures/repository-v1` 固定 20 个来自真实仓库模块的故障注入任务。
来源提交、文件与检查 hash 写入材料；这类受控源码子集回归不能推导完整仓库历史 issue 的通用成功率。

schema 1 可选 `[[followup_prompts]]`，每项包含 `prompt_file` 与 `prompt_sha256`，不额外限制轮数。
初始和后续 prompt 与工作区文件共用已有的 1 MiB fixture 源码总预算，每条 prompt 仍限 16 KiB。
初始 prompt 与后续固定用户 turn 使用同一实际准入的 session，每轮有独立 run ID；前轮真实
join 且终态 completed 才提交下一轮。输入等待、取消、失败不自动继续，全部轮次共享本案例的
campaign 剩余期限。独立检查与断言在最后一轮后验收，并同时检查此前要求的结果。

fixture 的 `checks.command` 是显式可信的 direct argv，不再限制为两条 Cargo 命令；Python、
Node 等使用相同 managed verification、真实执行 receipt 和源码前后快照。argv 有数量、大小与
NUL 边界，不拼接 shell。`allowed_tools` 必须显式列出，实际是否注册仍由生产 registry 校验，
不使用固定六工具名单。检查材料的 hash 在加载时确认，`file_unchanged` 独立断言会拒绝模型
改写检查文件后制造的通过；assistant final 不能替代这些证据。评测 deadline 收窄原检查进程的
有限 timeout，并等待真实执行结果，不通过丢弃 verification future 终止等待。

新旧 engine 对照必须使用相同 fixture 和评测 harness，单独记录 engine commit、harness patch
SHA、binary SHA、模型、权限、工具和任务顺序。如果旧 engine 应用了新评测 harness，不能将其
描述为完全未修改的旧 binary。消融要记录实际结果与 retain/remove 理由，不能用源码推测代替。

## 产物

output directory 只创建一次，包含：

- `results.jsonl`：schema V3 source of truth，每次 repetition 一条记录；
- `manifest.json`：campaign 计数、成本与精确 trend bucket；
- `summary.md`：供人阅读的 projection；
- `trajectory.jsonl`：固定各轮的 run ID、终态、用量/缓存、价格来源、durable provider/tool/check 轨迹，以及预算预留/记账；
- `<fixture>-<repetition>.patch`：声明文件和审计发现的修改文件的实际 UTF-8 unified diff，侧表保留 before/after hash；二进制、超界或不可读取明确标记，不能声称完整；
- 每次 run 的 generated workspace、无 secret config 与 V2 durable session。

侧表中的 `known_usage_cost_usd` 是已观察且有价格证据的费用小计；失败或取消请求缺失用量时，
它可能遗漏这次请求，不能当作完整账单。`usage_coverage` 使用实际 attempt terminal 的输出引用、
同 session 身份与 start-event correlation 精确关联正式 usage，不能用事件计数相等代替覆盖证明。
缺失 terminal、缺失或未关联 usage、未完整结束的流、未观察的子会话费用，都会让完整
`reported_or_priced_cost_usd` 与报告总费用保持 unknown。真实 typed confirmed-no-model-consumption
单独计数；没有 usage 不等于免费。完整覆盖仍要求每条 usage 有价格证据，价格快照换算的总额
标记 `estimated`，不声称是供应商账单或已核实当前价格。观测读取/投影不可用只产生 unknown，
不改变任务结果、补丁和验收。`budget_accounted_microusd` 仍取至少已知费用小计或预留值进行
保守预算记账，不能当作实际账单。
当前 durable 工具审计不足以证明“同 path + range、期间无写入”的重复读取，`redundant_reads`
明确为 unknown，不根据 read 次数猜测冗余。轨迹是既有执行事实的派生产物，不提供权限。

未通过验收的 run 仍可通过 session artifact path 和结构化 mismatch reason 定位。不要提交生成的 campaign 目录或 credential。

## Deterministic 模式

不需要模型调用时，使用 fake-provider conformance suite：

```bash
scripts/run-evals.sh --deterministic
```

Deterministic 结果只证明本地 contract，不能表述为真实模型成功率。

该入口同时检查生成的 orchestration corpus 未漂移，并执行 RFC-0053 的 permission、whole-batch、
reverse completion、429、cancel/restart、approval、integration lane 与 cleanup inventory
deterministic gate；它仍不替代真实 provider campaign 或 PTY 产品验收。

### 无改动与失败轨迹

没有文件改动时，评测仍发布真实的零字节 `.patch` 与空文件差异清单，并保留失败/暂停的运行结果。release output owner 允许空文件，仍校验固定输出根、单次 capsule、不可覆盖与字节上限。空 patch 不能被视为成功修复，也不应阻断其余报告导出。

未分类的执行异常记为 `unknown`，不能直接归因于模型：同一执行边界还包含 provider 传输、runtime、工具、持久化与清理。独立观察到的验证失败仍保留自身分类。报告不根据错误文案猜测责任来源，也不泄露可能含私密数据的原始异常。

### 独立长历史样本

`long-session-v1/composer-handoff` 在同一会话中发送12个固定用户turn，结合真实Desktop composer函数的故障切片与149,235字节合成维护历史。末轮独立检查验证真实函数行为、首/中/末轮交接值的SHA256，以及检查与package文件未被修改。每个prompt均在既有16KiB限制内，没有增加产品预算或用户准入。

它独立于已冻结的 `repository-v1` 20项比较。模型可以在回复或文件中外化历史，故通过仅证明带历史的继续工作，不证明无辅助记忆或盲测能力。结果报告实际执行turn、provider token用量和patch；没有实际触发溢出/压缩就不声称覆盖，普通model-eval关闭压缩，对应能力另查provider/application资格证据。
