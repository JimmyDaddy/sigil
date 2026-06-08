# Termquill

`termquill` 是一个 **TUI-first** 的 Rust AI coding agent。它的目标不是做一组零散命令，而是提供一个可复用的 agent 内核，并优先把真正面向用户的终端交互壳做好。

当前仓库已经具备这些核心能力：

- 通用 `kernel`：统一承载 provider、tool、session、approval、event 契约
- `DeepSeek-first` provider：优先支持 DeepSeek 流式对话、工具调用、reasoning replay 与 Beta 扩展点
- 内置工具注册表：文件读写、编辑、搜索、shell 执行
- stdio MCP 工具接入：远程工具通过统一 `ToolRegistry` 暴露给 agent
- 权限策略：read-only 默认放行，写工具默认审批，headless `ask` 自动放行
- 文档 memory boot：工作区根下 `TERMQUILL.md` / `AGENTS.md` / `CLAUDE.md` / `TERMQUILL.local.md` 与 `@path` 导入
- context compaction：soft threshold 提示、hard threshold 在 idle 边界自动压缩，append-only 审计日志不改写
- TUI 主入口：消息区、事件流、审批流、session 恢复、diff 预览、`/compact`、`/model`、`/effort`、Quick Setup、`/config`

## 仓库结构

```text
termquill/
  crates/
    termquill-kernel/              # 通用 agent 内核与领域契约
    termquill-provider-deepseek/   # DeepSeek provider 实现
    termquill-tools-builtin/       # 内置工具
    termquill-mcp/                 # stdio MCP client 与工具适配
    termquill-cli/                 # 薄 CLI 启动器与调试入口
    termquill-tui/                 # 第一用户入口
  dev/governance/                # 开发约束、代码规范、工程规范
  dev/docs/                      # 架构与技术方案
  termquill.toml                   # 本地配置示例
```

## 当前入口

### 1. TUI

推荐优先使用 TUI：

```bash
cargo run -p termquill-tui
```

TUI 当前支持：

- 提交 prompt 并流式查看输出
- prompt 提交后 composer 会清空并保持可见；主聊天区改为 app-owned transcript，可用 `Up/Down`、`PageUp/PageDown`、`Ctrl-U/D`、`Ctrl-Home/End` 和滚轮持续回溯到会话最顶，同时最近一段历史仍会同步进终端原生 scrollback
- 输入 `/` 时弹出 slash command selector，支持 `Up/Down` 选中、`Tab` 接受、`Enter` 执行；`/model`、`/effort`、`/tool`、`/tools` 会继续下钻参数候选，当前内置 `/compact`、`/config`、`/model`、`/effort`、`/resume`、`/sessions`、`/tool`、`/tools`
- composer 的历史输入只响应键盘：仅在 composer 聚焦且光标位于首行或末行时，`Up/Down` 才切换历史
- `/config` 打开 TUI guided config flow；provider 主流程只保留 `model / api_key / base_url / fim_model`，文本项统一走弹窗输入，底部固定 `Actions` 栏可用 `Down` 聚焦，再用 `Left/Right` 选 `save / save+close / close`
- 主屏默认走 chat-first：inline viewport 会占满当前终端可视区，左侧主区域展示 live transcript + 底部 composer，右侧保留独立的 full-height `info rail`；启动恢复旧会话时只 seed 最近一段 transcript 到 terminal scrollback，避免长会话整屏重放
- 不再要求 `Tab` 在主屏各卡片之间切焦点；`Shift-Tab` 直接轮换当前会话的 `allow / ask / deny`
- composer 顶部直接展示当前模型与运行阶段；运行中会在 composer 内显示紧凑的 `thinking / tool / streaming` live 状态
- 右侧 `Info rail` 独立占据整列，展示 `Session / Permissions / Agents / Usage / Controls` 五组状态，而不是挤进 composer 旁边的一个小角落
- `ctx`、compaction status 和 auto-compaction 统一按同一个 effective context window 计算：已知模型窗口优先，其次才回退到 `compaction.context_window_tokens`
- assistant / tool 输出继续走线性展开：assistant markdown 按段落展开，tool result 改成卡片式展示；`read_file / ls / glob / grep / bash / write_file / edit_file` 走专用 renderer，其他结构化 payload 走树形 fallback，不再直接 dump 原始 JSON
- live phase 只保留在运行态和事件流里，不再固化成 chat transcript；completed thinking 默认折叠成一行摘要，用 `Ctrl-T` 展开或收起
- `/tool <latest|next|prev|open|close|toggle>` 选择并展开单个 tool card；`/tools <brief|full>` 继续控制全局 brief/full 视图
- 工具调用审批改为居中 review card：固定 `Summary / Files / Diff / Actions` 四区，composer 不会因为审批而消失
- `write_file` / `edit_file` diff 预览支持按文件切换、按 hunk 跳转和 diff mode 切换
- `/compact` 手动压缩当前会话的 provider 可见上下文
- `/model <flash|pro|id>` 切换运行时模型，并开启一个 fresh session，避免把旧 session identity 和新模型混在一起
- `/effort <low|medium|high|max>` 切换下一轮 agent run 的 reasoning effort
- soft / hard context threshold 提示与 idle 边界自动 compaction
- `Ctrl-C` 取消运行后从 durable JSONL log 恢复当前 session 视图
- TUI 重启时会默认恢复最近一次 durable session，而不是静默丢到一个全新的空白会话
- 首次启动或配置损坏时，直接进入极简 Quick Setup，只确认“信任当前目录 / 模型 / 认证”；模型走候选选择，API key 走隐藏输入，当前启动目录自动成为 workspace
- Quick Setup 保存后会写入 `workspace.root = "."`；当配置位于用户配置目录时，`.` 会在运行时解析成当前启动目录，而不是配置文件所在目录
- 长输出默认在主 transcript 内滚动查看；审批浮层继续保留键盘滚动

### 2. CLI

CLI 目前主要是自动化和调试入口：

```bash
cargo run -p termquill-cli -- run "总结一下当前仓库"
```

默认只暴露 `run`。`prefix` / `fim` 等 provider 专项入口保留在实现层，但不作为普通用户主心智。

## 快速开始

### 1. 准备配置

仓库根目录已经有一个示例配置文件：

`/Users/jimmydaddy/study/turbods/termquill.toml`

至少需要准备：

- `TERMQUILL_API_KEY`
- 如果你想手写配置，再准备 `termquill.toml` 中的 provider / workspace / session / permission / memory / compaction 配置

TUI / CLI 当前按这个顺序找配置：

1. `--config <path>`
2. 当前工作目录下的 `./termquill.toml`
3. 标准用户配置目录里的 `termquill.toml`

标准用户配置路径：

- macOS：`~/Library/Application Support/termquill/termquill.toml`
- Linux：`$XDG_CONFIG_HOME/termquill/termquill.toml` 或 `~/.config/termquill/termquill.toml`
- Windows：`%APPDATA%\\termquill\\termquill.toml`

如果 TUI 启动时没有可用配置，或配置加载失败，它不会直接退出，而是进入 Quick Setup，把配置写回上述目标路径。首配只要求授权当前目录、从候选里选择模型并填好认证；认证可以直接录入 `api_key` 并持久化到配置文件，也可以预先导出 `TERMQUILL_API_KEY` 在运行时覆盖。如果要用自定义模型 id，可以先 `Esc` 退出选择器后手动输入。后续 `/config` 继续只暴露高频项；更细的 provider 兼容字段改走配置文件或环境变量。

### 2. 启动方式

```bash
# 推荐
cargo run -p termquill-tui

# 自动化 / 脚本场景
cargo run -p termquill-cli -- run "read README and summarize the repo"
```

如果想把配置写到自定义位置：

```bash
cargo run -p termquill-tui -- --config /absolute/path/to/termquill.toml
```

### 3. Session 持久化

默认 session log 落在：

```text
.termquill/sessions/
```

当前实现采用 append-only JSONL，便于审计、恢复与 provider continuation state 持久化。

当前恢复语义还包括：

- session identity 会跟随 durable log 恢复，而不是盲目回退到当前配置里的 provider/model
- `/config` 保存后的默认 provider/model 不会静默改写当前 session identity；当前会话仍以 durable log 中的身份为准，新默认值会用于后续新 session 或空白 session
- 运行中取消后，TUI 会从 durable JSONL log 重建会话视图，避免把临时内存态当成恢复真相
- 每轮 usage 会追加持久化 control 记录，session resume 后可恢复 cache hit、累计 usage 和最近一次 prompt pressure
- compaction 只追加 `CompactionApplied` control 记录，不改写旧历史；后续请求使用稳定 summary + tail 投影
- hard threshold 自动 compaction 只在 run 回到 idle 后触发，不会抢占当前流式执行

## 配置要点

新增的默认配置块如下：

```toml
[permission]
write_mode = "ask"

[memory]
enabled = true

[compaction]
enabled = true
soft_threshold_ratio = 0.5
hard_threshold_ratio = 0.8
context_window_tokens = 128000
tail_messages = 6
```

当前语义：

- `permission.write_mode = "ask"`：TUI 写工具弹审批；CLI/headless run 遇到 `ask` 自动放行
- `memory.enabled = true`：启动时稳定装载工作区根 memory 文档，并支持单独一行 `@path` 导入
- `compaction.*`：控制 `/compact` 手动压缩、soft threshold 提示和 hard threshold 的 idle 自动 compaction；若当前模型已有已知 context window，会优先按模型窗口计算阈值，否则回退到 `context_window_tokens`
- `/config`：当前已支持在 TUI 内按 step 编辑 provider 常用项、permissions、memory、compaction，以及 MCP server 的 `name / command / args_csv / startup_timeout_secs`；底部固定 `Actions` 栏负责保存和退出
- `model` / `fim_model` 默认优先走 picker；`api_key` 默认优先走 secret modal，并会在保存时写回配置文件
- 弹窗内 `Enter` 只应用当前字段；`Ctrl-S` 会应用当前字段并保存，`F2` / `F3` 仍可作为可选快捷键
- config 页可以先 `Down` 到底部 actions，再 `Left/Right` 选中动作并 `Enter` 执行；如果有未保存改动，第一次 `Esc` 会把焦点拉到 `save` 并提示，第二次 `Esc` 才会丢弃草稿退出

另外，`workspace.root = "."` 有一个专门约定：

- 如果配置文件在当前仓库里，`.` 仍然表示该配置文件旁边的工作区
- 如果配置文件在用户配置目录里，`.` 会在启动时解析成你运行 `termquill-tui` 或 `termquill-cli` 时所在的目录

注意：permission 只负责 allow / ask / deny 策略判断，不等同于 shell sandbox。当前仍只保持 workspace 路径约束，不提供更强进程隔离。

### Provider 环境变量 override

除了可以在配置文件里保存 `api_key`，当前还支持这些运行时 override：

- `TERMQUILL_MODEL`
- `TERMQUILL_BASE_URL`
- `TERMQUILL_BETA_BASE_URL`
- `TERMQUILL_ANTHROPIC_BASE_URL`
- `TERMQUILL_FIM_MODEL`
- `TERMQUILL_USER_ID_STRATEGY`
- `TERMQUILL_REQUEST_TIMEOUT_SECS`
- `TERMQUILL_STRICT_TOOLS_MODE`

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
cargo run -p termquill-tui

# 启动 CLI
cargo run -p termquill-cli -- run "hello"

# 只测某个 crate
cargo test -p termquill-tui
```

## 文档索引

- 架构方案：[`dev/docs/termquill-rust-agent-core-technical-solution.md`](dev/docs/termquill-rust-agent-core-technical-solution.md)
- 代码规范：[`dev/governance/code-standards.md`](dev/governance/code-standards.md)
- 工程规范：[`dev/governance/engineering-standards.md`](dev/governance/engineering-standards.md)
- 仓库内协作说明：[`AGENTS.md`](AGENTS.md)

## 当前工程原则

- `TUI-first`：优先打磨用户真正会看到的交互壳
- `kernel` 保持通用：不能让 DeepSeek 私有语义反向污染公共接口
- provider 特性下沉到 provider crate：内核只承载通用能力
- session 与 control state 走 append-only 模型
- 任何代码改动都要配套验证与必要文档同步
