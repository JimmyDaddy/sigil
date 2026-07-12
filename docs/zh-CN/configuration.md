# Sigil 配置指南

[文档首页](README.md) · [快速上手](quickstart.md) · [Provider 指南](providers.md) · [排障](troubleshooting.md) · [参考](reference.md) · [English](../en/configuration.md)

本文负责 Sigil 的共享配置：配置解析、workspace、权限、任务、工具、外观、终端行为、插件和 MCP。Provider 选择、provider 配置区块和认证环境变量统一由 [Provider 指南](providers.md)及其链接的 provider 专页维护。

## 常见用户路径

| 目标 | 推荐路径 |
| --- | --- |
| 第一次本地 setup | 运行 `sigil` 并完成 Quick Setup |
| 临时本地认证 | 先选 provider，再使用对应的[环境变量](providers.md#认证优先级) |
| CI 或脚本认证 | 进入所选的 [provider 页面](providers.md)，使用其中的环境变量 |
| 从 TUI 切换 model/provider | 先读 [provider 选择方式](providers.md#provider-选择方式)，再使用 `/config` |
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

临时使用或 CI 场景，先选择 provider，再按 [provider 认证映射](providers.md#认证优先级)设置对应环境变量，然后启动 Sigil。各 provider 专页提供可复制的 shell 命令；不存在对所有 provider 通用的 API key 环境变量。

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

## 共享配置片段

如果需要手写配置，可以从这段 provider-neutral 配置开始：

```toml
[workspace]
root = "."

[agent]
tool_timeout_secs = 30

[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3

[appearance]
theme = "sigil_dark"
syntax_theme = "auto"
usage_cost_currency = "auto"
```

请把这些共享设置与 [Provider 指南](providers.md)中的 provider 选择和认证设置组合使用。各 provider 专页提供可复制的 provider block 和环境变量命令。

可复制模板位于 [docs/examples/config](../examples/config)：

- [mcp-safe-defaults.toml](../examples/config/mcp-safe-defaults.toml)
- [code-intelligence-rust.toml](../examples/config/code-intelligence-rust.toml)

Provider 专项模板统一从 [Provider 指南](providers.md)进入，和对应的选择、认证规则放在一起维护。

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
| `.sigil/commands` | Sigil-native Markdown slash commands。每个 `*.md` 文件会作为 user-invocable inline command 发现。 |
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

`macos_seatbelt` 会通过 `/usr/bin/sandbox-exec` 运行命令；profile 允许读取文件系统、只允许写入命令工作目录，但它不能证明网络隔离。用户 shell 路径会如实记录这个限制；MCP/plugin hook 等 extension process 在所选 profile 禁止网络时会在 spawn 前失败，只有 network-allowed profile 才能使用该 backend。当前支持的本地路径包括非交互 shell，以及会记录 sandbox coverage receipt 的 PTY、MCP 和 plugin hook handoff surface。它仍然只支持 macOS，不会让远端工具或所有容器/daemon 场景自动获得 sandbox；如果 `sandbox-exec` 不可用会 fail closed。Apple 已将 `sandbox-exec` 标记为 deprecated，因此这个 backend 是 enforcement MVP，不是最终跨平台 sandbox 策略。

合法组合刻意收窄：`strategy = "local"` 不能同时配置 `[execution.sandbox]`；`strategy = "sandbox"` 必须配置 `[execution.sandbox]`；sandbox backend 只支持 `macos_seatbelt`、`linux_bubblewrap` 或 `docker`；Docker 必须配置 `container_image`；非 Docker backend 不能配置 `container_image`。`isolation` 由 strategy 派生，不再是用户配置项。

Sandbox capability 与 network receipt 假设本机安装的 enforcement executable 或 daemon 可信。Sigil 当前通过启动时的 `PATH` 发现 `bwrap` 和 `docker`，并检查 availability/conformance，但不会 attest binary supply chain、owner 或 mode。请使用管理员控制的安装与可信启动 `PATH`；恶意 wrapper 会破坏 receipt 模型。

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

请通过 [Provider 指南](providers.md)选择 provider，设置 `[agent].provider` 与 `[agent].model`，配置对应的 `[providers.*]` block，并选择正确的认证环境变量。本页只说明所有 provider 共享的配置。

## Permission

默认配置：

```toml
[permission]
mode = "manual"

[permission.commands]
allow = []
ask = []
deny = []

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

模式：

| Mode | 用户理解 | 语义 |
| --- | --- | --- |
| `read-only` | 只看不改 | 本地读取默认允许；本地写入和进程执行会被拒绝，即使低层本地覆盖尝试放行也不能放宽。网络 effect 单独求值。 |
| `manual` | 手动确认 | 本地读取默认允许；本地写入和执行默认询问，除非命中特定 tool/rule/external-directory 策略。 |
| `auto-edit` | 自动改文件 | Workspace 内文件编辑默认允许；本地进程执行默认仍询问。 |
| `danger-full-access` | 高风险本地全放开 | 默认允许本地访问。名称中显式带 `danger`，但它不能覆盖独立的网络 ask 或 deny。 |

含义：

- `mode = "manual"` 是默认交互安全姿态。
- `commands`、`tools`、`rules` 和 `external_directory` 是高级配置文件覆盖项，只用于特定命令、工具、subject 或外部路径，不再承担第二套默认权限 baseline。
- `permission.commands` 是推荐的高级 shell command 覆盖方式。Pattern 匹配归一化后的命令文本，并且只把 `*` 和 `?` 当作通配符。完全相同的 pattern 不能同时出现在 `allow`、`ask`、`deny` 多个分组中。
- 命中 `permission.commands` 时，审批卡片和 session audit 会记录 `permission.commands.<allow|ask|deny>`、pattern 与命令文本，方便解释为什么被放行、询问或拒绝。
- workspace 外路径默认不可执行；开启 external directory 后仍会先经过 external-directory gate。
- 临时 shell scratch 文件应使用 `bash` 或 `terminal_start` 提供的 `$SIGIL_SCRATCH_DIR`。它由 Sigil 用户态 cache root 承载，对模型显示为 `cache/tmp`；系统 temp 目录（如 `/tmp`、macOS `/private/tmp`、Windows `%TEMP%`）仍属于 workspace 外路径，默认不会放行。
- headless `run` 遇到最终 `ask` 不会静默自动执行，而是向模型回灌结构化 `approval_required` 工具错误。
- 本地 access 与网络 effect 是正交的两条轴。工具分别声明本地 `Read` / `Write` / `Execute`，以及可选的网络 `Read` / `Mutate` / `Unknown` effect。`NetworkEndpoint` 不属于 external-directory path；`read-only` 只有在有效 `NetworkPolicy` 允许时才允许网络读取，网络修改或未知网络 effect 在 `read-only` 下仍会拒绝。`danger-full-access` 不能覆盖网络 `Ask` 或 `Deny`。
- `[web]` 是 stable web search 与用户根 Streamable HTTP MCP 共用的网络策略。通用本地 stdio MCP 调用仍保守分类为本地 `Read` 加 `NetworkEffect::Unknown`；source/tool 审批继续参与最终的 `Deny > Ask > Allow` 求交。

优先级：

| 顺序 | 来源 | 职责 |
| --- | --- | --- |
| 1 | 本地 `mode` baseline | 用户可理解的顶层模式设置本地 Read/Write/Execute 姿态；`read-only` 是本地写入/执行硬上限。 |
| 2 | 独立 network policy | runtime 对声明或动态 network effect 单独求值；`Deny` 永不可放宽。交互式只读 `NetworkRequest` 的 `Ask` 可以由用户显式选择 `Allow session`，grant 只覆盖同一 tool 与 session，并继续经过 destination guard、disclosure 和 audit。 |
| 3 | 工具/source default | runtime/tool 提供的 source policy，例如 MCP trust 审批或可信只读命令降级。 |
| 4 | `tools.<tool_name>` | 工具名覆盖。 |
| 5 | `rules[]` | 命中的 tool/subject 规则；最后一条匹配规则生效，用文件顺序表达更具体的覆盖。 |
| 6 | `commands.allow/ask/deny` | shell command 的匹配 pattern。command 分组内部按 `deny > ask > allow` 合并；command `allow` 可以放宽 `manual` 默认 shell ask，但不能覆盖显式 tool/rule ask 或 deny。 |
| 7 | `external_directory` | workspace 外 `Path` subject 的额外 gate：未启用即 deny；启用后用命中的 external rules，否则用 `external_directory.default_mode`。网络 endpoint 不进入该 gate。 |
| 8 | Effective policy cap 和风险覆盖 | runtime cap、本地 `read-only`、protected path、destructive operation 和 external-directory deny 仍是硬安全边界。最终结果取 local、network 和 source decision 中最严格的一项。 |

## Web 搜索与网络

alpha 默认启用 stable web search，并允许网络访问。`auto` 优先使用当前精确模型支持的 provider-hosted search；否则用户显式指定的 MCP binding 具有权威性，只有 binding 不存在时才允许使用内置匿名 Exa MCP profile。

```toml
[web]
enabled = true
network_mode = "allow" # allow | ask | deny
search_route = "auto"  # auto | provider_hosted | mcp | bundled | disabled
max_results = 8
max_query_chars = 512
max_query_bytes = 2048

[web.bundled_search]
enabled = true
```

使用自有兼容 MCP tool 替换 bundled search：

```toml
[web.search_mcp]
server = "my-search"
tool = "search"
```

该 binding fail-closed：连接、identity、schema、permission 或 tool 失败时都不会回退到 bundled Exa。bundled route 会把规范化后的完整 query 发送到 `https://mcp.exa.ai/mcp`；Exa 与网络路径可观察 query 以及源 IP/代理出口 IP。Sigil 不为此 route 提供 API key，也不承诺 quota 或 SLA；query 出站前会阻止已识别 secret 与高置信个人数据。可通过 `enabled = false`、`search_route = "disabled"` 或 `network_mode = "deny"` 关闭。

provider-hosted search 会对每个 provider request 独立授权和披露。因为它没有普通 client tool 的审批回合，`network_mode = "ask"` 对 hosted search 保持 fail-closed；configured/bundled client search 仍走常规 tool approval。

`network_mode = "allow"` 时，只读 client `websearch` / `webfetch` 不逐次询问，但每次出站仍执行 disclosure、durable audit、SSRF/DNS 与 budget 检查。`network_mode = "ask"` 时，审批面提供 `Allow once`、`Allow session` 和 `Deny`；session grant 只放宽当前 tool 的只读网络 facet，不覆盖 source trust、网络写入/Unknown、不同 tool 或任何 `Deny`。

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
| `.sigil/commands` | 当前 workspace 固定的 Sigil-native Markdown slash commands。每个 `*.md` 文件会通过 `/command-id` 作为 inline skill context 运行。 |
| `.sigil/agents` | 当前 workspace 固定的 Sigil-native agent profiles。Agents 会作为 child session 运行，而不是 inline skill context。 |
| `user_skills` / `user_agents` | 是否加载用户配置目录里的 per-user skills 和 agents。它们不会改变 workspace discovery roots。 |
| `compatibility_sources` | 显式导入外部生态目录。当前支持 `claude` 和 `reasonix`；默认值为空，因此普通 workspace source 只来自 Sigil-native `.sigil/*`。 |

Compatibility source 会在 Agents / Skills 浏览器里通过 source/trust 标出来，并且仍需经过同一套 trust lifecycle 才能被模型或用户调用。TUI `/config` 的 Agents 和 Skills 区块用于浏览已发现条目、展示 source/trust/hash/run mode，并提供 trust/use action。Workspace discovery roots 固定在 `.sigil/*`。

Workspace agent profile 可以在 `.sigil/agents/<id>/agent.toml` 或 `.sigil/agents/<id>/AGENT.md` 里声明 OpenCode 风格的权限。`permission` 表达 agent 可以做什么；`tool_scope` / `allowed_tools` 只用于收窄这个 profile 能看到哪些工具，不授予权限：

```toml
description = "Focused implementation worker"
trust = "trusted"
invocation_policy = "model_allowed"
result_policy = "foreground_merge_required"

[permission]
read = "allow"
glob = "allow"
grep = "allow"
edit = "ask"

[permission.commands]
allow = ["cargo test *", "git status*", "git diff*"]
ask = ["cargo clippy *"]
deny = ["git push*", "rm *"]
```

Agent permission 会在全局 `[permission]` 之后合并。Agent command 分组使用和 root config 相同的 `allow` / `ask` / `deny` 语义。全局 `read-only` mode 仍是硬上限；protected path、destructive operation、external-directory gate 和写型 subagent 隔离仍然 fail-closed。可写 subagent 在更强隔离模式可用前，仍只能走 foreground changeset-only merge review。

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

开启后，runtime 会注册代码查询工具，以及用于 code action 和 symbol rename 的 LSP edit 工具。edit 工具属于 `Write` 工具，必须先展示 diff 审批，获批后才会改文件。Workspace trust 只控制配置的 LSP 进程能否启动，不会绕过工具权限或 diff 审批。TUI 可以用 `Alt-D` 对 git changed source files 触发 diagnostics 检查。

`auto_discover = true` 时，Sigil 会按 workspace 自动发现常见语言和 PATH 上可用的安全 LSP server。手写 `code_intelligence.servers` 只作为高级覆盖或补充。

TUI `/config` 面板里有 `Code Intel` 区块，可以调整 `enabled`、`server_startup` 和 `auto_discover`，并查看 LSP 进程 trust 边界、写操作审批要求与 readiness 检查。readiness 行复用同一份本地 doctor 事实，所以缺 LSP command 时会在启动 language server 前先给出修复建议。

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

`trust_required` 默认是 `true`。这类 server 只有在当前 session 内存在与当前 workspace 精确匹配的 durable `Trusted` decision 时才会启动；`Unknown`、`Restricted` 和 `Denied` 都会在解析 command 或 spawn 进程前 fail-closed。因此，全新的 `sigil run` session 不能启动要求 trust 的 LSP；只有明确接受该进程启动风险时，才应设置 `trust_required = false` 关闭这道 gate。没有 LSP 进程时仍可使用 Rust Tree-sitter fallback；无论采用哪种设置，LSP 写工具都继续要求正常的 diff 审批。

## Terminal

```toml
[terminal]
keyboard_enhancement = "auto"
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3
```

`keyboard_enhancement` 控制 crossterm 键盘增强协议。默认 `auto` 会在 TUI 启动时探测当前终端，只在支持时请求 enhanced key reporting。需要强制请求时设为 `on`；如果终端、multiplexer、SSH 层或嵌入式 PTY 不能稳定处理增强协议，设为 `off`。

`mouse_capture` 控制 TUI 是否向终端请求鼠标事件，用于点击、滚动、审批控件、setup/config/session 选择和 transcript 拖选。普通交互式 TUI 默认开启；如果终端、multiplexer、SSH 层或嵌入式 PTY 不能稳定处理 mouse mode，可以显式设为 `false`。键盘操作始终可用。

`osc52_clipboard` 控制 `Ctrl-C` 是否通过 OSC52 序列复制选中的 transcript 文本。如果终端禁用了 OSC52，或者会把控制序列显示成可见文本，可以关闭。关闭后 Sigil 会显示 `clipboard unavailable`，不会再向终端写剪贴板序列。

`scroll_sensitivity` 控制鼠标滚轮每 tick 在 transcript 和 approval diff 中移动的行数。默认值是 `3`；高分辨率滚轮可以调小，终端滚动事件偏慢时可以调大。

TUI `/config` 面板有只读 `Terminal` 区块用于查看这些控制项。兼容性覆盖请直接编辑 `sigil.toml`。`keyboard_enhancement` 在下一次启动时解析；`mouse_capture` 下一次启动生效；`osc52_clipboard` 每次复制时都会读取当前配置；`scroll_sensitivity` 在配置保存并重新加载后应用到运行配置。

`doctor` 会报告配置开关、`TERM`、常见终端 profile 变量、tmux/screen、SSH、WSL 和剪贴板桥接风险。跨 iTerm2、Terminal.app、WezTerm、kitty、tmux 和 SSH 的可重复人工 checklist 见 [Terminal 兼容性检查清单](terminal-compatibility.md)。

## 模型请求环境变量覆盖

- `SIGIL_MODEL_REQUEST_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS`

这些变量覆盖所有 provider 共用的 `[model_request]`。当某个 shell 或 CI job 需要不同的模型请求传输超时时，可以用环境变量覆盖，而不必修改 `sigil.toml`。Provider 专项 endpoint 和认证环境变量只在 [Provider 指南](providers.md)及其链接的 provider 专页中维护。

## Plugins

Workspace plugin manifest 从 `.sigil/plugins/<id>/plugin.toml` 发现。它们通过 TUI review，不在 `sigil.toml` 里直接编辑。

打开 `/config`，进入 `Plugins`，用 `PgUp/PgDn` 选择已发现 manifest。detail view 会展示 trust 状态、相对 manifest 路径、完整 manifest hash、skill 路径、带 args 和 approval mode 的 hook command，以及带 args、startup 和 required 状态的 MCP server command。footer 的 `approve` 只信任当前展示的 manifest hash；`deny` 会禁用这个 hash。记录决策前 Sigil 会重新加载 manifest，所以 hash 改变后必须重新 review。Plugin MCP entry 不能声明 `inherit_env`；需要凭证的 stdio server 应放在用户根配置。

## MCP

MCP server 使用 `[[mcp_servers]]` 配置。本地 stdio server 默认清空环境；需要显式凭证 grant 时，使用仅允许出现在用户根配置的 additive `inherit_env = ["ENV_NAME"]` 字段。`/doctor` 与 `/config` 只展示 grant name、missing 状态和 live-fingerprint readiness，不展示变量值。详见 [Sigil MCP 接入指南](mcp.md)。
