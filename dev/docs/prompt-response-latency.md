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

## 分段诊断

`sigil_run_latency` 是 process-local tracing target，不是 session/control authority，也不把内容放入用户 timeline。

| phase | 含义 |
| --- | --- |
| `run_route_projection` | HTTP 运行准入/准备的 fresh route 查询，以 session_scope_id 关联 |
| `total` | application run preparation 整体耗时 |
| `session_preparation` | 会话与本地配置准备，含 blocking worker 等待 |
| `provider_construction` | 所选 provider 装配 |
| `tool_surface` | 工具及配置为 eager 的 MCP 准备 |
| `request_context` | 请求上下文收集 |
| `provider_dispatch` | 准备调用 provider stream |
| `provider_stream_ready` | provider 返回 stream 对象；不代表首个内容已经到达 |
| `provider_first_chunk` | 首个 stream item 到达，可能只是 metadata 或错误 |
| `provider_first_content` | 首个非空 text/reasoning 内容到达；不等同于 GUI paint |
| `terminal_io` | HTTP 终态观察、outbox 重放或关闭的 blocking worker 已 join |

准备阶段的 ended 只表示离开阶段，可能成功、失败或取消；不表示业务成功。provider 各时间从该 physical attempt 的 stream 调用前起算，使用 run_id 关联。日志只记录阶段、run_id/session_scope_id、结果与时间，不记录 prompt、响应文本、地址或凭证。hosted 内容仍须完成原有安全投影才能发布，计时不绕过该边界。

可在已有输出日志的 CLI/HTTP 调试进程上设置 `RUST_LOG=sigil_run_latency=debug`。交互 TUI 的 tracing 默认写入 sink 以保护终端，不能宣称设置此变量就会产生 TUI 日志文件；本次没有增加绕过 resource authority 的文件日志。

## 验证口径

用 deferred Promise 验证 UI 在 receipt 前的状态，用协议 barrier 验证 MCP 实际并发及 failure cleanup，用阻塞 admission 证明 TUI 主线程可继续绘制/处理 Stop，用 gated provider stream 验证 text/reasoning 不等待后续 chunk 或结束才发布。这些测试证明顺序与生命周期，不把 fixture 时间宣称为真实模型延迟改善比例。

实际首响还取决于会话长度、所配置的 eager 服务、机器负载、网络与模型推理。没有现场分段 trace 时，不将用户观察的全部等待时间归因于模型，也不承诺固定秒数。
