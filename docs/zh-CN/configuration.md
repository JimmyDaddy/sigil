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
cargo run -p sigil-tui
```

如果你更喜欢环境变量，可以在启动前提供认证：

```bash
export SIGIL_API_KEY="sk-..."
cargo run -p sigil-tui
```

如果没有配置文件，TUI 会进入 Quick Setup，并在保存后生成可用配置。后续可以用 `/config` 调整常用项。

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

[providers.deepseek]
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
# 推荐优先使用 SIGIL_API_KEY；如果写在这里，会以 plaintext 保存。
# api_key = "sk-..."
```

`SIGIL_API_KEY` 优先级高于配置文件里的 `api_key`。旧环境变量 `DEEPSEEK_API_KEY` 仍作为 DeepSeek provider 的备用来源读取。

## Workspace

```toml
[workspace]
root = "."
```

`workspace.root = "."` 有特殊语义：`.` 会在启动时解析成运行 `sigil-tui` 或 `sigil-cli` 时所在的目录。这样同一份用户级配置可以跟随你当前打开的仓库工作。

文件类工具会限制在 workspace root 内，拒绝 `..`、绝对路径和指向 workspace 外的 symlink。`bash` 仍不提供完整进程 sandbox。

## Agent

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30
# max_turns = 20
```

- `provider`：当前 runtime 使用的 provider 名称。
- `model`：默认模型。
- `tool_timeout_secs`：工具执行超时。
- `max_turns`：可选保险丝。默认不限制；如果显式设置，模型连续达到阈值仍只请求工具而没有最终回答时，本轮会可恢复地停止。

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

TUI 的 `/config` 只暴露高频项，例如 `model`、`api_key`、`base_url` 和 `fim_model`。`beta_base_url`、`anthropic_base_url`、`user_id_strategy`、`request_timeout_secs` 和 `strict_tools_mode` 属于低频或 provider 专项项，保留给配置文件和环境变量。

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

开启后，runtime 会注册只读 code intelligence 工具，并允许 TUI 用 `Alt-D` 对 git changed source files 触发 diagnostics 检查。

`discovery.enabled = true` 时，Sigil 会按 workspace 自动发现常见语言和 PATH 上可用的安全 LSP server。手写 `code_intelligence.servers` 只作为高级覆盖或补充。

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

## Provider 环境变量 Override

当前支持：

- `SIGIL_MODEL`
- `SIGIL_API_KEY`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_REQUEST_TIMEOUT_SECS`
- `SIGIL_STRICT_TOOLS_MODE`

`SIGIL_API_KEY` 优先级最高。`DEEPSEEK_API_KEY` 作为 DeepSeek provider 的备用来源继续兼容读取。

## MCP

MCP server 使用 `[[mcp_servers]]` 配置，详见 [mcp.md](mcp.md)。
