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

这些展示规则不批准新的 provider route，不覆盖运行状态或真实权限错误。启动 run、取消与审批仍由原 application/runtime owner 校验准确的 scope 和身份。

已配置默认模型或候选模型的库存 readiness 是诊断，不是新建会话或切换模型的许可。Desktop 允许用户选择配置中的 route 并把请求交给共享服务；建会话时校验 route，真正启动 run 时再校验当前凭证和协议能力。缺少默认模型仍展示首次配置向导，库存诊断问题在已有会话中保留设置提示。

## 验收

通过统一隔离入口运行 `cargo test -p sigil-desktop --lib`。Unix native fixture 使用真实 child、stdout bootstrap、loopback metadata 与 stdin owner pipe，并以显式文件屏障覆盖慢启动、慢关闭、restart、调用者取消、close_all 和身份碰撞；测试失败不把等待超时当作退出证据。

`pnpm --dir apps/desktop exec vitest run src/App.test.tsx` 验证 inventory loading/失败/默认未配置时，恢复的 live session 仍可见并可调用精确的 Stop run；也验证 cleanup 失败通知及订阅收尾。HTTP wire schema 未改变，native event 仅携带固定错误分类。
