# 真实模型评测

Sigil 的真实模型评测是仅供开发者显式执行的验收流程。它让 committed generated fixture 经过生产 provider、tool、permission、mutation、session 和 verification 路径；它不属于 TUI、普通帮助、默认 `cargo test` 或 PR 必跑检查。

## 执行一次 smoke

```bash
scripts/run-evals.sh --model \
  --config ~/.config/sigil/config.toml \
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

