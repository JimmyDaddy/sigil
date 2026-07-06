# Sigil 配置指南

[文档首页](README.md) · [快速上手](quickstart.md) · [Provider 指南](providers.md) · [排障](troubleshooting.md) · [参考](reference.md) · [English](../en/configuration.md)

本文说明 Sigil 的用户配置方式。大多数用户应该先使用 TUI 中的 Quick Setup；当你需要可重复配置文件、环境变量配置、自动化行为或高级工具策略时，再读这一页。开发者需要修改配置 schema 时，请同步阅读 `dev/governance/code-standards.md` 和 `dev/governance/engineering-standards.md`。

## 常见用户路径

| 目标 | 推荐路径 |
| --- | --- |
| 第一次本地 setup | 运行 `sigil` 并完成 Quick Setup |
| 临时本地认证 | 启动前设置 `SIGIL_API_KEY` |
| CI 或脚本认证 | 使用环境变量，不把 key 写进 plaintext config |
| 从 TUI 切换 model/provider | 使用 `/config` |
| 一份配置跟随启动目录 | 使用 `workspace.root = "."` |
| 调试 config/auth/provider 状态 | 运行 `sigil doctor` 或 `/doctor` |

## 配置查找顺序

TUI 和 CLI 按这个顺序找配置：

1. 命令行指定的 `--config <path>`
2. 用户可见 Sigil 配置目录里的 `sigil.toml`

默认用户配置路径：

- `~/.sigil/sigil.toml`

Quick Setup 写入用户配置路径。启动时如果 `~/.sigil/sigil.toml` 不存在，但旧的按平台划分的用户配置存在，Sigil 会把旧配置复制到 `~/.sigil/sigil.toml` 并使用新路径。workspace 根目录的 `sigil.toml` 默认不会被读取；如果需要临时实验配置，请显式传入 `--config <path>`。

## 推荐最小路径

对普通使用者，直接启动 TUI 并完成 Quick Setup：

```bash
sigil
```

临时使用或 CI 场景，可以在启动前通过环境变量提供认证：

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

如果没有配置文件，TUI 会进入 Quick Setup，并在保存后生成可用配置。后续可以用 `/config` 调整常用项。

## 用 Doctor 排障

当配置、认证、MCP 或本地 LSP 工具链看起来不对时，先运行 `doctor`：

```bash
sigil doctor
```

在 TUI 内可以用 `/doctor`，它会把同一份报告渲染到 transcript。TUI 版本会先显示状态汇总和 `needs attention` 修复清单，再展示完整 check 列表。

如果启动 Sigil 时使用了非默认配置，也传入同一个配置路径：

```bash
sigil --config ./sigil.toml doctor
```

报告会检查配置加载、workspace 解析、session log 位置、provider 设置、API key 来源、MCP command 与 trust 设置、code intelligence language server 可用性，以及当前 `TERM`。它只展示 API key 的来源，不会打印密钥值。warning 和 error 会附带 `fix:` 修复建议；如果 key 只来自明文配置，doctor 会给出 warning，提示你改用环境变量或确认本地配置不会被提交。

## 最小配置示例

如果需要手写配置，可以从这个结构开始：

```toml
[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[terminal]
keyboard_enhancement = "auto"
mouse_capture = false
osc52_clipboard = true
scroll_sensitivity = 3

[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"

[providers.deepseek]
fim_model = "deepseek-v4-pro"
# 推荐优先使用 SIGIL_API_KEY；如果写在这里，会以 plaintext 保存。
# api_key = "sk-..."
```

`SIGIL_API_KEY` 优先级高于配置文件里的 `api_key`。`doctor` 会对仅来自明文配置的认证给 warning，但不会阻止运行。

可复制模板位于 [docs/examples/config](../examples/config)：

- [deepseek-basic.toml](../examples/config/deepseek-basic.toml)
- [openai-compatible.toml](../examples/config/openai-compatible.toml)
- [anthropic.toml](../examples/config/anthropic.toml)
- [gemini.toml](../examples/config/gemini.toml)
- [mcp-safe-defaults.toml](../examples/config/mcp-safe-defaults.toml)
- [code-intelligence-rust.toml](../examples/config/code-intelligence-rust.toml)

Provider 细节已经拆到独立页面：

| Provider | 适合场景 | 详情 |
| --- | --- | --- |
| DeepSeek | 使用默认 Quick Setup 路径、DeepSeek chat 和 FIM 相关设置。 | [DeepSeek provider](provider-deepseek.md) |
| OpenAI-compatible | 使用 Chat Completions-compatible `/v1` endpoint，例如 OpenAI 或兼容网关。 | [OpenAI-compatible provider](provider-openai-compatible.md) |
| Anthropic | 使用 Anthropic Messages streaming 和 Claude model 设置。 | [Anthropic provider](provider-anthropic.md) |
| Gemini | 使用 Gemini `streamGenerateContent` 和 function calling 支持。 | [Gemini provider](provider-gemini.md) |

对比和可复制 provider block 见 [Provider 指南](providers.md)。

## Workspace

```toml
[workspace]
root = "."
```

`workspace.root = "."` 有特殊语义：`.` 会在启动时解析成运行 `sigil` 时所在的目录。这样同一份用户级配置可以跟随你当前打开的仓库工作。

文件类工具会限制在 workspace root 内，拒绝 `..`、绝对路径和指向 workspace 外的 symlink。`bash` 仍不提供完整进程 sandbox。

## Storage 和 Session 路径

```toml
[storage]
state_root = "auto"
cache_root = "auto"

[session]
# log_dir = "sessions"
```

这些配置分别承担不同路径职责，不是同一个存储位置的多个别名。

| 配置 | 职责 | 默认值 / 解析方式 |
| --- | --- | --- |
| `storage.state_root` | 用户态持久状态根。Sigil 会在 `state_root/workspaces/<workspace-id>` 下为每个 workspace 派生状态目录，并把 input history、artifacts、changesets、terminal task records 等需要审计或恢复的数据放在这里。 | `auto` 使用平台用户状态目录。`SIGIL_STATE_HOME` 会覆盖配置文件值。手动覆盖时建议写绝对路径。 |
| `storage.cache_root` | 用户态可重建缓存根。Sigil 会在 `cache_root/workspaces/<workspace-id>` 下为每个 workspace 派生缓存目录，并用于 `$SIGIL_SCRATCH_DIR` 等临时数据。 | `auto` 使用平台用户缓存目录。`SIGIL_CACHE_HOME` 会覆盖配置文件值。手动覆盖时建议写绝对路径。 |
| `session.log_dir` | 当前 workspace 的 append-only session JSONL 日志目录。它只改变 session logs 写到哪里，不替代 `storage.state_root`。 | 省略时写入 workspace state 目录下的 `sessions` 子目录；相对覆盖值按 workspace state 目录解析。 |

项目内 Sigil 资产固定在 workspace 的 `.sigil` 目录下，不作为用户可编辑 root：

| 路径 | 职责 |
| --- | --- |
| `.sigil/skills` | Sigil-native workspace skills。 |
| `.sigil/agents` | Sigil-native workspace agent profiles。 |
| `.sigil/plugins` | Workspace plugin manifests 和 plugin-owned assets。 |

派生路径（workspace state/cache roots、artifacts、changesets、terminal task records、input history、scratch、`.sigil/*` 项目资产）不会再暴露成独立 root 配置。选择配置时按数据生命周期判断：持久审计/恢复数据走 state，可丢弃 scratch 走 cache，repo-local reusable assets 走固定 `.sigil/*`，`session.log_dir` 只用于调整 session JSONL 位置。

TUI `/config` 的 Storage 页不会编辑这些路径；它只展示已解析路径、artifact retention 和 cleanup action。只有 state/cache roots 可在 `sigil.toml` 或 `SIGIL_STATE_HOME` / `SIGIL_CACHE_HOME` 中配置，项目资产仍固定在 `.sigil/*`。

## Agent

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30
# max_turns = 20
```

- `provider`：当前 runtime 使用的 provider 名称。当前支持 `deepseek`、`openai_compat`、`anthropic` 和 `gemini`。
- `model`：默认模型。
- `tool_timeout_secs`：工具执行超时。
- `max_turns`：可选保险丝。默认不限制；如果显式设置，模型连续达到阈值仍只请求工具而没有最终回答时，本轮会可恢复地停止。

## 执行后端

```toml
[execution]
strategy = "local"
```

`[execution]` 是只通过配置文件编辑的高级配置。默认 `strategy = "local"` 保持普通本地 shell 行为，不声明 OS sandbox 隔离能力。

macOS 上的高级用户可以显式启用第一版 sandbox backend MVP：

```toml
[execution]
strategy = "sandbox"

[execution.sandbox]
backend = "macos_seatbelt"
profile = "workspace_write"
fallback = "deny"
```

`macos_seatbelt` 会通过 `/usr/bin/sandbox-exec` 运行命令；profile 允许读取文件系统、只允许写入命令工作目录，并且不开放网络访问。当前支持的本地路径包括非交互 shell，以及会记录 sandbox coverage receipt 的 PTY、MCP 和 plugin hook handoff surface。它仍然只支持 macOS，不会让远端工具或所有容器/daemon 场景自动获得 sandbox；如果 `sandbox-exec` 不可用会 fail closed。Apple 已将 `sandbox-exec` 标记为 deprecated，因此这个 backend 是 enforcement MVP，不是最终跨平台 sandbox 策略。

合法组合刻意收窄：`strategy = "local"` 不能同时配置 `[execution.sandbox]`；`strategy = "sandbox"` 必须配置 `[execution.sandbox]`；sandbox backend 只支持 `macos_seatbelt`、`linux_bubblewrap` 或 `docker`；Docker 必须配置 `container_image`；非 Docker backend 不能配置 `container_image`。`isolation` 由 strategy 派生，不再是用户配置项。

## 验证

```toml
[verification]

[verification.scope]
profile = "auto"
# extra_excludes = ["tmp/generated/**"]
# generated_roots = ["generated"]

[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test"]
effect = "read_only"
```

`[verification]` 是只通过配置文件编辑的显式用户检查配置。当前 task run 会先把这些条目物化成 verification policy 记录，再用于 completion readiness 判断。Sigil 的 kernel 也支持从 `.sigil/verification.toml`、CI `run:` 步骤、`package.json`、`Cargo.toml` 和 `Makefile` 发现仓库本地候选检查，但“发现”不等于“执行”。仓库本地候选会保持为 suggested checks，直到经过显式审批、满足 policy 的 sandbox decision 或 global policy promotion；仅仅 trust 一个 workspace 不会让所有 CI/Cargo/Makefile 发现项阻塞普通任务。

`[verification.scope]` 是 verification 范围的唯一用户配置入口。`profile` 选择粗粒度 preset，`extra_excludes` 增加项目自己的排除 glob，`generated_roots` 标记不应作为 verification evidence 的生成目录。

首次进入 workspace 时，TUI 会先记录粗粒度 workspace trust decision，然后才进入正式使用。这个 decision 允许加载仓库本地 instructions 和发现 repo-local checks，但不会自动提升发现到的 checks，也不会单独授予 shell、plugin、MCP 或文件写入权限。

每个 `[[verification.checks]]` 条目定义一个来自用户配置的受信任检查：

- `id`：稳定 check id，用于 verification policy 和审计记录。
- `command`：可执行命令名。
- `args`：可选 argv 列表。
- `cwd`：可选 workspace-relative 工作目录。
- `effect`：预期工具副作用。普通 build/test/lint 且不修改验证范围文件时使用 `read_only`。会修改文件的检查只能产生 mutation evidence；要得到 `Passed`，修改后必须再运行一次非写入验证。

用户配置里的项目型命令只会在匹配当前 workspace 时自动应用。例如 `cargo` 检查需要 workspace root 或配置的 `cwd` 向上能找到 `Cargo.toml`，`npm` 等包管理器检查需要 `package.json`，`make` / `just` 检查需要对应项目文件。这样同一份全局 `~/.sigil/sigil.toml` 不会因为某个 scratch 目录缺少对应项目类型，就把无关任务判成 verification failed。

## Appearance

```toml
[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"

[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

`theme` 控制 TUI 配色。内置值包括 `sigil_dark`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`nord` 和 `high_contrast_dark`。`/config` 面板提供 `Appearance` 区块；在 `Theme` 行按 `Enter` 会循环切换内置主题并立即预览草稿 palette，包括 current/draft 对比、syntax、page、shell、composer、tool-card、approval modal、状态、diff 和 markdown 样片。`Ctrl-S` 会把选中主题保存到 `sigil.toml`。

`syntax_theme` 控制 markdown code block、工具 markdown preview 和 approval preview summary 的 syntect/two-face 语法高亮。默认 `auto` 会跟随选中的 TUI theme。显式值包括 `catppuccin_mocha`、`catppuccin_latte`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`gruvbox_light`、`nord`、`one_half_dark`、`one_half_light` 和 `monokai`。

`usage_cost_currency` 控制 TUI usage cost estimate 的显示币种。默认 `auto` 会优先跟随 provider balance currency，拿不到时显示 USD。也可以显式设置为 `usd` 或 `cny`。该配置只影响展示；provider pricing 和 session usage accounting 仍以 USD-based estimate 记录。

`[appearance.colors]` 可以用 `#RRGGBB` 覆盖稳定语义 color token。未知 token 或非十六进制值会由 appearance diagnostics 报告，不会变成 provider 可见状态。覆盖只影响 TUI 渲染，不写入 session history、approval record、tool payload 或 provider 可见上下文。

在 `/config` 的 Appearance 中，`Syntax theme` 会在 `auto` 和显式代码高亮主题之间循环。`Color group` 会把颜色编辑器限制在一个 token 分组内，`Color token` 选择该分组里的语义 token，`Override` 编辑当前 token 的覆盖值。在 `Color group` 或 `Color token` 行按 `Enter` 可循环选项；在 `Override` 行输入或粘贴 `#RRGGBB` 可设置覆盖；在 token 或 override 行按 `Backspace` 或 `Delete` 会清除当前 token；在 group 行按 `Backspace` 或 `Delete` 会清除该分组；`Ctrl-R` 会清空草稿里的全部颜色覆盖。

`sigil doctor`、TUI `/doctor` 和 `/config` Appearance 实时 diagnostics 会在 config load 或草稿编辑后校验 appearance 覆盖。文字/背景低对比、语义颜色过近和结构提示过弱会作为 warning 展示并附带修复建议；非法覆盖值会显示在 `appearance:colors`。

支持的 color token 是稳定语义名。优先只覆盖表达目标变化的最小 token 组；例如想改变信息强调色时，先改 `accent_info`，不要把每个状态或工具卡颜色都单独覆盖一遍。

| 分组 | Token | 使用位置 | 建议约束 |
| --- | --- | --- | --- |
| Surface | `surface_base`, `surface_rail`, `surface_panel`, `surface_panel_alt`, `surface_input`, `surface_agent_panel`, `surface_overlay`, `surface_overlay_shadow`, `surface_badge`, `surface_selection`, `surface_user_message`, `surface_code` | Shell 背景、info rail、composer、agent panel、overlay、badge、选中行、用户气泡、代码块 | 保持 `text_primary` 在 `surface_base`、`surface_panel`、`surface_input`、`surface_user_message` 上可读。 |
| Border | `border_subtle`, `border_strong`, `border_focus`, `border_danger` | 面板分隔线、焦点边框、危险边框 | subtle border 要可见，但不要抢过 focus/danger border。 |
| Text | `text_primary`, `text_secondary`, `text_muted`, `text_inverse`, `text_disabled` | 正文、次级详情、提示、选中按钮文字、禁用文字 | `text_primary` 需要高对比；`text_muted` 只用于非关键标签。 |
| Accent | `accent_primary`, `accent_secondary`, `accent_info`, `accent_success`, `accent_warning`, `accent_danger`, `accent_streaming`, `accent_idle` | Composer 状态、section label、信息/成功/警告/危险语义、streaming/idle 状态 | success、warning、danger、info 需要能一眼区分。 |
| Selection / Button | `selection_fg`, `selection_bg`, `button_selected_fg`, `button_selected_bg`, `button_inactive_fg` | 活跃行、选中的 footer/config action、按钮式 chip | 选中态前景色要在 `selection_bg` 和按钮背景上都可读。 |
| Status | `status_idle`, `status_thinking`, `status_tool`, `status_streaming`, `status_success`, `status_warning`, `status_error`, `status_pending` | live status、doctor 结果、task/agent indicator、info rail marker | success、warning、error、pending 需要能快速区分。 |
| Diff | `diff_header_fg`, `diff_hunk_fg`, `diff_added_fg`, `diff_added_bg`, `diff_removed_fg`, `diff_removed_bg`, `diff_context_fg`, `diff_gutter_fg`, `diff_current_hunk_bg` | 工具预览和 approval diff 面板 | added/removed 颜色及背景要彼此可区分。 |
| Approval / Risk | `approval_bg`, `approval_backdrop_bg`, `approval_border`, `approval_shadow`, `risk_low`, `risk_medium`, `risk_high`, `approval_allow_bg`, `approval_deny_bg`, `approval_selected_bg` | 工具审批 modal、risk badge、allow/deny action | allow 和 deny 背景要明显不同；`risk_high` 要比 `risk_low` 更醒目。 |
| Markdown | `markdown_heading`, `markdown_quote_bar`, `markdown_quote_text`, `markdown_rule`, `markdown_code_fg`, `markdown_code_bg`, `markdown_link` | Timeline markdown、tool-card markdown preview、approval summary markdown | inline code 要在 `markdown_code_bg` 上可读；link 要和 heading 可区分。 |
| Modal / Overlay | `modal_bg`, `modal_border`, `modal_shadow`, `modal_command_bg`, `modal_selected_bg`, `overlay_bg`, `overlay_shadow` | 弹窗和 slash command overlay | command chip 要在 `modal_command_bg` 上可读；选中行要明显。 |
| Config / Setup | `config_bg`, `config_border`, `config_primary`, `config_detail`, `config_warning`, `config_danger`, `config_tab_bg`, `config_section_bg`, `config_selected_bg`, `setup_bg` | `/config`、setup flow、config preview、config footer/action | `config_selected_bg` 要和 `config_bg` 区分；warning/danger 要分开。 |

推荐约束：

- 只使用 `#RRGGBB` 值。命名颜色和 alpha 值会被拒绝。
- 把 token 当作语义角色，而不是组件私有 CSS 变量；同一个 token 可能影响多个 TUI 表面。
- 修改 override 后运行 `sigil doctor`；warning 表示配置被接受，但可能难读。
- 先从内置主题出发，只覆盖少量 token。完全自定义 palette 可以做到，但更难保持可读性。

## Task Planning

```toml
[task]
enabled = true
default_mode = "chat"
max_plan_steps = 12
max_replans = 2
max_subagents = 8
multi_agent_mode = "explicit_request_only"
allow_write_subagents = true

[task.planner]
# provider = "deepseek"
# model = "deepseek-v4-flash"
# reasoning_effort = "high"

[task.executor]
# model = "deepseek-v4-pro"

[task.subagent_read]
# 默认只读。

[task.subagent_write]
# 只有 allow_write_subagents = true 时才使用完整工具面。
```

计划任务通过 TUI 里的 `/task <任务>` 发起。`/plan` 仍是只读 planning prompt，只有用户显式接受 plan-ready handoff 后才会创建并运行 durable task state。`default_mode = "chat"` 会让普通 composer 提交始终保持 chat-first，即使当前 session 里还有未完成 task；需要继续任务时使用 `/task continue` 或 task UI action。只有明确想把计划任务作为默认流程时才改成 `plan`。

各 role 的 provider/model 未配置时继承 `[agent]`。Planner 和 subagent-read 默认只看到只读文件/搜索/code-intelligence 工具。Executor 可以看到完整 runtime registry。Subagent-write 只有在 `allow_write_subagents = true` 时才能看到完整 registry；否则回退到只读工具面。写工具仍然按正常审批策略执行。

Agent 并发由 `[task].max_subagents` 控制：默认最多允许 8 个活跃子 agent，覆盖 foreground、background、只读和可写角色。Token 用量会记录到 agent result 里用于报告，但不作为拒绝 spawn 的硬预算。

`multi_agent_mode` 控制模型可见 agent 工具的使用时机。默认 `explicit_request_only` 会保留 `spawn_agent`，但要求模型只有在用户或当前 repo/skill 指令明确要求 delegation、parallel agent work 或 subagent 时才使用子 agent。`none` 关闭普通模型委派指令，`proactive` 允许模型在并行、非重叠工作明显提升速度或质量时主动 spawn。可写 `worker` 仍受 runtime 策略约束：foreground 和 join-before-final 只能走 changeset-only merge review；后台 worker 写入会被拒绝，直到隔离能力可用。

每个 role 都可以覆盖可见工具：

```toml
[task.planner.tools]
names = ["read_file", "ls", "glob", "grep", "code_symbols"]
prefixes = []
allow_all = false
```

使用 name 和 prefix 时要保持克制。Scoped role registry 会同时限制 tool specs、preview、execute、permission hooks 和 egress hooks，因此隐藏工具不是只从 prompt 里省略，而是真的不能执行。

## Providers

`[agent].provider` 选择 runtime provider，`[agent].model` 选择聊天模型。对应的 `[providers.*]` 区块控制 endpoint、认证和 provider 专项选项。
只支持下表中的 provider 值；其他值会在配置校验时报错。

| Provider value | Config block | 主要 API key 环境变量 | 指南 |
| --- | --- | --- | --- |
| `deepseek` | `[providers.deepseek]` | `SIGIL_API_KEY` | [DeepSeek provider](provider-deepseek.md) |
| `openai_compat` | `[providers.openai_compat]` | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | [OpenAI-compatible provider](provider-openai-compatible.md) |
| `anthropic` | `[providers.anthropic]` | `SIGIL_ANTHROPIC_API_KEY` | [Anthropic provider](provider-anthropic.md) |
| `gemini` | `[providers.gemini]` | `SIGIL_GEMINI_API_KEY` | [Gemini provider](provider-gemini.md) |

Provider 专项行为保留在 provider 配置和 provider crate 内。共享的 `sigil-kernel` 契约保持 provider-neutral：messages、tools、usage、approvals 和 session state 不应包含 provider-only 术语。

## Permission

默认配置：

```toml
[permission]
mode = "manual"

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

模式：

| Mode | 用户理解 | 语义 |
| --- | --- | --- |
| `read-only` | 只看不改 | 读取默认允许；写入、执行、网络工具会被拒绝，即使低层覆盖尝试放行也不能放宽。 |
| `manual` | 手动确认 | 读取默认允许；写入、执行、网络工具默认询问，除非命中特定 tool/rule/external-directory 策略。 |
| `auto-edit` | 自动改文件 | Workspace 内文件编辑默认允许；shell 和网络工具默认仍询问。 |
| `danger-full-access` | 高风险全放开 | 默认允许所有工具访问。名称中显式带 `danger`，避免误用。 |

含义：

- `mode = "manual"` 是默认交互安全姿态。
- `tools`、`rules` 和 `external_directory` 是高级配置文件覆盖项，只用于特定工具、subject 或外部路径，不再承担第二套默认权限 baseline。
- workspace 外路径默认不可执行；开启 external directory 后仍会先经过 external-directory gate。
- 临时 shell scratch 文件应使用 `bash` 或 `terminal_start` 提供的 `$SIGIL_SCRATCH_DIR`。它由 Sigil 用户态 cache root 承载，对模型显示为 `cache/tmp`；系统 temp 目录（如 `/tmp`、macOS `/private/tmp`、Windows `%TEMP%`）仍属于 workspace 外路径，默认不会放行。
- headless `run` 遇到最终 `ask` 不会静默自动执行，而是向模型回灌结构化 `approval_required` 工具错误。

优先级：

| 顺序 | 来源 | 职责 |
| --- | --- | --- |
| 1 | `mode` baseline | 用户可理解的顶层模式设置默认姿态；`read-only` 是非读硬上限，`danger-full-access` 是显式全放开。 |
| 2 | 工具自身 default | runtime/tool 提供的默认值，例如可信只读命令降级。 |
| 3 | `tools.<tool_name>` | 工具名覆盖。 |
| 4 | `rules[]` | 命中的 tool/subject 规则；多条命中按最严格模式合并：`deny > ask > allow`。 |
| 5 | `external_directory` | workspace 外 subject 的额外 gate：未启用即 deny；启用后用命中的 external rules，否则用 `external_directory.default_mode`。 |
| 6 | Effective policy cap | runtime cap 继续按同一个最严格模式合并。 |

## Memory

```toml
[memory]
enabled = true
```

启用后，Sigil 启动时会稳定装载工作区根 memory 文档，例如 `SIGIL.md`、`AGENTS.md`、`CLAUDE.md`、`SIGIL.local.md`，并支持单独一行 `@path` 导入。

## Skills 和 Agents

```toml
[skills]
enabled = true
user_skills = true
user_agents = true
compatibility_sources = []
```

Skill 和 agent discovery 分成三类 source：

| 配置 | 职责 |
| --- | --- |
| `.sigil/skills` | 当前 workspace 固定的 Sigil-native reusable skills。 |
| `.sigil/agents` | 当前 workspace 固定的 Sigil-native agent profiles。Agents 会作为 child session 运行，而不是 inline skill context。 |
| `user_skills` / `user_agents` | 是否加载用户配置目录里的 per-user skills 和 agents。它们不会改变 workspace discovery roots。 |
| `compatibility_sources` | 显式导入外部生态目录。当前支持 `claude` 和 `reasonix`；默认值为空，因此普通 workspace source 只来自 Sigil-native `.sigil/*`。 |

Compatibility source 会在 Agents / Skills 浏览器里通过 source/trust 标出来，并且仍需经过同一套 trust lifecycle 才能被模型或用户调用。TUI `/config` 的 Agents 和 Skills 区块用于浏览已发现条目、展示 source/trust/hash/run mode，并提供 trust/use action。Workspace discovery roots 固定在 `.sigil/*`。

## Compaction

```toml
[compaction]
enabled = true
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
# fallback_context_window_tokens = 128000
tail_messages = 6
```

如果当前 provider/model 能解析 context window，Sigil 会优先使用模型窗口。只有无法解析时，才回退到 `fallback_context_window_tokens`。

## Code Intelligence

```toml
[code_intelligence]
enabled = false
server_startup = "lazy"
default_timeout_ms = 5000
max_results = 100
max_payload_bytes = 65536
auto_discover = true
report_missing = true
```

开启后，runtime 会注册只读 code intelligence 工具，以及用于 code action 和 symbol rename 的 LSP edit 工具。edit 工具属于 `Write` 工具，必须先展示 diff 审批，获批后才会改文件。TUI 可以用 `Alt-D` 对 git changed source files 触发 diagnostics 检查。

`auto_discover = true` 时，Sigil 会按 workspace 自动发现常见语言和 PATH 上可用的安全 LSP server。手写 `code_intelligence.servers` 只作为高级覆盖或补充。

TUI `/config` 面板里有 `Code Intel` 区块，可以调整 `enabled`、`server_startup` 和 `auto_discover`，并查看只读 trust 边界与 readiness 检查。readiness 行复用同一份本地 doctor 事实，所以缺 LSP command 时会在启动 language server 前先给出修复建议。

语言服务器示例：

```toml
[[code_intelligence.servers]]
name = "rust-analyzer"
languages = ["rust"]
command = "rust-analyzer"
root_markers = ["Cargo.toml"]
file_extensions = ["rs"]
startup_timeout_ms = 5000
trust_required = true
```

## Terminal

```toml
[terminal]
keyboard_enhancement = "auto"
mouse_capture = false
osc52_clipboard = true
scroll_sensitivity = 3
```

`keyboard_enhancement` 控制 crossterm 键盘增强协议。默认 `auto` 会在 TUI 启动时探测当前终端，只在支持时请求 enhanced key reporting。需要强制请求时设为 `on`；如果终端、multiplexer、SSH 层或嵌入式 PTY 不能稳定处理增强协议，设为 `off`。

`mouse_capture` 控制 TUI 是否向终端请求鼠标事件，用于点击、滚动、审批控件、setup/config/session 选择和 transcript 拖选。它默认关闭，优先保证不同 multiplexer 和嵌入式 PTY 下的键盘输入可靠；只有在你需要鼠标能力且终端能稳定处理 mouse mode 时再开启。键盘操作始终可用。

`osc52_clipboard` 控制 `Ctrl-C` 是否通过 OSC52 序列复制选中的 transcript 文本。如果终端禁用了 OSC52，或者会把控制序列显示成可见文本，可以关闭。关闭后 Sigil 会显示 `clipboard unavailable`，不会再向终端写剪贴板序列。

`scroll_sensitivity` 控制鼠标滚轮每 tick 在 transcript 和 approval diff 中移动的行数。默认值是 `3`；高分辨率滚轮可以调小，终端滚动事件偏慢时可以调大。

TUI `/config` 面板有只读 `Terminal` 区块用于查看这些控制项。兼容性覆盖请直接编辑 `sigil.toml`。`keyboard_enhancement` 在下一次启动时解析；`mouse_capture` 下一次启动生效；`osc52_clipboard` 每次复制时都会读取当前配置；`scroll_sensitivity` 在配置保存并重新加载后应用到运行配置。

`doctor` 会报告配置开关、`TERM`、常见终端 profile 变量、tmux/screen、SSH、WSL 和剪贴板桥接风险。跨 iTerm2、Terminal.app、WezTerm、kitty、tmux 和 SSH 的可重复人工 checklist 见 [terminal-compatibility.md](terminal-compatibility.md)。

## Provider 环境变量 Override

当前支持：

Model request：

- `SIGIL_MODEL_REQUEST_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS`

这些变量覆盖所有 provider 共用的 `[model_request]`。当某个 shell 或 CI job
需要不同的模型请求传输超时时，可以用环境变量覆盖，而不必修改 `sigil.toml`。

DeepSeek：

- `SIGIL_API_KEY`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_STRICT_TOOLS_MODE`

`SIGIL_API_KEY` 优先级最高。如果只配置了 `[providers.deepseek].api_key`，Sigil 会把它视为明文配置认证，`doctor` 会输出 warning 和修复建议。

OpenAI-compatible：

- `SIGIL_OPENAI_COMPATIBLE_API_KEY`
- `SIGIL_OPENAI_COMPATIBLE_BASE_URL`

OpenAI-compatible provider 认证使用 `SIGIL_OPENAI_COMPATIBLE_API_KEY`。通用 OpenAI 环境变量会被忽略，避免 Sigil 凭据和其他工具共享状态。

Anthropic：

- `SIGIL_ANTHROPIC_API_KEY`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_ANTHROPIC_VERSION`
- `SIGIL_ANTHROPIC_MAX_TOKENS`

Anthropic 认证使用 `SIGIL_ANTHROPIC_API_KEY`。通用 Anthropic 环境变量会被忽略。

Gemini：

- `SIGIL_GEMINI_API_KEY`
- `SIGIL_GEMINI_BASE_URL`

Gemini 认证使用 `SIGIL_GEMINI_API_KEY`。通用 Google/Gemini 环境变量会被忽略。

## Plugins

Workspace plugin manifest 从 `.sigil/plugins/<id>/plugin.toml` 发现。它们通过 TUI review，不在 `sigil.toml` 里直接编辑。

打开 `/config`，进入 `Plugins`，用 `PgUp/PgDn` 选择已发现 manifest。detail view 会展示 trust 状态、相对 manifest 路径、完整 manifest hash、skill 路径、带 args 和 approval mode 的 hook command，以及带 args、startup 和 required 状态的 MCP server command。footer 的 `approve` 只信任当前展示的 manifest hash；`deny` 会禁用这个 hash。记录决策前 Sigil 会重新加载 manifest，所以 hash 改变后必须重新 review。

## MCP

MCP server 使用 `[[mcp_servers]]` 配置，详见 [mcp.md](mcp.md)。
