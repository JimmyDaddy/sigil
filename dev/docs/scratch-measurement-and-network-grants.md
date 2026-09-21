# 普通 scratch 计量与网络会话授权

普通 Bash 和 terminal 使用 RA 准备的当前 session namespace。准备仅确认受管根、namespace 身份、私有权限；执行期间继续持有 RA lease，根被替换为符号链接、lease 无法建立或实际文件系统创建失败仍拒绝操作。512 MiB / 4 GiB 配置值是计量和清理阈值，不提供物理硬配额，也不在命令启动前扫描当前目录或其他会话。

计量在显式查询或 GC 维护中有界进行。RA 保存带时间戳的 Known 或 Unknown；Unknown 保留进程内上一次成功样本，历史值不加入当前 known subtotal。汇总出现任何未知 owner 时，`workspace_usage_bytes` 为 `None`，另给 known subtotal、unknown owners 和观察时间；没有上次样本时保留 `None`，不补零。重开后重新观察，不把旧值视为当前事实。quarantine 仍占空间，不能被当作释放；只有自身 owner / lease 检查后的实际删除或已确认缺失，才清理对应观察。

旧 `.authority-quota/session-scratch.json` 保留原始内容供历史检查，普通 scratch 不再读取它作为授权或改写它。实际 RA managed writer 的 capacity reservation、quota journal、owner 上限和底层 ENOSPC / EDQUOT 仍由原 writer 边界负责；磁盘余量或目录计量失败不会撤销已有 writer reservation。普通子进程直接写目录，因此不能宣称具有逐字节硬限额；需要物理硬限额的强能力要求仍由真实 backend capability 验证。

网络“本会话允许”使用独立 `NetworkSessionGrantBindingV1`，绑定 exact endpoint、transport、实际选中代理 route 以及当前网络 policy 的摘要。webfetch 的 exact URL 摘要包括被展示文本脱敏的 query 差异；websearch 绑定实际 bundled profile 或配置的 MCP transport、服务及环境来源。查询文本变化不扩大端点授权。URL capability、SSRF、DNS、重定向和目的地规则仍逐请求执行。

本地会话 grant 继续使用 containment binding；网络 grant 不伪造 shell backend/profile/environment。持久记录必须且只能有一种 binding。旧 local grant JSON 仍可读取；网络 grant 的 mint、reload 和匹配使用同一绑定规则，端点、route、transport、policy 或 permission authority 版本变化后旧授权不能复用。工具在执行入口还重验批准时的网络绑定，避免等待用户审批期间代理或凭证环境发生变化后直接执行。

定向验证：kernel agent approval 实际执行、落盘重开和重复调用，exact endpoint / route / transport / policy 漂移；真实 web 工具计划绑定；Bash 和 terminal 在超过原阈值、其他 namespace 无法计量时仍可运行，自身根无效或 lease 失败仍拒绝，GC 保留 leased namespace。
