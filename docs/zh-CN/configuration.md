# Sigil 配置指南

[English](../en/configuration.md)

本文说明 Sigil 的用户配置方式。开发者需要修改配置 schema 时，请同步阅读 `dev/governance/code-standards.md` 和 `dev/governance/engineering-standards.md`。

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

[providers.deepseek]
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# 推荐优先使用 SIGIL_API_KEY；如果写在这里，会以 plaintext 保存。
# api_key = "sk-..."
```

`SIGIL_API_KEY` 优先级高于配置文件里的 `api_key`。旧环境变量 `DEEPSEEK_API_KEY` 仍作为 DeepSeek provider 的备用来源读取。`doctor` 会对仅来自明文配置的认证给 warning，但不会阻止运行。

如果要接 OpenAI-compatible endpoint，把 provider 切到 `[providers.openai_compat]`：

```toml
[agent]
provider = "openai_compat"
model = "gpt-4.1"
tool_timeout_secs = 30

[providers.openai_compat]
base_url = "https://api.openai.com/v1"
model = "gpt-4.1"
# 优先使用 SIGIL_OPENAI_COMPATIBLE_API_KEY 或 OPENAI_API_KEY。
# api_key = "sk-..."
```

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

- `provider`：当前 runtime 使用的 provider 名称。当前支持 `deepseek` 和 `openai_compat`。
- `model`：默认模型。
- `tool_timeout_secs`：工具执行超时。
- `max_turns`：可选保险丝。默认不限制；如果显式设置，模型连续达到阈值仍只请求工具而没有最终回答时，本轮会可恢复地停止。

## Task Planning

```toml
[task]
enabled = true
default_mode = "chat"
max_plan_steps = 12
max_replans = 2
max_child_sessions = 8
allow_parallel_readonly_subagents = false
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

计划任务通过 TUI 里的 `/plan <任务>` 发起。`default_mode = "chat"` 会让普通 composer 提交在当前 session 没有 task context 时继续走 chat-first；已有 task context 后，composer 输入会作为 guidance 继续最近 task。只有明确想把计划任务作为默认流程时才改成 `plan`。

各 role 的 provider/model 未配置时继承 `[agent]`。Planner 和 subagent-read 默认只看到只读文件/搜索/code-intelligence 工具。Executor 可以看到完整 runtime registry。Subagent-write 只有在 `allow_write_subagents = true` 时才能看到完整 registry；否则回退到只读工具面。写工具仍然按正常审批策略执行。

每个 role 都可以覆盖可见工具：

```toml
[task.planner.tools]
names = ["read_file", "ls", "glob", "grep", "code_symbols"]
prefixes = []
allow_all = false
```

使用 name 和 prefix 时要保持克制。Scoped role registry 会同时限制 tool specs、preview、execute、permission hooks 和 egress hooks，因此隐藏工具不是只从 prompt 里省略，而是真的不能执行。

## DeepSeek Provider

```toml
[providers.deepseek]
base_url = "https://api.deepseek.com"
beta_base_url = "https://api.deepseek.com/beta"
anthropic_base_url = "https://api.deepseek.com/anthropic"
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# api_key = "sk-..."
user_id_strategy = "stable_per_end_user"
strict_tools_mode = "auto"
request_timeout_secs = 120
```

TUI 的 `/config` 只暴露高频项，例如 `model`、`api_key`、`base_url` 和 `fim_model`。通过 `/config` 保存 `api_key` 会把它以明文写入 `sigil.toml`；临时或 CI 场景优先用 `SIGIL_API_KEY`。`beta_base_url`、`anthropic_base_url`、`user_id_strategy`、`request_timeout_secs` 和 `strict_tools_mode` 属于低频或 provider 专项项，保留给配置文件和环境变量。

## OpenAI-compatible Provider

```toml
[providers.openai_compat]
base_url = "https://api.openai.com/v1"
model = "gpt-4.1"
# api_key = "sk-..."
organization = "org_..."
project = "proj_..."
request_timeout_secs = 120
```

这个 provider 使用 Chat Completions streaming 形态，支持 text delta、流式 tool calls、usage 和可选 `system_fingerprint`。它不提供 DeepSeek 专属的 prefix/FIM、reasoning replay、strict tools mode 或 beta endpoint 配置。

对这个 provider，`SIGIL_OPENAI_COMPATIBLE_API_KEY` 优先级最高，`OPENAI_API_KEY` 作为备用来源读取。`SIGIL_OPENAI_COMPATIBLE_MODEL`、`SIGIL_OPENAI_COMPATIBLE_BASE_URL` 和 `SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS` 会覆盖对应配置项。

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

## MCP

MCP server 使用 `[[mcp_servers]]` 配置，详见 [mcp.md](mcp.md)。
