# Desktop workspace 生命周期与辅助信息

`sigil-desktop::DesktopWorkspaceManager` 是每个 canonical workspace 的 native child owner。renderer 只使用 opaque workspace/session ID，不取得路径、bearer、process handle 或通用 I/O。全局 map 的锁只覆盖接纳、短状态读取和发布；启动、退出、pipe drain 与 reap 在 manager 持有的 operation task 内推进。

## 唯一 owner 与关闭

- Opening/restart 期间保留 canonical root 与已知 workspace ID 的 reservation；Closing 直到确认 cleanup 完成也保留 reservation。同 root 的打开、重启、关闭互斥，无关 workspace 仍可取得 typed client、停止运行和完成审批。
- 调用者等待 oneshot 回执；取消等待不会取消 operation 或丢弃其 child。manager 持有 operation JoinHandle，操作结束才释放登记。shutdown 的真实结果可重复读取，不将已完成的 shutdown 再解释为未知 child。
- child 的 Ready 发布、identity collision 检查与全局 closing 检查在同一临界区完成。启动迟到且 admission 已关闭时，child 进入 cleanup，不能重新发布为 Ready；不同 root 的身份碰撞清理不能释放另一 root 的 reservation。
- `close_all` 先关闭 lifecycle admission，等待已接纳的 open/restart/close，再关闭所有登记及 cleanup 保留的 child。多个 `close_all` 串行 drain；取消调用 future 不会结束 manager-owned cleanup。
- shutdown 失败不是 Stopped：保留 child handle 与 root reservation，后续 close/close_all 重试 cleanup。native 更新与退出都检查 cleanup 结果；失败时保持应用打开，退出通过固定的 `sigil-workspace-cleanup-failed` 事件提示重试，不向 renderer 转发原始错误或路径。

## 已有会话不依赖辅助目录

已有 ConversationPanel 使用自身 session continuity、run owner 与 run context。provider inventory 正在读取、读取失败或默认模型未配置，都不卸载已有会话；失败仅显示局部提示和重试/设置入口。没有已选会话时继续显示相应 onboarding 或配置错误。recent-workspace 写入失败也不撤销已打开 workspace 的成功结果。

同一后端切换会话时，空闲会话的强租约缓存仍会释放；尚有实际 supervisor 持有的租约通过本 driver 的弱索引按 durable scope 与 canonical path 精确复用，因此切回等待审批的会话不会误报另一窗口占用。弱索引不延长租约寿命；实际 owner 释放后仍重新获取租约，其他 driver 的占用继续返回 Busy，renderer 不以本地缓存代替租约校验。

这些展示规则不批准新的 provider route，不覆盖运行状态或真实权限错误。启动 run、取消与审批仍由原 application/runtime owner 校验准确的 scope 和身份。

已配置默认模型或候选模型的库存 readiness 是诊断，不是新建会话或切换模型的许可。Desktop 允许用户选择配置中的 route 并把请求交给共享服务；建会话时校验 route，真正启动 run 时再校验当前凭证和协议能力。缺少默认模型仍展示首次配置向导，库存诊断问题在已有会话中保留设置提示。

打开会话后，历史或 continuity 尚在读取、读取失败等待重试时，已选会话的本地文字草稿和图片选择仍可编辑；文字草稿继续按会话保存，已选图片在当前 composer 的重试过程中保留。发送和后续消息排队继续等待会话恢复及准入完成。切换会话时，在新目标真正选中前，旧会话保持不可操作，避免把输入交给错误的会话。

## 审批预览与恢复

等待审批时，canonical run snapshot 携带同一 request、call、tool-call hash、policy、expiry 与 event sequence 绑定的冻结安全预览，只公开已有 public preview 的 title、summary、body；不重新运行工具生成预览，也不把展示文字重新计算成 prepared mutation digest。新 renderer 与重连从仍在运行的 owner 取得这一事实，不依赖旧 renderer 缓存。服务端重启后已中断的审批不会因此复活。

live 与 canonical 预览复用同一共享文字安全投影；该投影先处理 Bearer 凭证值，再处理既有 URL 和 secret carrier，避免只遮住 Authorization 后的认证方案却留下凭证。无凭证的差异正文保持原样；这项回归不代表完成整套安全验收。

每个预览字段最多 128 KiB UTF-8，任一字段超限则整份预览缺失，不提供被截断的可批准前缀。一次 snapshot 的审批预览总预算为 256 KiB，优先等待中的请求，再保留非终结 lifecycle；终结 lifecycle 不重复正文。预算不足时只省略展示副本，保留准确审批身份和源预览。durable protocol envelope 继续使用既有 1 MiB 上限；若冗余展示副本使其超限，保留同 envelope 的 frozen public preview 作为重放来源。

文件写入、删除或 typed 文件操作，以及五类 typed 命令执行操作缺少具体预览时，Desktop 说明恢复方式并禁用所有“允许”，仍可拒绝或打开诊断。canonical snapshot 不会按工具名称、raw tool input 或旧请求补猜正文；预览恢复后才可再次批准。

等待审批的安全 display 附带可缺失的 `reconciliation_id`：HTTP 只从同一已冻结 protocol envelope 复制已有 opaque 语义锚，核对 run、call、request、hash、policy、有效期和事件序位后再更新 snapshot。native 转发该真实关联锚，renderer 仅在 canonical 请求项确实带有同锚时合并纯占位；旧字段缺失、锚不匹配或真实结果存在时保留原记录，不猜造 `live-v1` 身份。该字段只服务展示，不改变审批 guard、预览预算或授权。

`exec_command` 的 required preview 从本次调用参数和不可变 shell 配置冻结完整命令、请求工作目录、shell、终端模式和运行期限；不执行命令、不读取文件或解析实际路径。kernel 的 argument-only preview 契约默认关闭，仅内置命令工具显式声明这一纯格式化能力。外部目录仍使用原 permission subjects 的 canonical 目标和风险说明，并与命令组合后冻结；其他工具继续等待外部访问授权，不能提前读取文件生成预览。纯参数预览失败或缺失时不以错误正文或外部路径清单冒充完整动作；超过既有预览预算时整份公开 review 不可用，不能批准截断前缀。它不引入新的 renderer 路径能力或改变 prepared digest。

取消超过同步等待时限时，已发布的 Uncertain command receipt 保留原样。HTTP supervisor 的真实执行结束后，错误收尾只接受该执行冻结的 cancellation scope：必须有同一 request 的持久化取消完成记录，结果为 Cancelled、cleanup_complete 为 true、活跃 effect/task 均为零，且同一 run 的准确 domain/public terminal 为 Cancelled。满足这些条件后，在释放原 attachment 前，只做一次该 run 的待投递 outbox 重放和既有终态收敛。Interrupted、Failed、清理未完成或身份不匹配都不进入此补偿路径，继续保留 uncertain 前台状态与安全诊断；不以执行线程已结束、超时、owner 消失或公开事件的单独存在推断清理完成。

## 默认信息层级与草稿

默认页头不展示低价值运行计数；运行与启动状态由一处状态提示承担。任务卡优先使用真实目标，缺少目标时显示中性的“任务”，只为暂停、失败、取消或阻塞保留可行动说明；普通运行的技术状态进入详情。桌面“暂停，稍后继续”保留续作能力，“取消任务”明确结束当前任务且保留已完成的修改，不把二者写成相同的停止后果。

推理与进度默认折叠，不重复显示原始 streaming 字段；推理不默认展示正文预览，进度保留有界短预览。用户展开或收起后，同一消息的流式更新与完成保留选择，只有新消息身份才重置；最终答复仍直接流式显示正文。

审批的具体命令、文件差异和必要风险保持可检查，技术分析字段进入折叠详情；可滚动的审阅区域与始终可达的决定按钮分别布局。短小的已保存工具输出通过现有有界只读检索直接显示，搜索与分页按需展开；读取失败仍显示错误。恢复面板没有快照时使用简短空态，压缩和分叉分别折叠，受控文件恢复的限制紧靠真正的恢复动作。

当前待决审批正文已显示时，仅收起同一运行、调用与关联锚对应的工具请求占位和审批等待提示；身份不确定、其他审批、真实输出及已决历史继续显示。自动重试的 waiting/recovering 提示在同一运行的更新完整答复、等待用户输入或终态，以及 canonical 收敛后退休，旧序号重放不会重新显示；同一运行的待决审批或用户输入表单已显示时，自动重试提示退位给当前决定。需要用户处理的 blocked/paused 提示与持久重试历史保留。

有可审正文的新审批默认聚焦正文滚动区，键盘可直接逐页审阅；同一 guard 的快照更新不重复抢焦点，Escape 仍回到输入框。缺少正文时保留可见恢复提示并禁止允许。分析受限的默认说明提示人工核对完整命令与环境，技术原因及单次授权限制继续保留。

文字草稿绑定 workspace 与 catalog 提供的持久会话身份，不使用每次重开变化的 renderer session ID。新会话晚取得 catalog 身份时只迁移本地文本存储键，不重新挂载编辑器或图片。正常关闭重开同一会话可恢复文本，草稿不会自动发送；图片仍属于当前进程的原有 attachment 生命周期，不声称跨进程保存图片。文本沿用既有 256 KiB 本地保存预算和浏览器存储可用性边界。

## 验收

通过统一隔离入口运行 `cargo test -p sigil-desktop --lib`。Unix native fixture 使用真实 child、stdout bootstrap、loopback metadata 与 stdin owner pipe，并以显式文件屏障覆盖慢启动、慢关闭、restart、调用者取消、close_all 和身份碰撞；测试失败不把等待超时当作退出证据。

`pnpm --dir apps/desktop exec vitest run src/App.test.tsx` 验证 inventory loading/失败/默认未配置时，恢复的 live session 仍可见并可调用精确的 Cancel work（取消任务）；也验证 cleanup 失败通知及订阅收尾。cleanup native event 仅携带固定错误分类。

同一 App 测试还验证历史加载和失败重试期间文字及图片草稿可编辑且保留、发送仍阻止，以及切换会话时旧会话保持 inert 直至新目标选中。

审批回归覆盖 HTTP frozen review 的持久化重开、身份与预览不匹配拒绝、单字段及总预算边界、native live/canonical 的冷恢复与超限限制，以及 renderer 不复用旧请求预览和无正文时禁止允许。HTTP OpenAPI 与生成的 renderer schema 同步新增可缺失的安全预览。

取消回归通过真实 production driver 与 loopback provider 进入手工文件审批等待，再验证取消后的原始文件、唯一 durable/public terminal、sealed protocol、前台 owner 释放与下一请求准入；同一错误收尾入口也验证 clean Cancelled 后的 protocol cache 恢复、另一 execution scope 的拒绝，以及仍等待审批、Interrupted/Failed 或取消清理证据不完整时不会强行结束运行。负例在独立副本中沿用真实 production control 的 scope，以 typed durable writer 追加替代终态，不修改原始取消记录。

真实 production 会话切换回归覆盖 A 等待文件审批、创建 B、重新打开 A 后同一租约 generation 与审批 identity/hash/preview 保留、批准后继续新请求；同时验证其他 driver 的真实 Busy、错误路径拒绝，以及原 owner 真实释放后能够正常取得新租约。
