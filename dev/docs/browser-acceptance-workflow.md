# 可选浏览器验收工作流

`dev/workflows/frontend-acceptance` 是显式安装、显式选择的项目插件样例。它将实际页面交互、页面状态、console/network 与截图作为代码验收证据，复用既有 skill、插件信任、MCP 审批和进程结算。普通任务不会因此新增浏览器依赖或预检。

安装步骤、锁定版本与目标项目目录见[工作流 README](../workflows/frontend-acceptance/README.md)。依赖安装由用户显式执行，运行时不通过 `npx` 下载包。skill 不接受模型自动选择；用户在 Desktop/TUI 选择它后，模型通过已有 typed MCP 工具调用浏览器。网页内容不构成指令或授权。

## 验收约定

1. 确认具体页面目标与本次拥有的应用服务。外部站点或真实账户数据需要相应授权。
2. 先重现错误：真实 navigate/click/fill 后检查可见状态及请求结果。语法检查、构建成功或模型口述不能替代交互断言。
3. 修复项目源文件，重新加载同一路径并重复交互。保留失败和修复后的截图、console、network、断言结果与源文件哈希或 diff 身份。
4. 文件再次改变后重新验收；旧截图只说明生成时的代码状态。
5. 通过 `browser_close` 关闭本次浏览器，运行 owner 继续等待 MCP exact generation 的 drain、quiesce 与结算。只结束本次拥有的应用服务，不使用全局 `kill-all` 或 `close-all`。

默认 Playwright 配置只允许 HTTP loopback origin。新增 origin 应是应用实际需要的完整 origin。这个过滤器不是操作系统网络 sandbox，不扩大 Sigil 权限。浏览器使用独立 profile，不连接个人浏览器、已有 CDP endpoint 或登录态。

工作流的固定启动脚本保留配置与产物的绝对路径，并在 Unix 上于既有受管 `TMPDIR` 内使用相对 socket pathname；长工作区或缓存路径不会因此超过 Unix socket 地址长度。它与 MCP CLI 使用同一进程、stdio 和生命周期，MCP roots 仍指向原工作区，文件限制不变。Windows 使用原 named-pipe 路线。这是锁定 SDK 的工作流兼容处理，没有新增通用临时目录授权或浏览器进程管理器。

## CLI 与 MCP 的选择依据

官方 [Playwright CLI](https://github.com/microsoft/playwright-cli) 通过命令与 skill 降低常驻工具描述成本；[Playwright MCP](https://github.com/microsoft/playwright-mcp) 通过 stdio 暴露结构化工具。两者均可完成实际页面交互，不能单凭 schema 大小判断任务总成本。

本样例目前选 MCP：它沿 Sigil 已有受管 stdio 生命周期运行。CLI 的 daemon 跨短命令存活；取消一个 CLI 请求不会关闭其浏览器 session，因此还需要独立验证准确的 named-session 所有权与取消收尾。本样例没有把 CLI daemon 当作普通一次性 shell 子进程交付。

2026-09-27 的独立 SDK 对照使用固定 Playwright CLI 0.1.21、MCP 0.0.82 和 Chrome 153.0.8010.53。在同一坏页面上，语法检查通过，但两条浏览器路线都发现点击失败；修复源文件后，都验证了成功页面状态和一次实际 POST。取消测试确认：CLI 短客户端退出后仍需 named close，且关闭本次 session 不影响另一个控制 session；MCP 在活跃调用期间 stdin EOF 后退出，读取线程已 join。

这些结果是独立 SDK、真实浏览器、已知交互脚本的验证。它们不是 Sigil 真实模型任务通过率、操作系统进程树身份结算证明或稳定性能承诺。Sigil 全链路与真实模型验证记录在本轮执行台账中，完成前不得以 SDK 结果替代。

## 维护

包锁定 MCP 0.0.82，其上游依赖 Playwright 1.64.0-alpha-1789764292000。升级时同步 lockfile，并复跑坏页面/修复页面、取消与精确 session 清理。没有浏览器或安装失败时应显示可修复诊断，不阻塞未选择此工作流的普通会话。
