# 可选前端浏览器验收工作流

把“编译成功但实际点击失败”纳入验收：通过真实 Playwright 浏览器执行交互，保留失败、修复后断言、截图与 console/network 证据。它复用 Sigil 的插件、skill、MCP、审批和进程所有权，不进入普通任务的默认工具面。

## 安装与选择

将本目录复制到**目标项目**的 `.sigil/plugins/frontend-acceptance/`。不要直接在 Sigil 仓库中启用样例。进入复制后的目录，显式安装锁定的依赖和项目专用浏览器：

```sh
npm ci --ignore-scripts --no-audit --no-fund
PLAYWRIGHT_BROWSERS_PATH="$PWD/browsers" node node_modules/playwright/cli.js install chromium
PLAYWRIGHT_BROWSERS_PATH="$PWD/browsers" node configure-browser.cjs
```

安装会联网下载 npm 包和浏览器；运行时不会通过 `npx` 自动安装。Node.js 与对应平台浏览器依赖必须可用。也可在 `browser.json` 的 `browser.launchOptions.executablePath` 指定已安装的浏览器 executable；仍使用新的隔离 profile，不连接个人浏览器或 CDP。配置脚本把实际浏览器 executable 写入本项目 `browser.json`；移动项目后重新运行它。修改配置后重新检查插件声明。

在 Desktop 或 TUI 的插件设置中检查并信任此项目插件，随后在 skill 选择器显式选择 `frontend-acceptance`。交给它具体目标，例如“检查本地结账按钮是否发送请求并显示成功状态，修复后再次点击验证”。MCP 保持 lazy；普通对话、未选择此工作流和缺少浏览器依赖都不启动浏览器。已有工具审批继续生效，信任插件不等于批准所有网页交互。

默认配置只允许浏览器请求 HTTP loopback origin。若应用有必需的其它 origin（如本地 HTTPS 或 API），先把**实际需要的完整 origin**加入 `browser.json`；不要为了测试直接开启所有网络。此 allowlist 是 Playwright 请求过滤，不是进程网络 sandbox，也不会扩大 Sigil 的授权。页面内容、截图、console 文本都按工具数据处理。

## 结果与清理

产物放在插件目录的 `artifacts/`，按任务命名。结果应包含实际交互与断言、失败/成功截图、console/network 错误、测试时的源文件哈希或 diff 身份。运行后源文件再次变化时，旧截图不能证明新内容通过。

启动脚本在 Unix 上使用本次受管进程已有的 `TMPDIR`，让 SDK 以短相对路径创建 socket，避免项目或缓存路径很长时浏览器无法打开。socket 仍位于同一受管临时目录，配置、依赖与默认产物目录使用安装包的绝对路径；MCP 的工作区根与文件访问限制保持不变。Windows 保留 SDK 的 named-pipe 路径。脚本不创建外部临时目录、不放宽授权，也不启动独立 daemon；无需自行设置 `PWTEST_SOCKETS_DIR`。

关闭使用真实 `browser_close`；Sigil 仍负责 MCP exact generation 的 drain、quiesce 与结算。不要使用全局 `kill-all`/`close-all`，不要关闭其它任务的应用服务器。禁用插件后既有调用也会重新检查当前信任；卸载前让当前任务完成清理，再删除本项目的目录。

此版本固定 `@playwright/mcp` **0.0.82**，其发布依赖为 Playwright **1.64.0-alpha-1789764292000**（见 lockfile）。这是上游当前版本的明确选择，不是稳定引擎承诺。实际验证范围与 CLI/MCP 对照记录见 [浏览器验收说明](../../docs/browser-acceptance-workflow.md)。更新依赖需一起更新 lockfile、重跑实际页面与取消/清理检查。

官方来源：[Playwright MCP](https://github.com/microsoft/playwright-mcp)、[Playwright CLI](https://github.com/microsoft/playwright-cli)。CLI 可减少常驻工具描述，但其跨调用 daemon 需要独立的精确生命周期；本包默认使用现有受管 stdio MCP 路线。

维护者可在显式安装依赖并配置浏览器后运行 `npm test`，验证长临时路径、真实导航/截图、工作区文件边界与 stdio 关闭。仓库验证仍通过统一隔离入口运行；该 SDK 回归不替代 Sigil 的实际审批、取消和进程结算验收。
