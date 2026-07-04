# 命令与键位参考

[文档首页](README.md) · [English](../en/reference.md)

这一页集中列出较长指南中分散的用户可见命令、键位、路径和环境变量。

## TUI 键位

| 动作 | 键位 |
| --- | --- |
| 打开帮助 | `F1` |
| 打开 slash command selector | `/` |
| 提交 prompt 或已选 slash command | `Enter` |
| 切换右侧 info rail 精简/详情 | `F2` |
| 滚动 transcript | `PageUp/PageDown`, `Ctrl-U/D`, `Ctrl-Home/End` |
| 切换默认 permission mode | `Shift-Tab` |
| 插入 composer 换行 | `Ctrl-J`；terminal keyboard enhancement 已启用且能上报 modifier 时也支持 `Shift-Enter` / `Alt-Enter` |
| 按行或字符移动 composer 光标 | `Ctrl-A/E`、`Ctrl-B/F`、`Left/Right` |
| 按词移动 composer 光标 | `Alt-B/F`、`Ctrl-Left/Right` |
| 删除 composer 文本 | `Backspace/Delete`、`Ctrl-H`、`Ctrl-W`、`Ctrl/Alt-Backspace`、`Ctrl/Alt-Delete` |
| kill/yank composer 行尾 | `Ctrl-K/Y` |
| 恢复最近一次被 Esc 清空的 draft | `Ctrl-Z` |
| 取消当前 run | `Ctrl-C` |
| 离开 overlay 或清除 activity focus | `Esc` |
| 聚焦最新 activity | `Ctrl-G` |
| 在 activities 间移动 | `Alt-J` / `Alt-K` |
| 切换可见 agent transcript | composer agent 面板（`Down`、`Up/Down`、`Enter`）、`Alt-A`、`Shift-Alt-A` |
| 展开或折叠 thinking / activity | `Ctrl-T` |
| 对 changed source files 运行 code diagnostics | `Alt-D` |
| 取消聚焦的运行中 terminal task | `Alt-X` |

当 composer 聚焦时，`Up/Down` 会优先处理 prompt history 或 multiline input 内的光标移动。如果存在 child agent，输入光标位于 composer 最后一行时按 `Down` 会聚焦 composer agent 面板。`Ctrl-Z` 只恢复被 `Esc` 清空的单个 draft，不是通用 undo 栈。

## Slash Commands

| Command | 作用 |
| --- | --- |
| `/config` | 打开 TUI config panel |
| `/doctor` | 在 transcript 中运行本地 setup diagnostics |
| `/new` | 使用当前 provider 和 model 开始新 session |
| `/resume` | 选择并恢复历史 session |
| `/agent <main|child-id>` | 在 parent session 和 child agent transcript 之间切换主聊天区 |
| `/agent rename <child-id|current> <name>` | 为 child agent transcript 持久化一个短展示名 |
| `/queue` | 高级 follow-up 控制 |
| `/queue next|interrupt|edit|delete [item]` | 保留 follow-up 到下一轮、interrupt 并立即运行、编辑或取消 |
| `/plan` / `/plan <prompt>` | 运行一次只读 planning prompt；接受 plan card 后创建并运行 durable task |
| `/task <task>` | 创建 durable plan，并分步骤执行任务 |
| `/task continue` | 不带额外 guidance 继续最新 planned task |
| `/model <flash|pro|id>` | 切换下一轮 run 的 model，并开始新 session |
| `/effort <low|medium|high|max>` | 切换下一轮 run 的 reasoning effort |
| `/compact` | 手动 compact 当前 session 的 provider-visible context |
| `/quit` | 退出 TUI |

Aliases：`/m` 对应 `/model`，`/e` 对应 `/effort`，`/q` 或 `/exit` 对应 `/quit`。

Workspace trust 由启动时的 workspace trust gate 处理，不是 slash command。trust decision 会记录进 session audit log；它允许仓库本地 verification 候选检查被提升为 task readiness 可用的检查，但不会单独授予 shell、plugin、MCP 或文件写入权限。

`/model`、`/effort`、`/resume`、`/agent` 和 `/queue` 会展示候选项。使用 `Up/Down` 选择，`Tab` 接受，`Enter` 执行。`/agent rename` 会在输入新名字前展示 child agent 候选项。

## CLI Commands

| Command | 用途 |
| --- | --- |
| `sigil` | 在当前 workspace 打开 TUI |
| `sigil doctor` | 运行本地诊断 |
| `sigil run "<task>"` | 运行非交互自动化任务 |
| `sigil serve` | 校验 HTTP/SSE adapter 的 local bind/token 默认值；HTTP routing 尚未实现 |
| `sigil --version` | 打印安装版本 |
| `sigil --config <path> doctor` | 使用显式 config 文件运行诊断 |

子命令用于自动化、诊断、脚本和 adapter preflight check。完整产品表面是 TUI。

## Config 解析顺序

Sigil 按以下顺序解析 config：

1. `--config <path>`
2. 用户可见 Sigil 配置目录下的 `sigil.toml`

默认用户配置路径：

- `~/.sigil/sigil.toml`

## 重要路径

| Path | 含义 |
| --- | --- |
| 用户态 state root `workspaces/<workspace-id>/sessions/` | 默认 append-only session logs |
| 用户态 state root `workspaces/<workspace-id>/input-history.jsonl` | composer input history |
| 用户态 state root `workspaces/<workspace-id>/artifacts/` | terminal 和 changeset artifacts |
| 用户态 cache root `workspaces/<workspace-id>/tmp/` | shell scratch 目录，通过 `$SIGIL_SCRATCH_DIR` 暴露，对模型显示为 `cache/tmp` |
| 用户配置目录 `sigil.toml` | 默认本机配置 |
| `.sigil/agents/`、`.sigil/skills/`、`.sigil/plugins/` | 可选 workspace project assets |
| `SIGIL.md` | 稳定 workspace memory file |
| `AGENTS.md` | Sigil 可作为 memory 加载的 agent 协作说明 |
| `SIGIL.local.md` | 本地专用 memory file |

不要提交包含真实 secret 的 `sigil.toml` 或本地 memory 文件。workspace 根目录的 `sigil.toml` 默认不会被读取；如需实验配置，请显式传入 `--config <path>`。

## Provider 环境变量

Model request:

- `SIGIL_MODEL_REQUEST_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS`
- `SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS`

DeepSeek:

- `SIGIL_API_KEY`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_STRICT_TOOLS_MODE`

OpenAI-compatible:

- `SIGIL_OPENAI_COMPATIBLE_API_KEY`
- `SIGIL_OPENAI_COMPATIBLE_BASE_URL`

Anthropic:

- `SIGIL_ANTHROPIC_API_KEY`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_ANTHROPIC_VERSION`
- `SIGIL_ANTHROPIC_MAX_TOKENS`

Gemini:

- `SIGIL_GEMINI_API_KEY`
- `SIGIL_GEMINI_BASE_URL`

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
| `[verification]` | 显式用户批准的 verification checks |
| `[code_intelligence]` | LSP 和 code intelligence tools |
| `[terminal]` | Mouse、OSC52 clipboard 和 scroll 行为 |
| `[appearance]` | TUI 主题、usage cost currency 和语义颜色覆盖 |
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
- 存在未完成 planned task 时，`/task continue` 会继续最新任务。
