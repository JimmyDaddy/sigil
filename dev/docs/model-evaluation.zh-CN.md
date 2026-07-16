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

active provider 的 credential 必须通过正常环境变量或 secret source 提供。生成的隔离配置会移除内联 secret 字段，并关闭 Web、MCP、skills、memory、task delegation 和非 active provider。

`--max-cost-usd` 只是本地准入与停止预算，不能对已经发出的请求形成 provider-side billing cap。单次 repetition 只属于 smoke evidence；只有至少三次 provider-admitted repetition 且 fixture、provider、模型参数、归一化配置、tool schema、sandbox backend、OS 与 toolchain identity 全部一致时，才能进入 trend。

## 已提交案例

- `small-doc-edit`：受控文档编辑与 verification。
- `small-code-edit`：受控 Rust 源码编辑与 unit-test receipt。
- `stale-after-write`：先生成 passed receipt，再由 harness 执行 durable mutation；最终 verdict 必须为 stale。
- `workspace-trust`：仓库内指令不能暴露或调用任意 shell 工具。
- `sandbox-denial`：workspace 外写入被拒绝，外部路径保持不存在，committed fixture source 保持不变。

每个 manifest 都包含机器执行的 assertion；assistant final text 永远不能替代证明。

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

Source config 只用于选择 active provider/model 和无 secret 的 provider 选项。Active credential 必须位于该 provider 文档规定的环境变量中；inline key 永远不会复制到生成的 Plan config。Raw PTY/session/model artifact 只保留在显式选择且已忽略的本地 output。Aggregate budget 仍只是准入与记账限制，不能充当已经发出请求的 provider-side billing cap。

## 产物

output directory 只创建一次，包含：

- `results.jsonl`：schema V3 source of truth，每次 repetition 一条记录；
- `manifest.json`：campaign 计数、成本与精确 trend bucket；
- `summary.md`：供人阅读的 projection；
- 每次 run 的 generated workspace、无 secret config 与 V2 durable session。

未通过验收的 run 仍可通过 session artifact path 和结构化 mismatch reason 定位。不要提交生成的 campaign 目录或 credential。

## Deterministic 模式

不需要模型调用时，使用 fake-provider conformance suite：

```bash
scripts/run-evals.sh --deterministic
```

Deterministic 结果只证明本地 contract，不能表述为真实模型成功率。
