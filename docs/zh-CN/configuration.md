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
2. 当前工作目录下的 `./sigil.toml`
3. 标准用户配置目录里的 `sigil.toml`

标准用户配置路径：

- macOS：`~/Library/Application Support/sigil/sigil.toml`
- Linux：`$XDG_CONFIG_HOME/sigil/sigil.toml` 或 `~/.config/sigil/sigil.toml`
- Windows：`%APPDATA%\sigil\sigil.toml`

仓库根目录的 `sigil.toml` 默认不应提交，因为它可能包含真实密钥。

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

[session]
log_dir = ".sigil/sessions"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[terminal]
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3

[appearance]
theme = "sigil_dark"
syntax_theme = "auto"

[providers.deepseek]
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# 推荐优先使用 SIGIL_API_KEY；如果写在这里，会以 plaintext 保存。
# api_key = "sk-..."
```

`SIGIL_API_KEY` 优先级高于配置文件里的 `api_key`。旧环境变量 `DEEPSEEK_API_KEY` 仍作为 DeepSeek provider 的备用来源读取。`doctor` 会对仅来自明文配置的认证给 warning，但不会阻止运行。

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

## Appearance

```toml
[appearance]
theme = "sigil_dark"
syntax_theme = "auto"

[appearance.colors]
surface_base = "#07080A"
accent_primary = "#91B6AA"
markdown_code_bg = "#1C2129"
```

`theme` 控制 TUI 配色。内置值包括 `sigil_dark`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`nord` 和 `high_contrast_dark`。`/config` 面板提供 `Appearance` 区块；在 `Theme` 行按 `Enter` 会循环切换内置主题并立即预览草稿 palette，包括 current/draft 对比、syntax、page、shell、composer、tool-card、approval modal、状态、diff 和 markdown 样片。`Ctrl-S` 会把选中主题保存到 `sigil.toml`。

`syntax_theme` 控制 markdown code block、工具 markdown preview 和 approval preview summary 的 syntect/two-face 语法高亮。默认 `auto` 会跟随选中的 TUI theme。显式值包括 `catppuccin_mocha`、`catppuccin_latte`、`solarized_dark`、`solarized_light`、`gruvbox_dark`、`gruvbox_light`、`nord`、`one_half_dark`、`one_half_light` 和 `monokai`。

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
max_child_sessions = 8
max_parallel_readonly = 3
max_parallel_write = 1
max_background_threads = 2
max_spawn_fanout_per_turn = 4
max_agent_tokens_per_task = 200000
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

计划任务通过 TUI 里的 `/task <任务>` 发起。`/plan` 只用于只读 planning prompt，不创建 durable task state。`default_mode = "chat"` 会让普通 composer 提交始终保持 chat-first，即使当前 session 里还有未完成 task；需要继续任务时使用 `/task continue` 或 task UI action。只有明确想把计划任务作为默认流程时才改成 `plan`。

各 role 的 provider/model 未配置时继承 `[agent]`。Planner 和 subagent-read 默认只看到只读文件/搜索/code-intelligence 工具。Executor 可以看到完整 runtime registry。Subagent-write 只有在 `allow_write_subagents = true` 时才能看到完整 registry；否则回退到只读工具面。写工具仍然按正常审批策略执行。

Agent fan-out 限制都放在 `[task]`：默认只读子 agent 最多并行 3 个、后台 agent 最多 2 个；需要串行化只读子 agent 时再显式设置 `max_parallel_readonly = 1`。`max_spawn_fanout_per_turn` 应不超过期望的单轮 spawn 上限。旧字段 `allow_parallel_readonly_subagents` 仅为兼容读取保留，当前预算以 `max_parallel_readonly` 为准。

每个 role 都可以覆盖可见工具：

```toml
[task.planner.tools]
names = ["read_file", "ls", "glob", "grep", "code_symbols"]
prefixes = []
allow_all = false
```

使用 name 和 prefix 时要保持克制。Scoped role registry 会同时限制 tool specs、preview、execute、permission hooks 和 egress hooks，因此隐藏工具不是只从 prompt 里省略，而是真的不能执行。

## Providers

`[agent].provider` 选择 runtime provider。对应的 `[providers.*]` 区块控制 endpoint、model、认证和 provider 专项选项。

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
default_mode = "ask"

[permission.access]
read = "allow"

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []
```

含义：

- 未显式覆盖的工具调用默认进入审批。
- 只读文件和搜索工具默认放行。
- workspace 外路径默认不可执行；开启 external directory 后仍会按规则进入审批或放行。
- 临时 scratch 文件应使用 workspace 内 `.sigil/tmp/`。系统 temp 目录（如 `/tmp`、macOS `/private/tmp`、Windows `%TEMP%`）仍属于 workspace 外路径，默认不会放行。
- headless `run` 遇到最终 `ask` 不会静默自动执行，而是向模型回灌结构化 `approval_required` 工具错误。

## Memory

```toml
[memory]
enabled = true
```

启用后，Sigil 启动时会稳定装载工作区根 memory 文档，例如 `SIGIL.md`、`AGENTS.md`、`CLAUDE.md`、`SIGIL.local.md`，并支持单独一行 `@path` 导入。

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

旧配置里的 `context_window_tokens` 仍兼容读取；保存时会写成新的 fallback 字段。

## Code Intelligence

```toml
[code_intelligence]
enabled = false
startup = "lazy"
default_timeout_ms = 5000
max_results = 100
max_payload_bytes = 65536

[code_intelligence.discovery]
enabled = true
report_missing = true
```

开启后，runtime 会注册只读 code intelligence 工具，以及用于 code action 和 symbol rename 的 LSP edit 工具。edit 工具属于 `Write` 工具，必须先展示 diff 审批，获批后才会改文件。TUI 可以用 `Alt-D` 对 git changed source files 触发 diagnostics 检查。

`discovery.enabled = true` 时，Sigil 会按 workspace 自动发现常见语言和 PATH 上可用的安全 LSP server。手写 `code_intelligence.servers` 只作为高级覆盖或补充。

TUI `/config` 面板里有 `Code Intel` 区块，可以调整 `enabled`、`startup` 和 discovery 设置，并查看只读 trust 边界与 readiness 检查。readiness 行复用同一份本地 doctor 事实，所以缺 LSP command 时会在启动 language server 前先给出修复建议。

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
mouse_capture = true
osc52_clipboard = true
scroll_sensitivity = 3
```

`mouse_capture` 控制 TUI 是否向终端请求鼠标事件，用于点击、滚动、审批控件、setup/config/session 选择和 transcript 拖选。如果你的终端或 multiplexer 对 mouse mode 支持不好，可以关闭；键盘操作仍可用。

`osc52_clipboard` 控制 `Ctrl-C` 是否通过 OSC52 序列复制选中的 transcript 文本。如果终端禁用了 OSC52，或者会把控制序列显示成可见文本，可以关闭。关闭后 Sigil 会显示 `clipboard unavailable`，不会再向终端写剪贴板序列。

`scroll_sensitivity` 控制鼠标滚轮每 tick 在 transcript 和 approval diff 中移动的行数。默认值是 `3`；高分辨率滚轮可以调小，终端滚动事件偏慢时可以调大。

TUI `/config` 面板有 `Terminal` 区块可以调整这些控制项。`mouse_capture` 下一次启动生效；`osc52_clipboard` 每次复制时都会读取当前配置；`scroll_sensitivity` 在配置保存并重新加载后应用到运行配置。

`doctor` 会报告配置开关、`TERM`、常见终端 profile 变量、tmux/screen、SSH、WSL 和剪贴板桥接风险。跨 iTerm2、Terminal.app、WezTerm、kitty、tmux 和 SSH 的可重复人工 checklist 见 [terminal-compatibility.md](terminal-compatibility.md)。

## Provider 环境变量 Override

当前支持：

DeepSeek：

- `SIGIL_MODEL`
- `SIGIL_API_KEY`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_REQUEST_TIMEOUT_SECS`
- `SIGIL_STRICT_TOOLS_MODE`

`SIGIL_API_KEY` 优先级最高。`DEEPSEEK_API_KEY` 作为 DeepSeek provider 的备用来源继续兼容读取。如果只配置了 `[providers.deepseek].api_key`，Sigil 会把它视为明文配置认证，`doctor` 会输出 warning 和修复建议。

OpenAI-compatible：

- `SIGIL_OPENAI_COMPATIBLE_MODEL`
- `SIGIL_OPENAI_COMPATIBLE_API_KEY`
- `SIGIL_OPENAI_COMPATIBLE_BASE_URL`
- `SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS`

`OPENAI_API_KEY` 作为 OpenAI-compatible provider 的备用来源继续读取。

Anthropic：

- `SIGIL_ANTHROPIC_MODEL`
- `SIGIL_ANTHROPIC_API_KEY`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_ANTHROPIC_VERSION`
- `SIGIL_ANTHROPIC_MAX_TOKENS`
- `SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS`

`ANTHROPIC_API_KEY` 作为 Anthropic provider 的备用来源继续读取。

Gemini：

- `SIGIL_GEMINI_MODEL`
- `SIGIL_GEMINI_API_KEY`
- `SIGIL_GEMINI_BASE_URL`
- `SIGIL_GEMINI_REQUEST_TIMEOUT_SECS`

`GEMINI_API_KEY` 和 `GOOGLE_API_KEY` 作为 Gemini provider 的备用来源继续读取。

## Plugins

Workspace plugin manifest 从 `.sigil/plugins/<id>/plugin.toml` 发现。它们通过 TUI review，不在 `sigil.toml` 里直接编辑。

打开 `/config`，进入 `Plugins`，用 `PgUp/PgDn` 选择已发现 manifest。detail view 会展示 trust 状态、相对 manifest 路径、完整 manifest hash、skill 路径、带 args 和 approval mode 的 hook command，以及带 args、startup 和 required 状态的 MCP server command。footer 的 `approve` 只信任当前展示的 manifest hash；`deny` 会禁用这个 hash。记录决策前 Sigil 会重新加载 manifest，所以 hash 改变后必须重新 review。

## MCP

MCP server 使用 `[[mcp_servers]]` 配置，详见 [mcp.md](mcp.md)。
