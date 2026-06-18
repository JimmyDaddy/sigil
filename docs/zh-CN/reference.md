# 命令与键位参考

[文档首页](README.md) · [English](../en/reference.md)

这一页集中列出较长指南中分散的用户可见命令、键位、路径和环境变量。

## TUI 键位

| 动作 | 键位 |
| --- | --- |
| 打开帮助 | `F1` |
| 打开 slash command selector | `/` |
| 提交 prompt 或已选 slash command | `Enter` |
| 滚动 transcript | `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End` |
| 切换默认 permission mode | `Shift-Tab` |
| 取消当前 run | `Ctrl-C` |
| 离开 overlay 或清除 activity focus | `Esc` |
| 聚焦最新 activity | `Ctrl-G` |
| 在 activities 间移动 | `Alt-J` / `Alt-K` |
| 展开或折叠 thinking / activity | `Ctrl-T` |
| 对 changed source files 运行 code diagnostics | `Alt-D` |

当 composer 聚焦时，`Up/Down` 会优先处理 prompt history 或 multiline input 内的光标移动。

## Slash Commands

| Command | 作用 |
| --- | --- |
| `/config` | 打开 TUI config panel |
| `/doctor` | 在 transcript 中运行本地 setup diagnostics |
| `/new` | 使用当前 provider 和 model 开始新 session |
| `/resume` | 选择并恢复历史 session |
| `/plan <task>` | 创建 durable plan，并分步骤执行任务 |
| `/plan continue` | 不带额外 guidance 继续最新 planned task |
| `/model <flash|pro|id>` | 切换下一轮 run 的 model，并开始新 session |
| `/effort <low|medium|high|max>` | 切换下一轮 run 的 reasoning effort |
| `/compact` | 手动 compact 当前 session 的 provider-visible context |
| `/quit` | 退出 TUI |

Aliases：`/m` 对应 `/model`，`/e` 对应 `/effort`，`/q` 或 `/exit` 对应 `/quit`。

`/model`、`/effort` 和 `/resume` 会展示候选项。使用 `Up/Down` 选择，`Tab` 接受，`Enter` 执行。

## CLI Commands

| Command | 用途 |
| --- | --- |
| `sigil` | 在当前 workspace 打开 TUI |
| `sigil doctor` | 运行本地诊断 |
| `sigil run "<task>"` | 运行非交互自动化任务 |
| `sigil --version` | 打印安装版本 |
| `sigil --config <path> doctor` | 使用显式 config 文件运行诊断 |

子命令用于自动化、诊断和脚本。完整产品表面是 TUI。

## Config 解析顺序

Sigil 按以下顺序解析 config：

1. `--config <path>`
2. 当前工作目录下的 `./sigil.toml`
3. 标准用户配置目录下的 `sigil.toml`

常见用户级路径：

- macOS: `~/Library/Application Support/sigil/sigil.toml`
- Linux: `$XDG_CONFIG_HOME/sigil/sigil.toml` 或 `~/.config/sigil/sigil.toml`
- Windows: `%APPDATA%\sigil\sigil.toml`

## 重要路径

| Path | 含义 |
| --- | --- |
| `.sigil/sessions/` | workspace 下默认 append-only session logs |
| `sigil.toml` | 本地或用户配置 |
| `SIGIL.md` | 稳定 workspace memory file |
| `AGENTS.md` | Sigil 可作为 memory 加载的 agent 协作说明 |
| `SIGIL.local.md` | 本地专用 memory file |

不要提交包含真实 secret 的 `sigil.toml` 或本地 memory 文件。

## Provider 环境变量

DeepSeek:

- `SIGIL_API_KEY`
- `SIGIL_MODEL`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_REQUEST_TIMEOUT_SECS`
- `SIGIL_STRICT_TOOLS_MODE`
- `DEEPSEEK_API_KEY` fallback

OpenAI-compatible:

- `SIGIL_OPENAI_COMPATIBLE_API_KEY`
- `SIGIL_OPENAI_COMPATIBLE_MODEL`
- `SIGIL_OPENAI_COMPATIBLE_BASE_URL`
- `SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS`
- `OPENAI_API_KEY` fallback

Anthropic:

- `SIGIL_ANTHROPIC_API_KEY`
- `SIGIL_ANTHROPIC_MODEL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_ANTHROPIC_VERSION`
- `SIGIL_ANTHROPIC_MAX_TOKENS`
- `SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS`
- `ANTHROPIC_API_KEY` fallback

Gemini:

- `SIGIL_GEMINI_API_KEY`
- `SIGIL_GEMINI_MODEL`
- `SIGIL_GEMINI_BASE_URL`
- `SIGIL_GEMINI_REQUEST_TIMEOUT_SECS`
- `GEMINI_API_KEY` fallback
- `GOOGLE_API_KEY` fallback

## 常见 Config Sections

| Section | 作用 |
| --- | --- |
| `[workspace]` | Workspace root |
| `[agent]` | Provider、model、tool timeout、可选 max turns |
| `[providers.deepseek]` | DeepSeek provider 设置 |
| `[providers.openai_compat]` | OpenAI-compatible provider 设置 |
| `[providers.anthropic]` | Anthropic provider 设置 |
| `[providers.gemini]` | Gemini provider 设置 |
| `[permission]` | 默认审批策略 |
| `[memory]` | Workspace memory loading |
| `[compaction]` | Context compaction 阈值 |
| `[task]` | Planned task 行为和 role settings |
| `[code_intelligence]` | LSP 和 code intelligence tools |
| `[terminal]` | Mouse、OSC52 clipboard 和 scroll 行为 |
| `[[mcp_servers]]` | stdio MCP server 配置 |

示例见 [configuration.md](configuration.md)。

## Approval Outcomes

| Outcome | 含义 |
| --- | --- |
| allow | 执行 tool call |
| deny | 向模型返回结构化 denial |
| timeout | 等待过久后自动 deny |
| approval_required | Headless mode 需要决策但无法交互询问 |

## Session Recovery Facts

- Session logs 是 append-only JSONL。
- 重启会恢复可见 session state。
- Started tools 没有 terminal records 时会恢复为 interrupted。
- 恢复不会静默重放未完成工具。
- `/new` 会开始一条新的 append-only session log。
- `/resume` 选择历史 session。
- 存在未完成 planned task 时，`/plan continue` 会继续最新任务。
