# Sigil

`sigil` 是一个 **TUI-first** 的 Rust AI coding agent。它的目标不是做一组零散命令，而是提供一个可复用的 agent 内核，并优先把真正面向用户的终端交互壳做好。

当前仓库已经具备这些核心能力：

- 通用 `kernel`：统一承载 provider、tool、session、approval、event 契约
- 共享 `runtime`：TUI / CLI 统一通过 `sigil-runtime` 装配 provider、内置工具、MCP 工具和 run options
- `DeepSeek-first` provider：优先支持 DeepSeek 流式对话、工具调用、reasoning replay 与 Beta 扩展点
- 内置工具注册表：文件读写、编辑、搜索、shell 执行
- Code intelligence：可选 LSP / Tree-sitter 代码智能，提供符号、定义、引用和诊断只读工具
- stdio MCP 工具接入：远程工具通过统一 `ToolRegistry` 暴露给 agent
- 权限策略：只读工具默认放行，其它访问默认审批；headless `ask` 会回灌结构化 `approval_required` 错误而不是静默执行
- 文档 memory boot：工作区根下 `SIGIL.md` / `AGENTS.md` / `CLAUDE.md` / `SIGIL.local.md` 与 `@path` 导入
- context compaction：soft threshold 提示、hard threshold 在 idle 边界自动压缩，append-only 审计日志不改写
- TUI 主入口：消息区、事件流、审批流、session 恢复、diff 预览、`/compact`、`/model`、`/effort`、Quick Setup、`/config`

## 仓库结构

```text
sigil/
  crates/
    sigil-kernel/              # 通用 agent 内核与领域契约
    sigil-provider-deepseek/   # DeepSeek provider 实现
    sigil-tools-builtin/       # 内置工具
    sigil-code-intel/          # LSP client、Tree-sitter fallback 与 code intelligence tools
    sigil-mcp/                 # stdio MCP client 与工具适配
    sigil-runtime/             # 入口共享的 provider / tool / run options 装配
    sigil-cli/                 # 薄 CLI 启动器与调试入口
    sigil-tui/                 # 第一用户入口
  dev/governance/                # 开发约束、代码规范、工程规范
  dev/docs/                      # 架构与技术方案
  sigil.toml                   # 本地配置文件，默认被 .gitignore 忽略
```

`sigil-tui` 当前保持 `src/app.rs` 作为 `AppState` façade；具体状态流拆在 `src/app/*_flow.rs`、`tool_focus.rs`、`worker_bridge.rs` 和 `formatting.rs`，对应测试拆在 `src/app/tests/*_tests.rs`。新增 TUI 行为时优先落到对应 flow 和同域测试，不要把状态机重新堆回 `app.rs`。

## 当前入口

### 1. TUI

推荐优先使用 TUI：

```bash
cargo run -p sigil-tui
```

TUI 当前支持：

- 提交 prompt 并流式查看输出
- prompt 提交后 composer 会清空并保持可见；主聊天区改为 app-owned transcript，可用 `PageUp/PageDown`、`Ctrl-U/D`、`Ctrl-Home/End` 和滚轮持续回溯到会话最顶，同时历史会分批同步进终端原生 scrollback
- 输入 `/` 时弹出 slash command selector，支持 `Up/Down` 选中、`Tab` 接受、`Enter` 执行；`/model`、`/effort`、`/resume` 会继续下钻参数候选，其中 `/resume` 展示可恢复 session 标题，当前内置 `/compact`、`/config`、`/effort`、`/model`、`/quit`、`/resume`
- `F1` 打开 keyboard help；核心快捷键、activity 快捷键和公开 slash command 列表都从真实命令面生成，不依赖隐藏兼容入口
- composer 的历史输入只响应键盘：composer 聚焦时 `Up/Down` 显示历史 prompt；多行输入中间行仍按垂直光标移动处理
- `Alt-D` 会在空闲时检查 git changed source files，并用真实 `code_diagnostics` 工具生成普通 activity；没有 changed source files 时只给轻量 notice
- assistant / thinking markdown 的 fenced code block 会按语言做语法高亮；未知语言、纯文本或超大代码块自动回退到普通代码块渲染
- `/config` 打开 TUI guided config flow；provider 主流程只保留 `model / api_key / base_url / fim_model`，文本项统一走弹窗输入；顶部 status strip 展示当前 section、字段、保存状态和配置路径，主面板展示 section tabs 与字段列表；宽屏下配置内容会保持居中最大宽度，Details 作为右侧说明栏，窄屏下说明内联到选中字段附近；`Actions` 栏跟随当前配置面板收在内容区内，可用 `Down` 聚焦，再用 `Left/Right` 选 `save / save+close / close`，并按宽度显示完整或紧凑的 `saved / unsaved / confirm close` 状态
- 主屏默认走 chat-first：inline viewport 会占满当前终端可视区，左侧主区域展示 live transcript + 底部 composer，右侧保留独立的 full-height `info rail`，窄终端会自动收起 info rail 给 chat/composer 让出空间；启动恢复旧会话时会把完整 scrollback 分批 seed 到 terminal scrollback，避免长会话集中在单帧重放
- 不再要求 `Tab` 在主屏各卡片之间切焦点；`Shift-Tab` 直接轮换并持久化默认 `allow / ask / deny` 权限模式
- composer 顶部直接展示 mode / model / provider / reasoning effort；运行态统一沉到底部 run strip，只保留 interrupt/details 快捷键和右下角 context 使用状态，当前任务进度交给 chat 区域展示
- 运行中 live transcript 底部会显示紧凑的 loading progress block，例如 `▰▱▱▱ Thinking...`、`Bash...`、`Read...` 和当前 reasoning/tool/streaming 摘要；这些运行态提示只做渲染层投影，不写回 durable transcript
- 右侧 `Info rail` 独立占据整列，展示 `Session / Permissions / Agents / LSP / Usage / Controls` 六组状态，而不是挤进 composer 旁边的一个小角落；`LSP` 会按 language/server 保留最近状态，例如 `rust: ready rust-analyzer`，跨语言 workspace symbol 查询会合并多个 server 的结果和状态；最近一次 diagnostics 会在这里保留文件级 errors / warnings / clean 摘要
- `ctx`、compaction status 和 auto-compaction 统一按同一个 effective context window 计算：已知模型窗口优先，其次才回退到 `compaction.fallback_context_window_tokens`
- assistant / tool 输出继续走线性展开：assistant markdown 按段落展开，tool result 改成 action-first activity 展示，例如 `Ran cargo test -p sigil-tui`、`Searched needle in src/main.rs`、`Read README.md`、`Deleted note.txt`；activity header 会区分动作词、命令/路径和参数；`read_file / ls / glob / grep / bash / write_file / edit_file / delete_file / code_symbols / code_workspace_symbols / code_definition / code_references / code_diagnostics` 走专用 renderer，其中 code intelligence 卡片会展示 LSP/Tree-sitter 来源、server、capability、server breakdown 和结果位置；简单只读 `rg / grep / fd / find` bash 命令会识别为 `Searched`，其他结构化 payload 走树形 fallback，不直接 dump 原始 JSON 或 call id
- live phase 只保留在运行态和事件流里，不再固化成 chat transcript；reasoning delta 会写入 append-only control log，用于取消或重启后的 thinking block 恢复；completed thinking 默认显示前几行预览，用 `Ctrl-T` 完整展开或收起
- tool result 默认以独立 brief activity 展示；bash 成功无输出会显示 `(no output)`，失败会突出 exit code 并优先展示诊断输出；存在 activity 后右侧 `Info rail / Controls` 会显示 `Ctrl-G` 聚焦最新 activity、`Alt-J` / `Alt-K` 切换 activity、`Ctrl-T` 展开/收起聚焦 activity，composer 为空时 `Esc` 清除 activity focus
- `write_file` / `edit_file` / `delete_file` 的结果 activity 默认展开执行时捕获的 bounded unified diff，diff 行会显示旧/新行号；activity 正文会跳过重复的 `@@` hunk header，并在文件头汇总 hunk 数；仍可用 `Ctrl-T` 收起，大 diff 会显示 `diff truncated · N lines hidden`，折叠态保留 diff stats 和隐藏提示
- 工具调用审批改为居中 review card：固定 `Summary / Files / Diff / Actions` 四区，composer 不会因为审批而消失；`Actions` 支持 `Left/Right` 选择 allow/deny 后 `Enter` 确认，也保留 `Y/N` 直达；审批通道 5 分钟无决策会自动 deny，避免 worker 永久等待
- `write_file` / `edit_file` / `delete_file` diff 预览支持按文件切换、按 hunk 跳转和 diff mode 切换；如果最近一次 `code_diagnostics` 覆盖了 affected file，审批 Files 列和 Diff 状态行会显示对应 errors / warnings 摘要
- `/compact` 手动压缩当前会话的 provider 可见上下文
- `/model <flash|pro|id>` 切换运行时模型，并开启一个 fresh session，避免把旧 session identity 和新模型混在一起
- `/effort <low|medium|high|max>` 切换下一轮 agent run 的 reasoning effort
- soft / hard context threshold 提示与 idle 边界自动 compaction
- `Esc` 或 `Ctrl-C` 取消运行后从 durable JSONL log 恢复当前 session 视图
- TUI 重启时会默认恢复最近一次 durable session，而不是静默丢到一个全新的空白会话
- 首次启动或配置损坏时，直接进入极简 Quick Setup，只确认“信任当前目录 / 模型 / 认证”；模型走候选选择，API key 走隐藏输入，当前启动目录自动成为 workspace
- Quick Setup 保存后会写入 `workspace.root = "."`；当配置位于用户配置目录时，`.` 会在运行时解析成当前启动目录，而不是配置文件所在目录
- 长输出默认在主 transcript 内滚动查看；审批浮层继续保留键盘滚动

### 2. CLI

CLI 目前主要是自动化和调试入口：

```bash
cargo run -p sigil-cli -- run "总结一下当前仓库"
```

默认只暴露 `run`。`prefix` / `fim` 等 provider 专项入口保留在实现层，但不作为普通用户主心智。

## 快速开始

### 1. 准备配置

仓库根目录不会跟踪真实 `sigil.toml`，避免误提交本地密钥。可以直接启动 TUI 走 Quick Setup 生成配置，也可以手动创建：

`./sigil.toml`

至少需要准备：

- `SIGIL_API_KEY`
- 如果你想手写配置，再按下方“配置要点”准备 provider / workspace / session / permission / memory / compaction 配置

TUI / CLI 当前按这个顺序找配置：

1. `--config <path>`
2. 当前工作目录下的 `./sigil.toml`
3. 标准用户配置目录里的 `sigil.toml`

标准用户配置路径：

- macOS：`~/Library/Application Support/sigil/sigil.toml`
- Linux：`$XDG_CONFIG_HOME/sigil/sigil.toml` 或 `~/.config/sigil/sigil.toml`
- Windows：`%APPDATA%\\sigil\\sigil.toml`

如果 TUI 启动时没有可用配置，或配置加载失败，它不会直接退出，而是进入 Quick Setup，把配置写回上述目标路径。首配只要求授权当前目录、从候选里选择模型并填好认证；认证可以直接录入 `api_key` 并以 plaintext 形式持久化到本地配置文件，也可以预先导出 `SIGIL_API_KEY` 在运行时覆盖。配置文件默认应放在用户配置目录或被仓库 `.gitignore` 忽略的 `sigil.toml`，不要提交到版本库。如果要用自定义模型 id，可以先 `Esc` 退出选择器后手动输入。后续 `/config` 继续只暴露高频项；更细的 provider 兼容字段改走配置文件或环境变量。

### 2. 启动方式

```bash
# 推荐
cargo run -p sigil-tui

# 自动化 / 脚本场景
cargo run -p sigil-cli -- run "read README and summarize the repo"
```

如果想把配置写到自定义位置：

```bash
cargo run -p sigil-tui -- --config /absolute/path/to/sigil.toml
```

### 3. Session 持久化

默认 session log 落在：

```text
.sigil/sessions/
```

当前实现采用 append-only JSONL，便于审计、恢复与 provider continuation state 持久化。

当前恢复语义还包括：

- session identity 会跟随 durable log 恢复，而不是盲目回退到当前配置里的 provider/model
- response handle、provider continuation state、prefix snapshot 与 compaction record 都写入 append-only control log；resume 后下一轮 request 会恢复最新匹配 provider 的 response handle
- tool approval、execution lifecycle 和 reasoning delta 会追加到 control log；已开始但没有完成记录的工具执行在恢复时标记为 `interrupted`，并用同一套 activity renderer 重建为用户可读卡片；悬空 tool call 会投影为结构化 `interrupted` tool result
- 文件变更工具的历史结果卡片会随 session restore 恢复；恢复后仍可回看 `write_file` / `edit_file` / `delete_file` 当时捕获的 bounded diff，下一轮模型上下文只保留工具结果摘要
- `/config` 保存后的默认 provider/model 不会静默改写当前 session identity；当前会话仍以 durable log 中的身份为准，新默认值会用于后续新 session 或空白 session
- 运行中取消后，TUI 会从 durable JSONL log 重建会话视图，避免把临时内存态当成恢复真相
- 每轮 usage 会追加持久化 control 记录，session resume 后可恢复 cache hit、累计 usage 和最近一次 prompt pressure
- compaction 只追加 `CompactionApplied` control 记录，不改写旧历史；后续请求使用稳定 summary + tail 投影
- hard threshold 自动 compaction 只在 run 回到 idle 后触发，不会抢占当前流式执行

## 配置要点

新增的默认配置块如下：

```toml
[permission]
default_mode = "ask"

[permission.access]
read = "allow"

[permission.external_directory]
enabled = false
default_mode = "ask"
rules = []

[memory]
enabled = true

[compaction]
enabled = true
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
# Optional: only used when provider/model metadata cannot resolve a window.
# Old configs with context_window_tokens still load as this fallback.
# fallback_context_window_tokens = 128000
tail_messages = 6

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

当前语义：

- `permission.default_mode = "ask"`：未显式覆盖的工具调用默认进入审批；`permission.access.read = "allow"` 让只读文件/搜索工具默认放行
- `bash` 静态属于 `Execute`；只有简单只读 allowlist 命令会在本次调用中降为 `Read`，复杂 shell 语法、重定向、变量展开、未知命令和包管理/测试/写操作仍按 `Execute` 审批
- `permission.external_directory.enabled = false`：workspace 外路径默认不会执行；开启后仍按 `default_mode` 和 `rules` 进入审批或放行，不会扩大 workspace root
- headless run 遇到最终 `ask` 不会静默自动执行，而是向模型回灌结构化 `approval_required` 工具错误
- 内置工具的 model-visible 输出默认有上限；`read_file` 支持 `offset/limit`，`ls/glob/grep` 支持 `limit`，截断信息会写入 tool result metadata
- `agent.max_turns` 默认不限制；如果用户在配置里显式设置该阈值，并且模型连续达到阈值仍只请求工具、没有给最终回答，TUI 会提示本轮已停止并保留已写入的 tool results；这不是工具执行失败，用户可继续发送下一条消息接着跑
- `memory.enabled = true`：启动时稳定装载工作区根 memory 文档，并支持单独一行 `@path` 导入
- `compaction.*`：控制 `/compact` 手动压缩、soft threshold 提示和 hard threshold 的 idle 自动 compaction；若当前模型已有已知 context window，会优先按模型窗口计算阈值，否则回退到 `fallback_context_window_tokens`；旧配置里的 `context_window_tokens` 会继续兼容读取，但保存时会写成新的 fallback 字段
- `code_intelligence.*`：默认关闭；开启后 runtime 会注册 `code_symbols`、`code_workspace_symbols`、`code_definition`、`code_references`、`code_diagnostics` 只读工具，并允许 TUI 用 `Alt-D` 对 git changed source files 触发 diagnostics 检查。工具结果同时受 `max_results` 和 `max_payload_bytes` 约束。`discovery.enabled = true` 时会按 workspace 自动发现常见语言和 PATH 上可用的安全 LSP server；手写 `code_intelligence.servers` 只作为高级覆盖或补充。Rust 项目默认使用 `rust-analyzer`；没有可用 LSP server 时，符号和语法诊断会回退到 Tree-sitter Rust outline / syntax diagnostics，不阻塞普通 chat 和工具调用。配置或发现多个 language server 时，文件型工具按扩展名路由，`code_workspace_symbols` 会查询所有可用 server 并合并结果；`report_missing = true` 会在 TUI `LSP` 区显示已发现但未安装的 server
- `/config`：当前已支持在 TUI 内按 section 编辑 provider 常用项、permissions、memory、compaction，以及 MCP server 的 `name / command / args_csv / startup_timeout_secs`；配置页使用产品化字段标签、选中字段详情和统一快捷键提示，顶部 status strip 会做宽度自适应截断；宽终端会把配置内容限制在居中最大宽度，并把 Details 作为右侧说明栏展示，窄终端会把说明内联到选中字段附近，真实配置 key 会在 Details 中显示；TUI 新增的 MCP server 默认保存为 `required = true`、`startup = "eager"`、`trust.trust_class = "self_hosted"`；`Actions` 栏跟随配置面板负责保存和退出，MCP section 会额外提供 `activate` 来手动启动已保存的 lazy server，并在 lifecycle summary 中展示 `deferred` / `activating` / `ready` / `failed` 运行态，窄终端会切换到紧凑动作文案
- `model` / `fim_model` 默认优先走 picker；`api_key` 默认优先走 secret modal，并会在保存时以 plaintext 写回本地配置文件；`SIGIL_API_KEY` 始终可以在运行时覆盖配置值
- 弹窗内 `Enter` 只应用当前字段；`Ctrl-S` 会应用当前字段并保存，`F2` / `F3` 仍可作为可选快捷键
- config 页可以先 `Down` 到底部 actions，再 `Left/Right` 选中动作并 `Enter` 执行；如果有未保存改动，第一次 `Esc` 会把焦点拉到 `save` 并提示，第二次 `Esc` 才会丢弃草稿退出

另外，`workspace.root = "."` 有一个专门约定：

- 如果配置文件在当前仓库里，`.` 仍然表示该配置文件旁边的工作区
- 如果配置文件在用户配置目录里，`.` 会在启动时解析成你运行 `sigil-tui` 或 `sigil-cli` 时所在的目录

注意：permission 只负责 allow / ask / deny 策略判断，不等同于 shell sandbox。文件类内置工具会 canonicalize workspace root，并拒绝 `..`、绝对路径和指向 workspace 外的 symlink；`bash` 仍不提供更强进程隔离。

### MCP 工具

MCP server 通过 `mcp_servers` 配置接入，远端工具会用隔离后的 provider-visible 名称暴露给模型，例如 `mcp__filesystem__read_file`：

```toml
[[mcp_servers]]
name = "filesystem"
command = "node"
args = ["/absolute/path/to/server.js"]
startup_timeout_secs = 5
required = true
startup = "eager"

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = false

# Required when pin_version = true.
# [mcp_servers.trust.pinned]
# command_fingerprint = "sha256:..."
# protocol_version = "2025-06-18"
# server_name = "filesystem"
# server_version = "1.0.0"
```

当前 MCP 支持 stdio、`initialize`、`tools/list`、`tools/call`、provider-visible 名称清洗/截断/hash 去重，以及 `roots/list` 响应。client 只把已解析的 workspace root 暴露为 root；`notifications/progress` 会被安全忽略，不刷 timeline；TUI 运行时会声明并处理 `elicitation/create`，通过 modal 展示 server、请求字段和默认值，用户确认后只发送 modal 中确认过的 flat primitive object 字段，用户拒绝或取消时分别返回 `decline` / `cancel`，非 TUI 默认 runtime 仍返回明确 unsupported，不挂起也不伪造输入。MCP tool 的 permission subjects 会包含 `mcp_trust_class:<class>`，可被 permission rule 匹配；`approval_default` 会作为该 server 工具的默认审批模式参与逐调用决策，并且仍会被显式 tool/rule override 覆盖。`egress_logging = true` 时，MCP tools/call 会在审批通过后、执行前把 server、trust class、remote tool 和参数形状写入 append-only control state，不记录参数值或 secret 原文。`allow_secrets = false` 时，MCP tool 参数、`roots/list` payload 或 TUI elicitation response 中一旦包含已解析 secret 或 secret-like 字段会被阻断；允许发送 secret 的 MCP 结果仍会在回到本地 tool result 前脱敏。`pin_version = true` 时，启动会校验 `trust.pinned` 的 command fingerprint、protocol version、server name 和 server version；缺少 pinned identity 时会失败并输出 observed pin。MCP server 默认是 required + eager；`required = false` 的 eager server 启动或 `tools/list` 失败时会跳过并记录 warning；`startup = "lazy"` 的 server 启动时不会启动或注册工具，TUI `/config` 的 MCP section 可在 worker 空闲时用 `activate` 显式启动已保存的 lazy server，并在 lifecycle summary 中展示 `deferred`、`activating`、`ready` 或 `failed`；成功后才把真实 MCP tools 加入当前 agent registry，避免向模型暴露不可调用伪工具。

### Provider 环境变量 override

除了可以在配置文件里保存 `api_key`，当前还支持这些运行时 override：

- `SIGIL_MODEL`
- `SIGIL_API_KEY`
- `SIGIL_BASE_URL`
- `SIGIL_BETA_BASE_URL`
- `SIGIL_ANTHROPIC_BASE_URL`
- `SIGIL_FIM_MODEL`
- `SIGIL_USER_ID_STRATEGY`
- `SIGIL_REQUEST_TIMEOUT_SECS`
- `SIGIL_STRICT_TOOLS_MODE`

`SIGIL_API_KEY` 优先级最高；`DEEPSEEK_API_KEY` 作为 DeepSeek provider 的备用来源继续兼容读取。

主 TUI 配置页不会继续暴露 `anthropic_base_url`、`request_timeout_secs`、`strict_tools_mode`，以及 `beta_base_url` / `user_id_strategy` 这类 provider 专项低频项；它们保留给配置文件和环境变量层处理。

## 开发命令

### 质量门

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

这组质量门也已经固化在 GitHub Actions：

- [`.github/workflows/ci.yml`](.github/workflows/ci.yml)

### 常见开发命令

```bash
# 启动 TUI
cargo run -p sigil-tui

# 启动 CLI
cargo run -p sigil-cli -- run "hello"

# 只测某个 crate
cargo test -p sigil-tui
```

## 文档索引

- 架构方案：[`dev/docs/sigil-rust-agent-core-technical-solution.md`](dev/docs/sigil-rust-agent-core-technical-solution.md)
- 代码规范：[`dev/governance/code-standards.md`](dev/governance/code-standards.md)
- 工程规范：[`dev/governance/engineering-standards.md`](dev/governance/engineering-standards.md)
- 仓库内协作说明：[`AGENTS.md`](AGENTS.md)

## 当前工程原则

- `TUI-first`：优先打磨用户真正会看到的交互壳
- `kernel` 保持通用：不能让 DeepSeek 私有语义反向污染公共接口
- provider 特性下沉到 provider crate：内核只承载通用能力
- session 与 control state 走 append-only 模型
- 任何代码改动都要配套验证与必要文档同步
