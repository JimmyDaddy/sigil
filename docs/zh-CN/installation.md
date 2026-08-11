<!-- public-doc-role: installation; authority: install-update-uninstall-authority; sections: requirements,supported-install-channels,install-desktop-on-macos,install-tui-with-npm,install-with-homebrew,install-with-cargo,install-from-source,start,install-from-a-release-archive,update,uninstall; cta: start-quickstart -->

# 安装

[文档首页](README.md) · [快速上手](quickstart.md) · [English](../en/installation.md)

本页集中说明 Sigil 的安装方式、更新与卸载命令，以及发布压缩包的使用方法。其他用户指南只链接到这里，不重复这些细节。如果你想按首次使用流程走一遍，先看[快速上手](quickstart.md)。Sigil 仍是早期预览版，配置、插件、高级沙箱行为和自动化接口都可能调整。

官网文档基于 `main` 分支，因此[尚未发布](changelog.md#尚未发布-main)的功能在下一个 beta 版本发布前可能只能从源码体验。带精确版本的 Cargo 示例仍固定到最近一次源码 tag。

## 前置条件

- Desktop：Apple 芯片或 Intel 的 macOS。
- TUI：一个现代终端模拟器。
- 一种安装工具：Desktop DMG、npm、Homebrew，或通过 `rustup` / 系统软件包安装的 Rust 工具链。
- 一份模型服务凭据。首次启动时可以在快速设置中填写。

## 当前安装渠道

| 渠道 | 当前覆盖 | 适合场景 |
| --- | --- | --- |
| Desktop beta | 随 beta 发布 Apple 芯片与 Intel 的签名、公证 DMG。 | 希望在 macOS 使用原生会话、审批与设置工作区。 |
| npm beta | `@sigil-ai/sigil@beta` 会自动选择当前平台对应的软件包。 | 想安装当前公开的跨平台 TUI 预览版。 |
| Homebrew tap | macOS 配方位于 `JimmyDaddy/homebrew-sigil`，安装名是 `sigil-ai`，最终命令仍是 `sigil`。 | 习惯用 Homebrew 管理终端工具。 |
| Cargo git tag | 使用本机 Rust 工具链，从带版本标签的 Git 发布构建。 | 已有 Rust 工具链，或希望从源码构建。 |
| GitHub 发布压缩包 | 提供各平台的压缩包与校验文件。 | 需要手动或离线安装。 |

## 在 macOS 安装 Desktop

打开 [GitHub prerelease 页面](https://github.com/JimmyDaddy/sigil/releases)，下载与你的 Mac 匹配的 DMG：

- Apple 芯片：`Sigil_<version>_aarch64-apple-darwin.dmg`
- Intel：`Sigil_<version>_x86_64-apple-darwin.dmg`

每个 beta 只有在双架构 DMG、对应 SHA-256 和已签名更新包全部到齐后才会公开。DMG 上传前会完成 Developer ID 签名、Apple 公证、staple 与验证。打开 DMG，把 Sigil 拖入“应用程序”，然后正常启动。

## 通过 npm 安装 TUI

npm 包名是 `@sigil-ai/sigil`。安装时会先放置一个很小的 Node.js 启动器，再下载当前平台对应的 Sigil 可执行文件。最终命令仍然是 `sigil`。

```bash
npm install -g @sigil-ai/sigil@beta
```

确认安装：

```bash
sigil --version
sigil doctor
```

首个发布版本不使用未带 scope 的 npm 包名 `sigil`。

## 通过 Homebrew 安装

Homebrew 使用专用配方 `sigil-ai`，避免和其他同名软件混淆。安装后的可执行文件仍然叫 `sigil`。

```bash
brew install JimmyDaddy/sigil/sigil-ai
```

确认安装：

```bash
sigil --version
sigil doctor
```

发布流程会根据 macOS 压缩包生成 `sigil-ai.rb`，并将配方发布到 `JimmyDaddy/homebrew-sigil` tap。

## 通过 Cargo 安装

首个发布版本通过 Git tag 安装，不从 crates.io 分发：

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-beta.4 --locked sigil
```

这会把 `sigil` 可执行文件安装到 Cargo 的二进制目录。macOS 和 Linux 默认为 `~/.cargo/bin`，Windows 默认为 `%USERPROFILE%\.cargo\bin`。

crates.io 上的 `sigil` 包名已被其他项目占用，因此后续需要为 crates.io 分发选择新的软件包名；最终命令仍然可以保持为 `sigil`。

## 从源码安装

如果你已经检出源码，希望从本地构建，请在仓库根目录运行：

```bash
cargo install --path crates/sigil --locked
```

确认安装：

```bash
sigil --version
sigil doctor
```

## 启动

日常使用时，先进入你希望 Sigil 操作的仓库或工作目录，再启动 TUI：

```bash
cd /path/to/workspace
sigil
```

如果没有可用配置，Sigil 会进入快速设置。完成后，`workspace.root = "."` 表示启动 `sigil` 时所在的目录就是当前工作区。

显式子命令只用于自动化、诊断或脚本：

```bash
sigil doctor
sigil run "总结一下当前仓库"
```

在 TUI 内也可以使用 `/doctor`，同一份诊断结果会显示在会话记录中。

## 从发布压缩包安装

能使用包管理器时，请优先选择上面的安装方式。手动安装时，从 [GitHub Releases 页面](https://github.com/JimmyDaddy/sigil/releases)下载当前平台对应的压缩包和校验文件，核对校验和，解压后把 `sigil` 可执行文件放到 `PATH` 中。

压缩包内包含 `sigil` 可执行文件、用户 README、Logo 资源和安装文档。Desktop beta 还会携带已签名的更新包；安装更新仍由用户明确触发。

## 更新

使用原来的安装器更新：

```bash
npm install -g @sigil-ai/sigil@beta
brew upgrade sigil-ai
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-beta.4 --locked sigil --force
cargo install --path crates/sigil --locked --force
```

Desktop 设置页可以检查签名 beta manifest，下载并独立验证匹配当前架构的更新；只有选择“下载并安装”后才会替换应用。重启是另一个明确动作，存在运行中任务时会阻止重启。

TUI 与 CLI 可以使用：

```bash
sigil update check
sigil update apply --yes
```

TUI 对应命令是 `/update check`、`/update refresh` 与 `/update apply`。需要主动切换通道时可在动作后加 `beta` 或 `stable`，例如 `/update check beta`；`current` 会跟随当前已安装的 prerelease 通道。官方独立压缩包只有通过 checksum 与 release 准入后才会被替换；npm、Homebrew、Cargo 和源码安装只会显示对应安装器命令。Sigil 不会在后台静默安装或重启更新。

## 卸载

使用对应卸载命令：

```bash
npm uninstall -g @sigil-ai/sigil
brew uninstall sigil-ai
cargo uninstall sigil
```

把“应用程序”中的 `Sigil.app` 移除即可卸载 Desktop。除非另行删除，已保存的 Sigil 状态会保留。

<!-- public-doc-cta: start-quickstart -->
下一步：[从快速开始入门](quickstart.md)。
