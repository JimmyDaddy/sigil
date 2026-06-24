# Sigil Capability Roadmap

状态：实现路线图

更新日期：2026-06-14

本文基于当前仓库实现、`README.md`、`dev/governance/*` 和
[`sigil-rust-agent-core-technical-solution.md`](sigil-rust-agent-core-technical-solution.md)
整理。它不是对既有架构方案的替代，而是把“当前还缺什么、先做什么、做到什么程度算完成”拆成可执行路线。

## 1. 当前基线

当前 `sigil` 已经具备一个可用的 TUI-first coding agent 闭环：

- `sigil-kernel`：provider、tool、session、approval、permission、event、memory 和 compaction 的通用契约。
- `sigil-runtime`：TUI / CLI 共享 provider、内置工具、MCP tool registry 和 run options 装配。
- `sigil-provider-deepseek`：DeepSeek 主链路、reasoning replay、strict tools、prefix / FIM 专项入口、usage / cache token 统计。
- `sigil-provider-openai-compat`：OpenAI-compatible Chat Completions 主链路、tool call、usage、base URL 和 header 配置。
- `sigil-tools-builtin`：`read_file`、`write_file`、`edit_file`、`delete_file`、`ls`、`glob`、`grep`、`bash`。
- `sigil-code-intel`：可选 Code Intelligence / LSP 能力，包含常见语言 LSP 自动发现、Rust `rust-analyzer` client、Tree-sitter Rust fallback、符号/定义/引用/诊断/code action 查询工具、需要审批 diff 的 code action / rename 写工具和 TUI code tool card。
- `sigil-mcp`：stdio MCP server 启动、`tools/list` / `tools/call` 适配、read-only `resources/list` / `resources/read` 适配、read-only `prompts/list` / `prompts/get` 适配、`roots/list` 响应、elicitation、progress/listChanged runtime events、lazy activation 和 trust enforcement。
- `sigil`：无子命令时启动 TUI，TUI 提供 chat-first transcript、composer、slash selector、Quick Setup、`/config`、`/resume`、审批 modal、tool activity、diff preview、session 恢复、context compaction、markdown code block 高亮和鼠标辅助交互；`sigil --version` 输出版本与构建元数据。
- `sigil run`：公开自动化入口；`prefix` / `fim` 作为隐藏调试入口保留。
- `scripts/build-release-archive.sh`：构建本地 release archive，执行 built binary smoke，并输出 sha256 checksum。

当前产品已经能完成真实 coding task；后续重点不应是继续堆命令，而是补齐长期使用所需的代码智能、插件信任、多 provider、交互完整性和自动化接口。

## 2. Roadmap 原则

1. TUI 是第一用户表面。新增能力优先设计 TUI 交互和状态流，CLI 只做自动化入口。
2. kernel 保持 provider-neutral。DeepSeek、OpenAI、Anthropic、Gemini 等私有语义不得泄漏进公共 API。
3. session / control state 保持 append-only、可恢复、可审计。
4. MCP 与外部工具默认保守。信任、审批、secret egress 和 lazy activation 必须有真实 enforcement 后才可宣传为安全能力。
5. code intelligence 先做可验证的最小闭环，不一次性上重型 codegraph。
6. 每个阶段都要有可执行 gate；docs-only 阶段至少确认路径、链接和命令不漂移。

## 3. 优先级总览

| 优先级 | 能力 | 当前状态 | 推荐目标 |
| --- | --- | --- | --- |
| P0 | Code intelligence / LSP | Rust MVP、多语言自动发现、TUI trust/readiness/remediation、只读查询和 code action/rename 审批闭环已落地；默认关闭，支持 LSP + Tree-sitter fallback | 后续扩展更多语言细节和 capability UX |
| P0 | MCP 完整闭环 | tools 可用；read-only resources/prompts、trust enforcement、TUI 手动 lazy activation、模型按需 activation、lifecycle status、elicitation modal 与 elicitation decision audit、progress live panel、listChanged stale/refresh 已落地 | 后续如需要再补 sampling、completion、非 stdio transport |
| P0 | Secret 与安全产品化 | TUI 遮罩输入；配置仍支持明文 api_key，UI 与 doctor 均明确 plaintext 语义 | 评估 session-only 或可选安全存储 backend |
| P0 | Diagnostics / doctor | CLI `doctor` 与 TUI `/doctor` 已复用同一份 `DoctorReport` 检查 config、workspace、provider/auth、MCP、LSP 和 terminal 基线，并输出 remediation | 后续如需要再升级为 dedicated diagnostics panel |
| P1 | 多 provider | runtime 支持 `deepseek` 和 `openai_compat` | 后续扩 Anthropic / Gemini，并打磨 provider capability UX |
| P1 | 鼠标交互 | Phase 1-4 已落地：区域滚动、低风险点击、approval/setup/config/session、文本选择、OSC52、兼容性配置 | 后续只做真实终端 smoke 与明确产品需求驱动的小扩展 |
| P1 | Planner / executor / subagent | 单 agent loop | 分 session 双模型协作与任务型 subagent |
| P2 | PTY / background tasks | `bash` 是一次性命令 | 交互式 shell、后台任务、恢复与进程控制 |
| P2 | 更强编辑工具 | exact snippet replace | patch plan、多文件变更集、冲突检测、可回滚 |
| P2 | CLI / HTTP automation | CLI 公开 `run` | JSON 输出、headless 审批策略、HTTP streaming adapter |
| P3 | Packaging / distribution | npm scoped wrapper/package tarball 生成、Cargo git-tag 安装文档、`sigil --version` 构建元数据、本地 release archive、release CI matrix、GitHub provenance attestation、`sigil-ai.rb` Homebrew tap formula 和 release notes 生成已落地 | 后续补真实 tag 发布验证、npm registry 发布、tap 仓库同步和自更新策略 |
| P3 | Auto memory / indexed facts | 文档型 memory boot | 可审计自动记忆和 fact index |

## 4. Phase 0：安全与路线固化

目标：在大功能前先降低安全和路线漂移风险。

### 4.1 Secret resolution 与 redaction

当前事实：

- TUI secret modal 会遮罩 `api_key`。
- `sigil.toml` 被 `.gitignore` 忽略，并继续支持把 `api_key` 明文写入本地配置；这与 Codex / Claude Code / opencode 的本地配置体验一致。
- 环境变量继续优先于本地明文配置，便于 CI、临时 session 或用户不希望落盘的场景。
- `/config` 与 Quick Setup 的 key 输入明确提示保存结果是 plaintext；`doctor` 会把仅来自明文配置的认证报告为 warning，并给出迁移到 `SIGIL_API_KEY` 或保持本地私有的 remediation。
- MCP trust policy 有 `allow_secrets` 字段，需要逐调用 enforcement。

已完成：

1. 增加 runtime secret resolver，优先支持环境变量、本地明文配置与 session-only 值的兼容读取。
2. 把本地明文配置作为 P0 默认支持路径；Keychain 或 file-backed encrypted store 不作为 P0 默认路径，只作为后续可选 backend 评估。
3. `/config` 保存 api key 时明确标注本地配置为 plaintext，并保留环境变量覆盖路径。
4. 所有 TUI activity、session control、tool meta、error chain 默认 redaction secret-like 字段。
5. MCP `allow_secrets = false` 时，模型或 MCP server 不应收到已识别 secret。

后续可选：

1. 评估 “仅本进程使用” UI 入口，决定是否值得引入额外交互复杂度。
2. 评估 Keychain 或 file-backed encrypted store 作为 opt-in backend，不作为默认 P0 路径。

验收标准：

- 旧配置仍能加载。
- `SIGIL_API_KEY` 继续优先于配置文件里的 `api_key`。
- 本地 TOML 保存 api key 是受支持行为，但必须在文档和 UI 文案中明确它是 plaintext。
- `doctor` 会对 config plaintext auth 给 warning 和 remediation，但不打印 secret 值。
- session log、tool result、notice、debug output 不包含明文 secret。
- MCP server trust policy 能阻止 secret egress。

阶段实现后建议验证：

```bash
cargo test -p sigil-kernel config permission
cargo test -p sigil-tui config modal setup
cargo test -p sigil-mcp
cargo clippy --all-targets -- -D warnings
```

### 4.2 Roadmap 与 issue/task 切分

交付物：

1. 把本文拆成可执行 issue / task。
2. 每个 task 标注影响 crate、是否触碰 session / approval / provider / TUI。
3. 对 P0 能力建立 completion checklist。

验收标准：

- 后续工作能按 task 执行，不需要重新解释能力边界。
- README 和 architecture snapshot 不宣传尚未完成的能力。

### 4.3 Diagnostics / Doctor

当前事实：

- `sigil doctor` 已作为 CLI 本地诊断入口落地。
- TUI `/doctor` 会把同一份 `DoctorReport` 渲染到 transcript。
- 诊断逻辑位于 `sigil-runtime::doctor`，CLI 和 TUI 只负责渲染。
- 当前检查覆盖 config path/load、workspace root、session log dir、DeepSeek provider/auth 来源、MCP command/trust、code intelligence LSP plan 和 terminal `TERM`。
- 诊断只报告 secret 来源，不打印 secret 值。
- warning 和 error check 会携带同一份结构化 remediation，CLI 与 TUI renderer 都只负责展示。
- config plaintext auth 会被标为 warning，并提示优先使用 `SIGIL_API_KEY` 或确认本地配置不会被提交。

后续交付物：

1. 后续如需要更强交互，再把 `/doctor` report 扩展成 dedicated diagnostics panel。
2. 如后续出现 release binary，把 `doctor` 作为安装后第一排障命令写入 release 文档。

验收标准：

- CLI 与 TUI 使用同一份诊断事实，不各写一套判断。
- secret 来源可以定位，但 secret 值不会进入 stdout、stderr、timeline 或 session log。
- 缺配置、缺 API key、缺 MCP command、缺 LSP server 都能给出明确定位。

## 5. Phase 1：Code Intelligence / LSP

目标：从“会读文件和 grep”升级到“理解项目结构、诊断和符号”的 coding agent。

当前实现状态：

- 已新增 `crates/sigil-code-intel`。
- 已支持 `code_symbols`、`code_workspace_symbols`、`code_definition`、`code_references`、`code_diagnostics` 只读工具；`code_workspace_symbols` 会对已配置或自动发现的 language server 做 fan-out 并合并结果。
- 已支持 `code_actions` 只读查询，以及需要审批 diff preview 的 `code_action` / `code_rename` 写工具。
- `code_intelligence.discovery.enabled = true` 时会按 workspace marker / 文件扩展名自动发现 Rust、TypeScript/JavaScript、Python、Go，并只把 PATH 上可用的内置 allowlist server 纳入启动计划；手写 `code_intelligence.servers` 作为高级覆盖或补充。
- Rust 项目优先走 `rust-analyzer`；LSP 不可用时，符号与语法诊断回退到 Tree-sitter Rust。
- TUI info rail 的 `LSP` 区按 language/server 显示最近状态，包括 installed、missing、ready、degraded、fallback，并在 `code_diagnostics` 后投影错误/警告摘要和最近文件级 diagnostics 列表；code tool result 使用专门 renderer。
- TUI `/config` 已新增 `Code Intel` 区块，可调整 enabled/startup/discovery，展示只读 trust 边界，并复用 doctor 检查输出 LSP readiness/remediation。
- 默认配置仍为关闭，不影响普通 chat、内置工具和 MCP。

### 5.1 最小 code intelligence 服务

推荐新增边界：

- 新 crate：`crates/sigil-code-intel`
- 初始职责：workspace 扫描、语言检测、LSP client 生命周期、符号/诊断缓存、model-visible 摘要。
- 暂不放进 `sigil-kernel` 公共契约；等工具接口稳定后再决定是否上移。

交付物：

1. LSP server 配置模型：按语言声明 command、args、root markers、file extensions。
2. workspace root 解析复用现有 `resolve_workspace_root` 结果。
3. 支持启动 / 停止 / 健康状态查询。
4. 支持基础请求：
   - `textDocument/diagnostic` 或 server-specific diagnostic stream
   - `textDocument/definition`
   - `textDocument/references`
   - `textDocument/documentSymbol`
   - `workspace/symbol`
5. TUI info rail 显示 code intelligence 状态：off / starting / ready / degraded / error。

验收标准：

- 没有 LSP server 时不影响普通 chat 和 tools。
- LSP 启动失败显示可诊断状态，不 panic、不阻塞 TUI 启动。
- 对 Rust 项目至少能返回当前文件 symbols 和 diagnostics。

阶段实现后建议验证：

```bash
cargo test -p sigil-code-intel
cargo test -p sigil-tui provider_status view_model
cargo check
```

### 5.2 Model-visible code context tools

交付物：

1. 新增只读工具，名称可先采用：
   - `code_symbols`
   - `code_definition`
   - `code_references`
   - `code_diagnostics`
2. 工具结果必须有 `max_results` / `max_payload_bytes` 上限和 truncation metadata。
3. 工具输出使用结构化 JSON envelope，不写裸文本。
4. TUI tool card 提供专门 renderer，而不是直接 dump JSON。
5. 多 server workspace symbol 查询需要合并结果，并保留每个 server 的 installed/missing/ready/degraded/fallback 状态。

验收标准：

- 模型能查询符号和诊断，但不能通过 LSP 绕过 workspace confinement。
- 大型 reference 结果会截断并提示 total / returned。
- tool preview / activity 清楚显示文件、行号、符号名。

阶段实现后建议验证：

```bash
cargo test -p sigil-tools-builtin
cargo test -p sigil-tui ui::timeline
cargo test -p sigil-kernel permission agent
```

### 5.3 TUI code intelligence workflow

交付物（已落地）：

1. 在 activity / info rail 中展示当前诊断摘要。
2. 在 approval diff 中关联 affected diagnostics。
3. 支持用户触发“检查当前变更”的 TUI action，优先走现有快捷键/焦点模型，不急于新增 slash command。

验收标准：

- 用户不需要知道 LSP 协议也能看到“当前代码是否有诊断问题”。
- 状态展示不刷屏，不写入 durable transcript，除非它是明确的 tool result。

## 6. Phase 2：MCP 完整闭环

目标：让 MCP 从“能调用工具”升级到“可治理、可交互、可按需启动的插件系统”。

### 6.1 Trust policy enforcement

当前事实：

- `McpServerTrustPolicy` 已支持 `trust_class / approval_default / egress_logging / allow_secrets / pin_version`。
- `permission_subjects` 已包含 `mcp_trust_class:<class>`，permission rules 可以按 trust class 匹配 MCP 调用。
- `approval_default` 已作为 MCP server 工具的默认审批模式参与逐调用 permission decision。
- `egress_logging = true` 已在 MCP tools/call 审批通过后、执行前写入安全出境摘要到 append-only control state。
- `allow_secrets = false` 已阻断 MCP tool/resource/prompt args、`roots/list` payload 和 elicitation response 中的已解析 secret，并对 MCP tool/resource/prompt 结果做本地脱敏。
- `pin_version = true` 已校验 `trust.pinned` 中的 command fingerprint、protocol version、server name 和 server version；缺少 pinned identity 时会失败并输出 observed pin。
- resources 已通过 provider-visible read-only tools 暴露 `resources/list` / `resources/read`；prompts 已通过 provider-visible read-only tools 暴露 `prompts/list` / `prompts/get`。二者都不会自动注入 system prompt。

交付物：

1. MCP tool wrapper 在 `permission_subjects` 中带上 server trust class。（已落地）
2. `approval_default` 参与逐调用审批决策。（已落地）
3. `egress_logging = true` 时记录安全的出境摘要到 control state。（已落地）
4. `allow_secrets = false` 时对 tool args、roots、prompts/resources 做 secret egress gate。（已落地）
5. `pin_version = true` 时记录并校验 server identity / command fingerprint / protocol version。（已落地）

验收标准：

- 不同 trust class 的 MCP server 能产生不同默认审批行为。
- 所有 enforcement 都有测试，不只是配置 roundtrip。
- TUI tool card 能显示 MCP server 与 trust class。

阶段实现后建议验证：

```bash
cargo test -p sigil-mcp
cargo test -p sigil-kernel permission
cargo test -p sigil-tui timeline_flow_tests
```

### 6.2 Elicitation bridge

当前事实：

- TUI runtime 会声明 `elicitation` client capability；server 发 `elicitation/create` 时，TUI modal 会展示 server、请求 message、字段、默认值和 selected field 描述。
- modal 支持 MCP 规范中的 flat primitive object 输入：string、number、integer、boolean、enum；用户 `Enter` accept、`Ctrl-D` decline、`Esc` cancel。
- 非 TUI 默认 runtime 仍返回明确 unsupported，避免挂死或伪造输入。
- `allow_secrets = false` 时，TUI elicitation response 如果包含已解析 secret 或 secret-like 字段会被阻断。

交付物：

1. kernel 增加 provider-neutral 的 user elicitation event 或 tool-side interaction request。（已落地为 `ControlEntry::McpElicitation`，记录 server、请求摘要、字段名和 action，不记录用户输入值）
2. TUI 增加 modal，展示 MCP server、请求字段、默认值、风险提示。（已落地）
3. Headless CLI 默认返回明确错误；可选支持 `--elicitation-policy deny|json-file`。
4. 所有 TUI elicitation 决策写入 append-only control state。（已落地）

验收标准：

- MCP server 请求用户输入时，TUI 不挂起、不伪造输入。
- 用户拒绝时 server 收到 `decline`，取消时收到 `cancel`。
- 用户允许时只发送 modal 中确认过的字段。

### 6.3 Lazy activation

当前事实：

- `startup = "lazy"` 的 server 在普通启动阶段不启动、不注册工具，不拖慢 TUI/CLI registry 构建。
- crate 层已有显式 lazy activation API：activation 时启动 lazy server、执行 `tools/list`，成功后把真实工具加入 registry，失败按 required / optional 策略处理。
- TUI `/config` 的 MCP section 已在 footer actions 中提供 `activate`；worker 空闲时可把已保存的 lazy server 热接入当前 agent registry，运行中会拒绝并给出可诊断错误，lifecycle summary 会展示 `deferred` / `activating` / `ready` / `failed` 运行态。
- 模型可见 `mcp_activate_server` 工具已支持按需启动指定 lazy MCP server；成功后真实 MCP tools 会进入同一个 agent registry，下一轮模型请求即可看到并调用这些工具，TUI lifecycle summary 同步更新为 `ready`。
- 模型不会在 activation 前看到不可调用伪工具。

交付物：

1. 启动时记录 lazy server 配置状态，但不暴露伪工具给模型。（crate activation API 和 TUI lifecycle metadata 已落地）
2. 用户或模型需要某 MCP capability 时触发 activation。（TUI 手动触发与模型 `mcp_activate_server` 触发均已落地，运行态 info rail 会同步展示 lifecycle）
3. activation 成功后工具进入 registry；失败按 required / optional 策略处理。（已落地）
4. TUI 显示 MCP server lifecycle。（配置页 summary 和运行态 status 已落地）

验收标准：

- lazy server 不拖慢启动。
- activation 失败可诊断，不破坏当前 run。
- provider-visible tool list 不包含不可调用的伪工具。

## 7. Phase 3：Multi-provider Runtime

目标：把 `DeepSeek-first` 推进为真正的 provider-neutral 产品，而不是 `DeepSeek-only`。

### 7.1 OpenAI-compatible provider

交付物：

1. 新 crate：`crates/sigil-provider-openai-compat`。（已落地）
2. 支持 chat streaming、tool calls、usage、model config、base_url。（已落地）
3. runtime `build_provider` 支持 `deepseek` 和 `openai_compat`。（已落地）
4. `/model` 和 `/config` 支持 provider-aware model selection。（已落地）
5. README 明确 provider matrix。（已落地）

验收标准：

- DeepSeek provider 行为不回退。
- OpenAI-compatible provider 能跑完整 tool-call loop。
- session identity 仍记录 provider + model，不混会话。

阶段实现后建议验证：

```bash
cargo test -p sigil-provider-openai-compat
cargo test -p sigil-runtime
cargo test -p sigil-kernel agent session
cargo test -p sigil-tui config slash session
```

### 7.2 Anthropic / Gemini provider

交付物：

1. 先做 capability mapping，不急于追齐所有私有特性。
2. 明确哪些 provider 支持 reasoning、tool streaming、schema constrained tools、background task。
3. TUI 在 provider capability 不支持某能力时降级显示。

验收标准：

- 不支持的能力显示为 unavailable，而不是运行时才失败。
- provider 私有字段留在 provider crate，不进入 kernel 公共 API。

### 7.3 Provider capability UX

交付物：

1. Info rail 增加 provider capability 摘要。
2. `/config` 中隐藏当前 provider 不支持的字段。
3. 错误提示能区分 auth、quota、network、unsupported capability。

## 8. Phase 4：TUI 鼠标交互

目标：补齐现代 TUI 产品的鼠标辅助体验，同时不破坏键盘主路径和审批安全。

当前实现状态：

- 已抽出 layout snapshot 与 hit target，鼠标滚轮按区域作用于 transcript、approval、slash、info rail、setup/config/session 等表面。
- 已支持 composer 点击定位光标、slash candidate 点击选择、tool activity 聚焦、tool card header / hidden-preview 行展开/折叠和 hover visual state。
- Approval modal 已支持 file row、hunk、diff mode、metadata、allow/deny 明确 hit area；审批仍走现有 `AppAction::ApprovalDecision` 和 worker command 路径。
- Setup/config/session selector 支持鼠标选择与确认。
- Transcript 支持按显示列拖选、OSC52 复制、复制状态提示和 terminal capability 配置开关。
- `Terminal` 配置区支持 `mouse_capture`、`osc52_clipboard` 与 `scroll_sensitivity`。

已有设计：

- [`sigil-tui-mouse-interaction-design.md`](sigil-tui-mouse-interaction-design.md)

### 8.1 Area-aware scroll

交付物（已落地）：

1. 抽 `LayoutSnapshot`、`HitTarget`、`MouseInput`、`MouseInputKind`。
2. 滚轮根据区域命中作用于 timeline、approval diff、slash overlay、info rail、setup/config list。
3. 保留当前 fallback：approval 优先，否则 timeline。

验收标准：

- 鼠标滚轮不再只按全局状态处理。
- 未命中区域时行为与当前一致。

### 8.2 Low-risk clicks

交付物（已落地）：

1. 点击 composer 聚焦 composer。
2. 点击 slash candidate 只选中，不执行。
3. 点击 tool activity body 聚焦，点击 tool card header / hidden-preview 行展开/折叠。
4. 点击 info rail card 切换活动区域。

验收标准：

- 所有点击都有键盘等价路径。
- 普通文本点击不触发隐藏动作。

### 8.3 Approval modal mouse actions

交付物（已落地）：

1. 只有明确点击 `allow` / `deny` action 区域才触发审批结果。
2. 点击文件列表切换文件。
3. 点击 diff mode / metadata toggle 复用现有状态转换。
4. 滚动 diff / files 区域保持区域内语义。

验收标准：

- 鼠标审批不能绕过现有 `AppAction::ApprovalDecision` 和 worker command 路径。
- approval 相关测试覆盖 allow / deny / file / hunk / diff mode。

阶段实现后建议验证：

```bash
cargo test -p sigil-tui mouse_
cargo test -p sigil-tui approval_flow_tests
cargo test -p sigil-tui shell
```

## 9. Phase 5：Planner / Executor / Subagent

目标：让 Sigil 支持更长、更复杂的任务，同时保持 prefix cache 和 session audit 清晰。

### 9.1 Planner / executor 双 session

交付物：

1. planner session 不带工具，只生成 plan。
2. executor session 接收 handoff，并执行真实 tool-call loop。
3. 两个 session 的 log、identity、prefix snapshot 分开。
4. TUI 展示 plan，但不把 planner 中间推理污染 executor 上下文。

验收标准：

- planner 失败不会破坏 executor 当前 session。
- executor 可以独立 resume。
- cache-stable prefix 不因 planner 文本反复变化。

### 9.2 Task tool / subagent

交付物：

1. 新增 provider-neutral task/subagent control model。
2. 子任务有独立 session log、状态、取消路径。
3. TUI activity 显示子任务进度和结果摘要。
4. 权限继承必须保守；子任务不能自动扩大 workspace 或 external directory 权限。

验收标准：

- 子任务 started / completed / failed / cancelled 全部进入 control state。
- 重启后能恢复子任务最终状态，不能误以为还在运行。

### 9.3 Reviewer / compactor / summarizer roles

交付物：

1. 明确后台角色模型选择。
2. 并发预算先作为 provider/runtime 内部策略，不提前变成 kernel public type。
3. TUI 显示后台角色，但不刷 durable transcript。

## 10. Phase 6：Shell / PTY / Background Execution

目标：把一次性 `bash` 工具扩展成可控、可审计、可恢复的终端执行能力。

### 10.1 PTY shell

交付物：

1. 增加可选 PTY backend。
2. 支持交互式命令、持续输出、stdin 注入、窗口大小变化。
3. 进程输出有 bounded preview 和完整 log 路径。
4. TUI 显示进程 card：running / exited / failed / cancelled。

验收标准：

- PTY 不绕过 permission / approval。
- 长输出不会灌满 provider context。
- 取消能杀进程组或明确标记无法终止。

### 10.2 Background task handles

交付物：

1. `BackgroundTaskHandle` 从预留类型变成可用控制面。
2. 支持 submit、poll、resume、cancel。
3. session log 记录 handle 和最终状态。

验收标准：

- 进程重启后不会丢失后台任务状态。
- provider request 不假设每轮都同步流式结束。

## 11. Phase 7：Editing Tools 2.0

目标：提升多文件编辑可靠性，减少 exact replace 的脆弱性。

交付物：

1. 新增 patch plan 类型：文件列表、操作、预期 old hash、diff preview。
2. 支持 apply unified patch 或结构化 edits。
3. 写前冲突检测：文件 hash / mtime / snippet mismatch。
4. 可选 rollback metadata：至少能展示本轮写入前状态摘要。
5. TUI approval card 展示多文件变更集、风险、格式化建议。

验收标准：

- 多文件 patch 不因一个文件失败导致不可解释的半成品。
- 写入结果和 approval preview 能对应。
- 大 diff 仍可导航、截断和恢复。

阶段实现后建议验证：

```bash
cargo test -p sigil-tools-builtin edit write
cargo test -p sigil-kernel approval session
cargo test -p sigil-tui approval timeline
```

## 12. Phase 8：CLI / HTTP / Automation

目标：保留 TUI-first 产品心智，同时给自动化场景稳定接口。

### 12.1 CLI JSON mode

交付物：

1. `sigil run --json` 输出 newline-delimited events。
2. 支持 headless approval policy：deny / allow-readonly / require-preview-file。
3. 支持指定 session log、resume session、model、effort。
4. stdout / stderr 分离稳定，便于脚本消费。

验收标准：

- JSON schema 有测试。
- 非 JSON human output 不受影响。
- headless ask 不静默执行写操作。

### 12.2 HTTP streaming adapter

交付物：

1. 新入口 crate 或 binary，复用 runtime / kernel。
2. SSE 或 newline JSON event stream。
3. 明确 auth、workspace root、permission policy。
4. 不把 HTTP transport 反向塞进 kernel。

验收标准：

- TUI、CLI、HTTP 消费同一套 `RunEvent` 语义。
- HTTP 取消能映射到 runner cancel。

## 13. Phase 9：Packaging / Doctor / Release

目标：让 Sigil 可以被稳定安装、诊断和升级。

当前实现状态：

1. release binary archive 脚本已落地：`scripts/build-release-archive.sh`。
2. Homebrew tap formula 生成已落地：`scripts/render-homebrew-formula.sh` 输出 `sigil-ai.rb`。
3. npm scoped wrapper 与 platform package tarball 生成已落地：`scripts/prepare-npm-packages.sh`。
4. `cargo install` 文档（已落地为 `docs/en/installation.md` 和 `docs/zh-CN/installation.md`，首发使用 Git tag 安装并保持 `sigil` 一个 binary 入口）。
5. `sigil --version` 输出 package version、git commit、target 和 profile。
6. `sigil doctor` 与 TUI `/doctor` 已落地：
   - config path
   - provider auth
   - workspace root
   - MCP server status
   - LSP availability
   - terminal capability
7. release archive 脚本会对 built binary 运行 `--version` 与 `doctor` smoke，并输出 `.sha256`。

后续交付物：

1. 首个真实 tag release 验证。
2. npm registry 发布验证。
3. 独立 Homebrew tap 仓库同步。
4. crates.io package name 决策。
5. 自更新策略评估。

验收标准：

- 新用户不需要从源码仓库理解所有配置。
- doctor 输出不泄漏 secret。

## 14. Phase 10：Memory 2.0

目标：从文档型 memory boot 扩展到可审计的长期记忆。

交付物：

1. Indexed fact store。
2. 明确的 memory write / update / delete flow。
3. TUI memory review card。
4. memory 变更进入 append-only audit。
5. prefix 稳定性策略：新 memory 不随意改写当前 session prefix。

验收标准：

- 用户能知道模型记住了什么。
- 用户能撤销或编辑 memory。
- Memory 不破坏 cache-first 设计。

## 15. 推荐执行顺序

推荐按下面顺序推进，而不是按实现趣味挑选：

1. **Secret 可选 backend 评估**：只在确有价值时推进 session-only UI、Keychain 或 encrypted file backend。
2. **Provider capability UX**：为 DeepSeek / OpenAI-compatible 能力差异补更清晰的 TUI 降级说明。
3. **Planner / executor**：在基础执行闭环稳定后支持复杂任务。
4. **PTY / background tasks**：解决长命令和交互式命令。
5. **Editing Tools 2.0**：提高多文件变更可靠性。
6. **CLI JSON / HTTP adapter**：扩展自动化表面。
7. **MCP sampling / completion 评估**：只在有清晰产品入口时推进，不要把 server-initiated LLM 调用隐式接入默认路径。
8. **Memory 2.0**：产品化长期记忆。

## 16. 每阶段通用完成定义

任一阶段宣称完成前，至少满足：

1. README、相关 `dev/docs` 和真实实现一致。
2. 涉及 TUI 的变更同步 keyboard help / info rail / command metadata。
3. 涉及 session / approval / provider 的变更有 append-only 恢复测试。
4. 新增配置有默认值、旧配置兼容和文档说明。
5. 新增工具有 preview、permission subjects、bounded output 和 structured error。
6. 相关 gate 通过；docs-only 至少通过路径和链接审计。

默认 Rust gate：

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
```

docs-only gate：

```bash
git diff --check
```

## 17. 暂不建议做的事

1. 不建议马上做桌面壳。当前 TUI 的交互和安全边界还在演进。
2. 不建议把 prefix / FIM hidden command 产品化。它们应进入 TUI 内部动作或编辑能力。
3. 不建议在没有 trust enforcement 前宣传 MCP plugin security。
4. 不建议先做大而全 codegraph。先用 LSP 和 bounded symbol context 建立可验证闭环。
5. 不建议为了多 provider 把 DeepSeek 私有 reasoning replay 抬进 kernel。
6. 不建议新增大量 slash command 来解释功能；优先用 TUI focus、modal、activity 和 info rail。
