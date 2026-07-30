# Desktop 构建、dogfood 与发布验收指南

状态：Desktop 与 TUI 是并列的一等产品表面。源码构建和短期 CI artifact 仍属于 dogfood；
macOS beta 使用独立的 Developer ID 签名、公证、staple 和签名更新通道，公开可用性以
GitHub Releases 与官网下载安装入口为准。Linux 与 Windows package 尚未进入公开分发。

## 1. 边界

- Desktop 与 TUI 是并列的一等产品表面，复用同一 kernel/runtime 语义。npm、Homebrew、Cargo 与 standalone
  GitHub release archive 分发 `sigil` 终端程序；macOS Desktop 通过双架构签名 DMG 分发。
- `apps/desktop` 监管每个工作区独立的 `sigil serve` sidecar。renderer 不持有 bearer、进程句柄、工作区绝对路径，也不能直接访问通用 HTTP、Shell 或文件系统。
- 会话重命名写入 workspace append-only lifecycle journal；删除复用 exact preview/apply，置顶、活动 run 或 verification 会 fail closed。SQLite 仍只是可重建目录投影。
- CI artifact 只保留七天，用于 dogfood，macOS 也只做 ad-hoc 签名。公开 macOS beta 必须走本地
  Developer ID 签名、公证与 staple，再上传 draft Release；Linux `.deb` 和 Windows NSIS 未进入公开发布工作流。
- `Desktop Package` 只在 desktop、bundled sidecar 生产代码、构建契约、manifest、toolchain 或 workflow
  变化时自动运行。参与 sidecar 构建的 Rust crate，其 `src/tests`、嵌套 `tests` 与顶层 `tests` 目录统一由主
  CI 覆盖，不为纯测试修改重复构建三套 installer；需要独立复验 package 时仍可手动 dispatch。
- macOS beta 已接入用户显式触发的签名 updater，通过
  `https://sigil.corerobin.com/updates/beta/latest.json` 检查、下载和安装；不会静默安装或自动重启。
  源码构建、CI artifact、Linux 与 Windows package 不进入该更新通道。

## 2. 前置条件

从仓库根目录安装锁定的前端依赖：

```bash
pnpm --dir apps/desktop install --frozen-lockfile
```

Linux 还需要 Tauri 2 的 WebKitGTK 4.1、AppIndicator、OpenSSL、rsvg 与 xdo 开发包。具体 CI 包名以
`.github/workflows/desktop-package.yml` 为准。

检查内部 component/fixture catalog 时可运行：

```bash
pnpm --dir apps/desktop dev:catalog
```

然后打开 `http://127.0.0.1:1421/catalog.html`。该页面只使用 synthetic fixture，不持有 desktop bridge、bearer、
workspace path 或 network capability，production build gate 会阻止它进入正式 bundle。

## 3. 本机构建

先验证 contract、TypeScript、交互测试和 production frontend：

```bash
pnpm --dir apps/desktop check
node scripts/test-prepare-desktop-sidecar.mjs
```

然后按当前平台构建 dogfood package：

```bash
pnpm --dir apps/desktop package --bundles app   # macOS
pnpm --dir apps/desktop package --bundles deb   # Linux
pnpm --dir apps/desktop package --bundles nsis  # Windows
```

`sidecar:prepare` 会从当前 checkout 使用 locked release profile 构建 `sigil`，再按 Tauri 要求复制为带 Rust target
后缀的 `sigil-runtime`。产品主程序与 sidecar 名称刻意不同，避免 package manager 或签名工具混淆两个 executable。
该命名与复制规则遵循 Tauri 官方的 [Sidecar](https://v2.tauri.app/develop/sidecar/) target-suffix 契约；package 类型与
系统前置条件以 [Distribute](https://v2.tauri.app/distribute/) 文档为基线。

主要输出位置：

- macOS：`target/release/bundle/macos/Sigil.app`
- Linux：`target/release/bundle/deb/*.deb`
- Windows：`target/release/bundle/nsis/*-setup.exe`

## 4. 必做 smoke

1. 启动 package，确认出现 `Sigil` 主窗口，而不是创建窗口前退出。
2. 选择一个含有效 `sigil.toml` 的工作区；确认只显示工作区名，不显示绝对路径或 bearer。
3. 新建会话并发送包含中文、emoji 与多行文本的 prompt；输入法合成期间不得提前提交。
4. 观察 streaming delta、durable assistant message 与 terminal event 只收敛为一份最终回复。
5. 长对话停留底部时应自动跟随；手动向上滚动后，新事件不得抢回滚动位置。
6. 对精确 tool request 分别验证 deny/approve-once；cancel 只显示 cooperative request，不声称撤销已有副作用。
7. 检查 verification receipt、snapshot、changeset 与 failure locator；只允许重跑 server 推荐的 exact check。
8. 关闭工作区和应用，确认 owned stream 先停止、owner pipe 触发 server graceful drain，超时才终止完整进程树。
9. 重新打开同一工作区，确认 recent 记录只用于重新认证启动；debug app 使用当前 `target/debug/sigil`，package 使用同 checkout 的 `sigil-runtime`，durable history 仍由 server catalog 重建。
10. 打开会话行的操作菜单：重命名后重启仍保留名称；删除必须二次确认，置顶会话不可删除，活动 run/verification 结束前删除返回可操作提示。确认删除只移除本地 conversation truth，不声称撤销文件、Shell 或远程副作用。
11. 在 **Appearance**（`Cmd/Ctrl+,`）中切换 Follow system、Light 和 Dark；重启后仍保留手动选择，System 模式会跟随 OS，切换过程不丢 draft、timeline scroll、approval 或 active-run attachment。
12. 分别在 1280、840、839 和 320px 检查顶栏、紧凑对话列表、filter/action popover 与 review surface；所有弹层必须留在可见 viewport 内，320px 下 Browse、workspace、new conversation 和 Appearance 均必须保持可见可操作，不出现 document 横向滚动。
13. 用键盘完成 workspace/session 选择、filter、navigation/review drawer、会话重命名/删除确认、approval 和 theme 切换，确认 Esc、Tab trap、选择后焦点恢复与中文 IME；每个公开 installer candidate 还需在当前 Space 使用 VoiceOver 验证 WebView 内容导航，不得用 hidden-window AX probe 代替。
14. 对公开 macOS beta candidate，在 **Settings → Desktop updates** 检查 signed beta channel；验证“仅检查”不会下载，
    安装完成后不会自动重启，活动任务会阻止重启，签名或 manifest 校验失败时不会替换现有应用。

macOS package 还必须通过：

```bash
test -x target/release/bundle/macos/Sigil.app/Contents/MacOS/sigil-runtime
target/release/bundle/macos/Sigil.app/Contents/MacOS/sigil-runtime --version
codesign --verify --deep --strict --verbose=4 target/release/bundle/macos/Sigil.app
```

启动失败时会在本机写入一份覆盖式、权限受限的错误文件：macOS 位于
`~/Library/Logs/Sigil/startup-error.log`，Linux 位于 `$XDG_STATE_HOME/sigil` 或 `~/.local/state/sigil`，Windows
位于 `%LOCALAPPDATA%\Sigil\logs`。成功启动前会删除上一份错误，文件不会自动上传。

## 5. Crash、restart 与 upgrade 结论

- `sigil serve` crash 会投影为 workspace `crashed/exited`，不会在 renderer 内静默新建第二个 server。用户重新打开
  workspace 时，manager 会重新 canonicalize、校验配置并完成 authenticated readiness。
- 正在运行的 child process 不能跨应用重启续跑。durable event/history 可以恢复，但 interrupted side effect 保持
  interrupted，不会自动重放。
- launcher 对 bootstrap/server-info schema 和 capability 集合做 exact fail-closed 校验。旧 sidecar 与新 shell 不兼容
  时必须拒绝 ready；当前 package 总是同时携带同一 checkout 构建的 shell 与 sidecar。
- SQLite catalog 是可重建 projection，不是升级事实源。schema 不兼容时应从 durable session logs 重建，而不是让
  renderer 迁移数据库。

## 6. 发布门禁

三平台 package CI 成功只证明“能够生成并检查 dogfood artifact”，不等于三个平台都可以公开发布。

macOS beta 已建立公开发行门禁：

- Apple Developer ID 签名、Hardened Runtime、安全时间戳、notarization、staple 与 Gatekeeper 验证；
- Apple Silicon 与 Intel 两套 DMG、SHA-256、`.app.tar.gz` updater archive、archive SHA-256 和 updater signature；
- tag 只创建或更新 draft Release；显式 publish 先验证完整 Desktop 资产并冻结 `latest.json`，再公开
  immutable Release、发布 npm，并部署官网 updater endpoint 与 Homebrew；
- 真实系统 WebView 的可访问性、输入法、剪贴板、滚动、任务执行、crash/restart 和更新安装 smoke。

完整流程见 [`desktop-macos-signing.md`](desktop-macos-signing.md) 与
[`release-process.md`](release-process.md)。CI artifact 的 ad-hoc signature 仍不能替代公开发行身份。

Linux 与 Windows 仍需在公开分发前完成以下门禁：

- Windows Authenticode 证书与 installer/update 签名；
- Linux Tauri/WebKitGTK/GTK 传递 advisory 的重新评估与支持发行版矩阵；
- 各平台安装覆盖、降级、数据保留、回滚与真实系统交互回归。
