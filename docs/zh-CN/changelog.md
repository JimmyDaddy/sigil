<!-- public-doc-role: changelog; authority: user-visible-release-history; sections: unreleased-main,v0-0-1-beta-4-2026-08-11,v0-0-1-beta-3-2026-08-06,v0-0-1-beta-2-2026-08-03,v0-0-1-beta-1-2026-08-02,v0-0-1-alpha-6-2026-07-30,v0-0-1-alpha-5-2026-07-18,v0-0-1-alpha-4-2026-07-16,v0-0-1-alpha-3-2026-07-15,v0-0-1-alpha-2-2026-07-15,v0-0-1-alpha-1-2026-07-08,v0-0-1-alpha-2026-07-07; cta: open-installation -->

# 用户变更记录

[文档首页](README.md) · [安装](installation.md) · [当前支持状态](status.md) · [English](../en/changelog.md)

这一页只记录用户能够直接感知的变化。当前支持边界和早期预览说明见[当前支持状态与后续计划](status.md)。

## 尚未发布 - main

- provider 的临时断线、超时、限流和服务端错误现在会在同一个 durable generation 内恢复：只有尚未提交
  模型输出或外部 effect 时才会有界重试，并显示重连/等待状态。重启后也只有原 child session 保存了精确的
  schedule/request proof 才会继续；partial output、未知 tool、hosted 或 workspace effect 会安全地停在
  待处理状态，不会静默重放，也不会直接让整条 Task 失败。
- Plan review 现在以可读 Plan 本身作为可审阅事实；可选 precompile 只是 advisory，不能拒绝一个
  有效 Plan。`Run` 会原子批准精确 Plan、创建唯一稳定的 Task及 host 生成的单一线性执行单元，并立即
  启动 runner；不再要求第二次模型调用生成 Task DAG或结构化 contract。typed提交失败时会把模型的有界
  完整文本保留为可审阅 Plan；普通 Task的可选 planner如果
  无法返回合法 plan，也会降级为直接线性执行。真实的 provider、权限、工具、验证和effect故障仍保持
  类型化、可恢复。
- Plan review 现在会打开完整、可滚动的工作台，不再把主要内容截断在紧凑状态卡片中。从 32x8 到
  宽屏终端都可以访问全部步骤；`Esc` 只关闭、不拒绝，输入可打印字符会返回 composer，所有动作继续
  绑定精确的 durable plan。
- Revise 现在会先询问“希望修改什么”，新方案成功前原计划始终保持 active；任何 terminal revision
  failure 都会恢复原计划。受影响的旧 session 通过严格只读兼容投影恢复，`sigil doctor` 会报告无法
  证明 lineage 的旧数据。
- 智能体现在可以通过 durable attention queue 提出 bounded typed 问题。待回答问题在退出和恢复 TUI
  后仍然存在且没有超时；后台智能体的问题会路由到 root session；一个已接受回答只会继续一次
  provider attempt，不会重放提出问题的 turn。MCP elicitation 复用同一表单 renderer，但不会继承
  durable replay 语义。

## v0.0.1-beta.4 - 2026-08-11

本次 beta 稳定了长时间运行的 TUI session，让 shell 执行在 transcript 中保持可审计，并收口
durable Task 续接、记忆路由和 final answer 协调链路。

- 修复 final answer 后 TUI transcript 错乱，并恢复终端 resize/reflow 后查看更早历史的能力。
  历史浏览会在新输出到达时保持稳定内容锚点；同一 session 的重复 artifact 维护失败只提示一次。
- busy turn 中提交的 follow-up 会等待安全交付点，不再中断当前 run；plan、queue、verification、
  attachment 和 agent 的紧凑控制在窄屏、短屏下仍保持对齐、可见并对应真实动作。
- Bash 卡片现在会在实时执行、reload 和 child-agent transcript 中显示经过安全投影的命令；
  running 与 terminal 状态不再合并成重复或误导性的卡片。受限的只读 Git metadata 检查可以被
  正确识别，同时不会放宽 `.git` mutation 的 protected 规则或 `danger-full-access` 的 hard deny。
- 继续 paused Task 时会选择精确的 durable Task，把用户本次续接指导带入经审阅的替代计划，并且
  TUI 只展示当前 Task，不再重新显示已经过时的初始计划；显式开始 plan review 也会把旧 Task
  从当前工作区移入历史，而不是继续与 plan preview 并列展示。
- 自动路由回合现在可以同时处理用户明确提出的持久记忆意图。审批通过的记忆写入会先 durable
  落盘，再执行所选 Chat / PlanReview / Task handoff；工具结果仍保持 provider 声明顺序。
- 自动路由的内部 reasoning 不再于正式工作回合之前显示成一段重复 Thinking。Final answer 协调
  现在只使用当前 logical run 的子代理 facts，facts 收敛后移除旧快照，并对连续 blocked-final
  尝试设置独立上限；被阻塞的 child 或 Task participant 不再被误报为 completed，最终被 blocker
  拒绝的 final candidate 也不会继续留在 TUI 中伪装成已接受回答。
- 完整的 0-byte 工具 artifact 现在是合法且可读取的 artifact，避免健康 session 的后台维护被
  反复延迟并持续刷提示。

## v0.0.1-beta.3 - 2026-08-06

本次 beta 发布自动计划审阅与 AI 编排生命周期、带配额/TTL 的 session-scoped scratch，以及
RFC-0062 工具结果结算的收口。


- 自动 Task 路由现在是默认行为：普通输入先在 review-first 基线上运行一次由 host 拥有的
  Chat / PlanReview / Task 决策，只有被接受的计划才能创建 durable Task。直接任务执行需要
  exact-route qualification manifest；显式 `routing_policy = "manual"` 是 coarse rollback。
  `sigil doctor` 会报告三项自动编排事实。
- Plan review 现在是完整一等 lifecycle：`/plan` 与自动 review 共享同一 typed plan draft、
  同一 pending plan card（Run / Save / Revise / Reject）与同一 HTTP/Desktop/TUI decision
  command；reconnect 与 reload 会恢复 pending plan。
- Durable Task 执行现在会把每步的目标路径、能力、交付物、验收条件和检查引用以版本化契约贯穿
  planning、compaction、中断与 resume；child/provider 启动前按 exact scoped tools 做能力准入，
  participant 在相同语义前沿重复分析时会由 durable no-progress guard 切入有界结项。
- 新增固定只读 `vcs_inspect`，以有界操作提供 status/diff 事实而不接受任意 Git 参数；semantic cache
  layout V2 同时从 provider-visible tool schema identity 中排除了逐轮授权身份。
- 工作区指令记忆与持久记忆现在默认启用；普通权限模式下持久变更仍会询问，`danger-full-access`
  不弹审批；可分别设置 `[memory].enabled = false` 或 `[memory].writable = false` 退出对应能力。
- 直接运行 `sigil` 现在会创建 fresh session，不再自动打开最近会话。显式恢复会在安全 endpoint
  修正后保留可移植对话；目的地变化或无法证明时先要求确认，并阻止同一 session 被两个
  TUI/Desktop/headless owner 同时以可写方式打开。
- TUI 与 Desktop 现在都能按 connection/model 单独配置可选的上下文大小，首次设置也有入口。留空会继续使用 Provider 元数据和全局 fallback，因此没有模型目录的站点不会被阻塞。
- TUI 会话文本现在会在松开鼠标时自动复制。复制会同时尝试 OSC52 与系统剪贴板；失败时保留
  选区供 `Ctrl-C` 重试，并继续排除右侧信息栏。
- 长任务不再因终端原生 scrollback 与异步输入争抢 cursor query 而直接退出。运行中 facts 不再
  被表达成 host 终结指令；初始工具 preview 预算耗尽后，typed artifact retrieval 仍然可见。
- 交互式工具审批不再于 300 秒后自动过期。**本次会话允许** 会在重新打开同一 session 后继续
  生效，并允许可识别验证命令跨 `tail`、`head`、`grep` 等纯展示变化复用，同时保留逐次精确执行校验。


## v0.0.1-beta.2 - 2026-08-03

本 beta 将模型目录发现移出首次配置关键路径，并允许冻结后的 TUI 包先于对应 Desktop 资产发布。

- 快速设置、`/config`、Desktop 设置和 HTTP 配置接口现在都把远端模型列表视为可选增强。
  刷新期间 bundled 模型仍可选择；Provider 没有 `/models` 接口或 discovery 失败时，也始终可以
  输入精确模型 ID。凭据、协议、端点和模型是否真正兼容，仍由第一次真实生成请求判定。
- 不再为未知模型或不支持 reasoning 的模型猜测并发送 reasoning 参数；请求只使用精确
  provider/model capability 明确支持的 effort。
- Alpha/Beta 的 TUI npm 包现在可从已冻结 release candidate 独立发布，同时保持 GitHub Release
  为 draft。Desktop DMG、更新包、Pages 更新、Homebrew 和公开 GitHub Release 后续继续从同一
  不可变 tag 完成。
- macOS Desktop 公证改为异步且可恢复：不可变 DMG/app submission 记录在 append-only 账本，
  状态命令只做单次查询，offline finalizer 会在上传 Desktop 资产前复验所有 Accepted submission。

## v0.0.1-beta.1 - 2026-08-02

本 beta 正式发布首个已签名的 macOS Desktop 安装渠道，以及配套的跨平台 TUI beta 渠道。

- Shell 权限现在通过同一份不可变结构化计划贯穿策略、审批、审计和执行。复合验证命令会逐个
  分析子命令；危险参数、重定向、动态语法、受保护目标和缺失的沙箱能力继续 fail closed。
  精确绑定隔离能力的会话授权可以减少重复审批，但不会扩大到无关命令。原生 PowerShell
  后台任务不会进入一次性 Shell 路径；无法解析的 shell 路径会保守降级，而不是中断计划。
- Desktop 与 TUI 的审批决定现在会在精确 command receipt 到达时立即收敛，不再只等待稍后的
  实时事件。已接受、正在解析、开始执行、过期、stale、状态待确认和终态相互区分，旧 receipt
  不能覆盖新状态。
- 有限命令只使用前台 Shell 路径；常驻和交互任务使用带 readiness 与事件驱动 wait 的明确
  terminal task。runtime 不再周期性查询 terminal 状态，模型也无需轮询没有变化的日志。
- Release tag 现在只构建一次 TUI/npm 字节，并在 draft 中冻结绑定 commit 的 candidate
  manifest；最终发布复用已准入 tarball，不再重新编译。Release doctor 与 Desktop 上传命令
  会绑定版本、tag/main/CI、updater 公钥、签名、公证和 macOS 双架构；公开后自动触发带有
  有界一致性等待的 npm/GitHub/Desktop/Pages/Homebrew 真实安装 smoke。
- 修复无效或非当前格式配置导致所有 Desktop 工作区都无法打开、设置页也无法进入的死路。
  Desktop 现在会启动支持配置恢复的工作区服务，并在设置中提供明确的当前格式替换流程；
  TUI 快速设置也提供相同恢复路径。两端都不会复用或迁移原无效文件中的值，并会拒绝覆盖
  已被其他进程修复的有效配置。
- 公开 macOS Desktop beta 渠道：提供已签名并完成 Apple 公证的 Apple 芯片与 Intel DMG，以及分架构签名更新包。
- 为 Desktop 设置页、TUI `/update` 与 CLI `sigil update` 增加明确的版本检查和更新；包管理器安装会收到对应更新命令，独立安装包继续校验 checksum/签名，并且不会静默重启。
- 更新官网、README、安装/状态文档与界面导览，把 Desktop 与 TUI 作为并列入口，同时提供分架构下载指引、真实 Desktop 截图导览和已有 TUI 真实运行 Demo。
- `/compact` 现在把命令本身视为明确意图：单次执行即可生成、校验并原子激活已准入的可恢复 checkpoint，不再打开确认弹窗。失败时保持当前上下文不变，并持续显示精确原因。
- 修复语义压缩自己的 provider-attempt 与 usage 审计记录推进 durable stream 后，被错误判定为 stale 并丢弃结果的问题。
- 修复 Desktop 在任务执行中按 Enter 无法可靠加入 durable 后续队列、运行控制偶发失联，以及 live/durable 消息归并可能把合法重放错误判为冲突的问题。
- 会话标题可在首轮完成后由当前模型生成简洁语义标题，手动或自动改名会同步更新会话页标题；标题生成不再与主请求竞争 provider。
- 统一 Desktop 会话正文、运行状态、审批卡和 composer 的宽度，收窄审批弹窗，并修复不确定总量被显示成停滞百分比的问题。
- 增加从当前源码启动的 Desktop Gherkin E2E，覆盖真实审批、Enter 排队、skill/agent 加载、`/plan`、自动规划与并行 Agent；同时保留 TUI 的 stateful 与 orchestration PTY 验收。

## v0.0.1-alpha.6 - 2026-07-30

以下变更已包含在打包发布的 `v0.0.1-alpha.6` 中。

### 新增

- 增加 AI 规划任务执行：Sigil 可以把一个仓库目标转换为可见、可审查的步骤计划，并行运行相互独立的步骤，继续遵守常规工具审批，最后以仓库自己的验证证据收口。
- 为未来桌面客户端增加 `sigil serve` 的带认证、跨重启历史会话目录，支持有边界的分页、标题搜索、模型服务/固定/来源状态筛选，并在游标过期时明确要求重新查询。会话日志仍是事实来源，目录故障不会阻止运行或记录。
- 增加供受信任本机客户端使用的桌面运行桥接：服务重启后可重新打开目录中的 durable session，启动信息与服务元数据共用一份版本化 JSON，并可通过显式启用的 stdin owner pipe 在不轮询 PID 的情况下触发优雅关闭。
- 增加从源码构建的桌面 dogfood 壳：通过同一套带认证的本机服务完成原生工作区选择、durable 历史、对话运行、精确审批与取消以及验证证据查看。CI 会生成短期保留且未签名的 macOS、Linux 与 Windows dogfood artifact；它们不是公开安装渠道。
- 增加 exact-route orchestration rollout manifest。qualified release 可以为匹配的新安装启用 `auto + proactive`；manifest 缺失、过期、无效或 route 不匹配时会 fail closed。
- 增加 session-scoped durable 工具输出 artifact。大型 shell、文件、搜索、terminal 与 MCP 结果在对话中只保留 bounded 卡片；policy-safe 正文可由模型、TUI、HTTP 与桌面端通过 typed、限额分页或 literal search 精确读取。

### 调整

- Provider API key 存储现在默认使用 owner-only 凭据文件。已有显式 `auto` 配置改为非交互、严格文件落盘；它不会查询旧的原生系统记录。只有显式 `keyring` 模式可能显示系统密码框。
- 上下文精简只使用 cache-aware V3：保持 provider/tool 稳定前缀，多次精简后仍通过带来源的 checkpoint 延续有效意图，按完整回合保留 tail，并以可信 cache 成本做准入。手动压缩会直接请求一个可恢复语义 checkpoint；正常 semantic compact 会在当前 route 额外调用一次无工具 LLM。provider-native materialization 在精确 route resume 落地前保持 fail-closed。
- 围绕工作区/会话导航、单一对话任务表面和验证检查器重构桌面 dogfood 壳。它能够回放有边界的已保存消息，在工作区服务保持打开时跨导航保留运行控制，将最终回复与进度/工具输出分开，并提供聚焦的审批、差异、证据和会话草稿交互。
- 为桌面壳增加统一视觉系统、自适应宽屏/双栏/紧凑布局、跟随系统的亮色与暗色主题、高对比度与减少动画适配、键盘焦点捕获/恢复、只在结束时播报流式运行摘要，以及低至 320 CSS 像素的可用重排。
- 不兼容配置会被拒绝，不会迁移。Doctor 会报告 release qualification；`manual + explicit_request_only` 仍是 rollout 的 coarse rollback，且不会删除 Task history。
- 新 session 默认使用 V2 tool-result schema，并在 semantic compaction 之前先做 deterministic tool-output aging。pre-V2 开发日志明确不兼容：Sigil 以 bounded schema diagnostic 将 inline 正文标记为 `Unavailable`，保持原文件不变，不回填、改写或猜测缺失 artifact。

### 修复

- 修复非交互 `auto` 凭据检查在 macOS Keychain 上无限等待的问题。`auto` 不再访问原生存储；显式原生检查未及时完成时，Doctor 会回退到离线状态。
- 修复打包桌面应用在 Tauri setup 生命周期执行前读取托管状态的问题；该问题会让 macOS 应用在创建窗口前直接退出。
- 修复事件驱动 TUI 在空闲状态下处理第一个按键后不再继续接收输入的问题。
- 修复后台工具 artifact 维护与会话操作竞争的问题；此前该竞争可能返回瞬时锁失败，而不是完成用户请求。

## v0.0.1-alpha.5 - 2026-07-18

以下变更已包含在打包发布的 `v0.0.1-alpha.5` 中。

### 新增

- 增加远端 Streamable HTTP MCP 服务的显式 OAuth 登录，包括自动或手动回调、系统凭据存储、刷新、退出和具体的恢复错误。每个目标仍会经过常规网络提示与目标检查；非交互启动不会打开浏览器。
- 增加可配置的信息栏显示状态、通过 `F2` 显示或隐藏的快捷键，以及复制选中对话记录或最新智能体回复的命令。

### 调整

- Windows Shell 与终端工具现在默认使用 PowerShell。Doctor 和工具卡片会显示检测到的 Shell，超时后也能更可靠地停止子进程；本地执行仍不提供隔离。
- 激活或刷新远端 MCP 服务时，可用工具会同步更新，不再留下旧的重复项。Windows 也能更可靠地清理已经停止的本机 MCP 进程树。
- 为了统一产品形象，更新了 Sigil 标志、仓库首页、文档站、社交预览图和发布材料。

### 修复

- 回复完成状态、排队任务和会话状态切换现在可以更可靠地恢复，避免最终回复重复或滞留。
- 长会话会限制时间线尾部索引的更新范围，减少历史记录增长后的重复渲染工作。

## v0.0.1-alpha.4 - 2026-07-16

以下变更已包含在打包发布的 `v0.0.1-alpha.4` 中。

### 新增

- 增加默认关闭且具有明确隐私边界的终端通知，用于提示长任务完成、等待审批、执行失败或需要用户输入，并可自动选择 OSC 9、OSC 777 或 BEL。
- 为 Rust、Python、JavaScript/TypeScript 与 Go 增加有边界的仓库上下文：优先使用可用的语言服务器，不可用时回退到内置解析器。
- 增加 TUI 图片附件：可通过本地路径或系统图片剪贴板输入 PNG、JPEG 与 WebP，提供可删除的附件标签，并明确检查模型服务和模型是否支持图片。
- 增加 `sigil doctor --output json`，为支持请求提供带版本且脱敏的本地诊断格式。
- 增加 `/feedback`：先预览包含和排除的数据，再显式导出仅保存在本机的 JSON；报告绝不会自动上传。
- 增加用于报告问题、提出功能建议和反馈文档问题的结构化 GitHub 表单。

### 调整

- 补全 `/feedback` 的后续流程：导出后可以在 TUI 内检查报告、在文件管理器中定位文件，或明确打开问题报告表单；只有用户自行添加附件时，报告才会离开本机。

## v0.0.1-alpha.3 - 2026-07-15

以下变更已包含在打包发布的 `v0.0.1-alpha.3` 中。

### 新增

- 为脚本增加稳定的 `sigil run --output json` 与 `--output jsonl` 格式，并增加只监听本机、要求 bearer 认证的高级 `sigil serve` 接口。
- 增加明确的已保存会话操作：安全导出、从当前对话分叉、固定会话、删除前精确检查，以及保留期限清理的预览与确认。

### 调整

- 所选模型具备已安装的本地计数支持，且压缩后请求已证明可以装入上下文时，`/compact` 现在可以确认一次手动上下文压缩。已完成的长对话与排队请求可以使用同一检查路径。一个固定的官方 OpenAI Responses 模型也可以在确认尚未产生输出的上下文超限后，经过独立计数和节省量检查，只恢复一次。

## v0.0.1-alpha.2 - 2026-07-15

以下变更已包含在打包发布的 `v0.0.1-alpha.2` 中。

### 新增

- 通过 `[providers.openai_responses]` 增加 OpenAI Responses 模型服务。
- 增加稳定的 `websearch` 和受支持的 `webfetch` 路由，并提供独立的网络控制和可见来源。
- 增加任务验证卡片、`Alt-V` 聚焦、推荐检查，以及与已检查文件和修改对应的可查看证据。
- 增加 `Ctrl-R` 检查点检查，并提供受控恢复或从当前对话分叉的选择。
- 增加通过 `/compact` 打开的只读 Context Compaction V2 预览。

### 调整

- 除了 stdio 服务，本地 MCP 还支持在用户级配置中添加 Streamable HTTP 服务，并沿用同一套信任、审批和敏感信息出站策略。
- 围绕验证、恢复和上下文控制更新用户文档与网站导航。

### 当前限制

- 修复正确性问题期间，Context Compaction V2 的应用操作仍暂时停用，包括受控的上下文超限恢复；`/compact` 目前只提供检查预览。

## v0.0.1-alpha.1 - 2026-07-08

### 新增

- 发布带命名空间的 npm 包：`@sigil-ai/sigil@alpha`。
- 发布 Homebrew Tap 配方：`JimmyDaddy/sigil/sigil-ai`。
- 补齐 npm、Homebrew、Cargo Git 标签、源码构建和手动下载发布归档的安装路径。
- 生成 GitHub Pages 文档页，覆盖安装、配置、模型服务、安全、隐私、MCP、界面导览、故障排查、参考和当前支持状态。

### 调整

- 明确 `v0.0.1-alpha.1` 属于早期预览：核心 TUI 工作流已经可用，但配置、插件 API、高级沙箱行为和自动化入口仍可能调整。
- 把文档入口改成更清晰的任务路径：快速上手、安装、视觉导览、日常工作流、安全、排障和参考。
- 更新用户文档中的模型服务范围：DeepSeek、OpenAI-compatible、Anthropic 和 Gemini。

### 已知限制

- 暂不支持自更新。
- alpha 阶段暂不承诺稳定的插件 API 兼容性。
- 沙箱覆盖范围会随平台与后端而变化。
- 非交互自动化入口不能展示交互式审批弹窗。

## v0.0.1-alpha - 2026-07-07

### 新增

- Sigil TUI 的首个公开 alpha 版本。
- 通过 `sigil` 命令进入 TUI。
- 快速设置、`/config`、`sigil doctor` 和 `/doctor`。
- 通过 `/task` 和 `/plan` 使用多步骤任务与规划流程。
- 文件变更、Shell 命令、MCP 使用和代码智能编辑都经过审批控制。
- 重启后可以恢复保存在本机的会话。

### 已知限制

- 这是最初的预览版本，已经被 `v0.0.1-alpha.1` 取代。
- 用户应优先使用 `alpha` 安装渠道，或最新文档中列出的发布标签。

<!-- public-doc-cta: open-installation -->
下一步：[查看安装与更新方式](installation.md)。
