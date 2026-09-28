# Prompt 响应与交互等待

发送反馈、运行准备、provider 返回首个数据、可见内容与最终完成是不同事实。准备状态不得被表示为模型已经在思考，也不得为了早显示状态提前发布持久完成或放宽取消、身份、审批与来源校验。

## 产品行为

- Desktop 发送时立即显示启动/发送状态，不等待 native receipt；继续 Task 采用相同反馈并阻止重复提交。排队 prompt 在 receipt 之前显示发送中，确认后才显示已排队。异步请求只清空自己提交的草稿版本，保留等待期间的新编辑。
- TUI 普通运行、会话创建/切换、配置保存与其他 typed application 命令由受管后台 admission owner 执行，恢复及持久命令准入不会占住绘制线程。配置后续动作仅在持久 receipt 确认成功且线程退出后应用；草稿替换与面板关闭还必须匹配原面板实例和编辑版本。回执已到但线程未退出时，界面继续唤醒以完成收尾。配置/会话动作按顺序提交，后续会话目标跨 worker 重绑保留。已退出线程的未决请求保留原恢复身份，不再阻塞新建/切换会话，也不会自动转发到新 worker；恢复仍需已有的显式持久恢复路径。Stop 与 admission 有 generation 绑定，已停止的旧 admission 不能迟到启动新运行；每次新运行使用独立操作身份，旧回执不清除新的输入/运行状态。首个 provider 内容之前显示准备状态，实际 reasoning 事件到达后才显示思考状态。

## 准备链路

- HTTP 运行准入、队列和 supervisor 读取 fresh、窄的 route facts，不构造用于界面的扩展目录、模型列表或 usage 展示。用户打开模型/扩展选择时仍使用完整投影。会话 frontier、route generation、recovery binding 与实际配置校验保持当前值，禁止用展示 cache 代替 authority。
- 普通 run 的同步路由读取、本地工具/skill 装配和 MCP 生命周期文件扫描在受管 blocking worker 中执行，调用者等待其完成；不会把 JoinHandle 丢弃来伪造快速返回。
- HTTP 终态读取、重放与关闭由调用者等待受管 blocking worker 完成；仍以 durable terminal 和 outbox 为完成依据。恢复对账按 event id 建立一次索引，保留原始事件顺序与重复 id 的首条匹配规则。后台 agent 等待前台运行释放时使用可取消的异步观察，避免运行时关闭时卡在常驻 blocking loop；取消观察不代表运行已经结束。
- stdio MCP 每批最多 4 个服务并发启动/发现，同批全部 settle 后按配置顺序注册，以保持名称冲突处理确定性。失败会清理同批尚未注册的成功连接，并回滚已注册连接。保留 durable lifecycle audit、required/optional 与 eager/lazy 配置语义。
- 仓库 lexical context 除最多 160 个文件外，原始目录项枚举最多 2,560 项；RepoMap 同样在底层枚举时消费其配置预算。忽略项、非源码项和读取错误都计入预算，每个目录先截取剩余额度内的前缀再排序，避免全量枚举/排序后才截断。预算耗尽时只保证已枚举前缀的排序，不保证选中全树字典序最前的文件；不设额外深度门禁。父级与本地 `.ignore`、`.gitignore`、`.git/info/exclude` 和全局 Git ignore 策略继续生效；额外明确支持 linked worktree 的 `.git` 指针与 `commondir` 共享 exclude。保留隐藏文件可见、不跟随符号链接及既有敏感路径排除。显式用户路径仍直接处理，不受目录扫描顺序影响。LSP 上下文仍只消费最多等待 35ms 的 warm snapshot，不为 prompt 隐式启动 LSP。

TUI 的 deferred application recorder 在同次同步初始化内复用一份已校验的 durable records，构造 public outbox 水位、终态与 Task/UserInput 投影；不为这些投影重复读取日志。existing-only reopen 的恢复校验保持不变，普通 adapter 回放及延迟执行仍读取当前记录。该优化不增加跨运行缓存，也不把旧 projection 当作后续写入许可。

## 分段诊断

运行时阶段直接进入有界的 process-local 缓冲，不依赖 `RUST_LOG` 或 tracing subscriber。
支持报告导出包含最近 256 条计时以及丢弃计数；锁争用、长度预算和淘汰只丢弃观测，不阻断运行。
缓冲不读写会话、不会自动上传，也不是 session/control authority。阶段为固定枚举，不接收任意描述文本。

| phase | 含义 |
| --- | --- |
| `run_route_projection` | HTTP 运行准入/准备的 fresh route 查询，以 session_scope_id 关联 |
| `input_accepted` | 本地输入被提交；还未取得持久运行身份时使用临时诊断关联 |
| `first_feedback_frame` | TUI 成功完成 terminal draw，或 Desktop 已提交反馈状态后跨过绘制帧 |
| `admission` | 本地输入到客户端取得 run receipt／TUI 收到 live run 的耗时 |
| `preparation` | application run preparation 整体耗时 |
| `session_preparation` | 会话与本地配置准备，含 blocking worker 等待 |
| `provider_construction` | 所选 provider 装配 |
| `tool_surface` | 工具及配置为 eager 的 MCP 准备 |
| `request_context` | 请求上下文收集 |
| `provider_dispatch` | 准备调用 provider stream |
| `provider_stream_ready` | provider 返回 stream 对象；不代表首个内容已经到达 |
| `provider_first_chunk` | 首个 stream item 到达，可能只是 metadata 或错误 |
| `provider_first_content` | 首个非空 text/reasoning 内容到达；不等同于 GUI paint |
| `tool_execution` | 实际工具执行到 settlement 入口，不含用户审批等待 |
| `cancellation_requested` | 开始请求停止，不表示已停止 |
| `cancellation_settled` | runtime 取消终态持久化，或 renderer 收到取消／中断终态 |
| `terminal_io` | HTTP 终态观察、outbox 重放或关闭的 blocking worker 已 join |

准备阶段的计时只表示离开阶段，可能成功、失败或取消；不表示业务成功。provider 各时间从该 physical attempt 的 stream 调用前起算。导出使用 run ID 的 SHA-256 标签关联；标签不提供认证或匿名性保证，不导出原始 ID、prompt、响应、路径、地址或凭证。hosted 内容仍须完成原有安全投影才能发布，计时不绕过该边界。

TUI 启动会记录实际经过的 session/provider/tool surface 与总 preparation；它先于用户 run，使用独立的进程内诊断标签，不冒充某次发送的阶段。普通发送的 session preparation、request context 与总 preparation 使用既有 public run 标签，与该 run 的 provider 观测关联。阶段计时不依赖 `SIGIL_TUI_PHASE_TIMINGS`；该变量只保留旧 stderr profiler 的显式输出。

Desktop 同一支持报告补充最多 32 次本地提交的反馈、admission、首内容和取消观察，按 workspace 过滤。
首内容消费已提交的 conversation reducer 内容（含有效 assistant/reasoning），不是收到任意非空 preview 即计时；旧 attempt、已退休内容及终态后被 UI 拒绝的帧不计。该观测表示 React commit 后消费到有效内容，不等同于屏幕完成 paint。
终态和 canonical retirement 关闭当前 run 观察并清理尚未绑定的 early-content；若终态早于 admission receipt，无法保留的首内容时刻保持未知，不从迟到帧补造。正常早到内容仍可在 receipt 后关联。
renderer 的所有时长从自己的输入接收时刻起算，runtime 的 `observed_at_ms` 只相对本进程启动；不得相减不同进程时钟。
刷新 renderer 或重启 host 后这些观测会消失，缺失值保持未知。保存报告不以先前诊断预览成功为门禁；当前导出本身的错误仍明确呈现。
HTTP route projection 和 terminal I/O 的额外开发日志仍可使用 `RUST_LOG=sigil_run_latency=debug`；
TUI tracing 写入 sink 保护终端，支持导出无需打开日志，也不增加专用日志文件或遥测后台任务。

对独立采样的支持报告运行 `python3 scripts/summarize-run-timings.py <report.json> ...` 可得到各阶段的样本数、P50/P95 和丢失观测数。
该汇总不混合 renderer 与 host 的时钟／起算点。host 按进程实例与观测序号去重；renderer 按独立提交标签与 phase 去重，取得 run receipt 后标签仍不变，因此重叠导出不会重复加权。缺少这些字段的旧格式无法可靠去重，仍应只输入独立采样。

## 验证口径

用 deferred Promise 验证 UI 在 receipt 前的状态，用协议 barrier 验证 MCP 实际并发及 failure cleanup，用阻塞 admission 证明 TUI 主线程可继续绘制/处理 Stop，用 gated provider stream 验证 text/reasoning 不等待后续 chunk 或结束才发布。这些测试证明顺序与生命周期，不把 fixture 时间宣称为真实模型延迟改善比例。

实际首响还取决于会话长度、所配置的 eager 服务、机器负载、网络与模型推理。没有现场分段 trace 时，不将用户观察的全部等待时间归因于模型，也不承诺固定秒数。

## 普通提交的持久接纳回执

普通文本和图片提交通过既有 application 命令 K/F 绑定原始输入摘要。运行 owner 持久化该次 `ConversationRunStarted` 后、启动 provider 前，沿同一 Session writer 原子追加 `ConversationRunAcceptedV1` 与既有 operation marker；摘要覆盖原始文字和图片内容引用，review 注释的完整 options 仍由命令指纹绑定。writer 同时核对该 run 仍活动，且其 start 晚于该命令的 prepare，防止新命令认领旧 run。

`ConversationRunAccepted` 回执只表示输入已被该次运行持久接纳。执行失败、等待用户输入、取消和最终成功仍以原 run 生命周期为准。TUI 可释放已接纳提交的 pending 槽位；32 个并发未决操作的保护保持不变。HTTP 使用同一事实恢复 exact run ID，异步准备尚未完成时仍保留不确定结果。

重试和重启通过原 K/F 对应的原子 batch 恢复回执。只有旧 `RunStarted`、相同 prompt、当前 UI 终态或通道 ACK 都不能补造接纳事实；缺少 marker 的历史不确定命令不会自动变成成功。连续提交回归必须经过真实 worker、application service 和 session writer，不能通过增加 pending 上限掩盖回执缺失。
