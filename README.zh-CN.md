<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/sigil-lockup-dark-mode.svg">
    <img src="assets/logo/sigil-lockup.svg" alt="Sigil" width="520">
  </picture>
</p>

<p align="center"><strong>修改可审查，任务可恢复，桌面端或终端都能继续。</strong></p>
<p align="center">同时提供 Desktop 与 TUI 一等体验的 Rust coding agent。</p>

<p align="center">
  <a href="https://github.com/JimmyDaddy/sigil/releases"><img src="https://img.shields.io/github/v/release/JimmyDaddy/sigil?include_prereleases&amp;sort=semver&amp;style=flat-square&amp;color=C85B4B" alt="当前版本"></a>
  <a href="https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/JimmyDaddy/sigil/ci.yml?branch=main&amp;style=flat-square&amp;label=build" alt="构建状态"></a>
  <a href="https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml"><img src="https://img.shields.io/github/actions/workflow/status/JimmyDaddy/sigil/pages.yml?branch=main&amp;style=flat-square&amp;label=docs" alt="文档状态"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/JimmyDaddy/sigil?style=flat-square&amp;color=242932" alt="MIT License"></a>
</p>

<p align="center">
  <a href="https://sigil.corerobin.com/zh-CN/">网站</a> ·
  <a href="https://sigil.corerobin.com/zh-CN/docs/">文档</a> ·
  <a href="docs/zh-CN/quickstart.md">快速开始</a> ·
  <a href="https://sigil.corerobin.com/zh-CN/docs/visual-tour/">视觉导览</a>
</p>

<p align="center"><a href="README.md">English</a> · 简体中文</p>

<p align="center">
  <a href="https://sigil.corerobin.com/zh-CN/#demo">
    <img src="assets/demo/sigil-desktop-demo-poster.png" alt="包含计划、工具活动与审批的 Sigil Desktop 工作台" width="900">
  </a>
</p>

<p align="center"><a href="https://github.com/JimmyDaddy/sigil/releases">查看 macOS 预发布版本</a> · <a href="https://sigil.corerobin.com/zh-CN/#demo">观看 Desktop + TUI Demo</a> · <a href="docs/zh-CN/changelog.md">变更记录</a></p>

> [!NOTE]
> Sigil 仍处于早期预览阶段。网站与用户文档跟随 `main`，可能领先于已发布的软件包。依赖新功能前，请先查看[安装指南](docs/zh-CN/installation.md)与[变更记录](docs/zh-CN/changelog.md)。

## 为什么选择 Sigil

| 工作不脱离上下文 | 风险始终可控 |
| --- | --- |
| **桌面端或终端**<br>选择适合当前工作的表面，同时复用相同的对话、任务、审批与恢复语义。 | **风险操作先审查**<br>写文件、运行命令、访问网络或外部集成前，先检查审批信息和 diff。 |
| **任务可恢复**<br>回到已保存的 session，恢复中断任务时不会静默重跑未完成的工具。 | **模型与工具自由组合**<br>从支持的 provider 中选择模型，接入 MCP，并按需启用仓库感知能力。 |
| **大输出仍可检查**<br>对话只展示有界摘要，policy-safe 的工具输出则保存在 session artifact 中，可按页或按字面量精确读取。 | **上下文缓存稳定**<br>历史工具输出先确定性老化，再进入语义压缩；长会话可以降低 token 压力，而不重写仍在使用的缓存前缀。 |
| **命令生命周期明确**<br>有限检查以前台方式运行并实时展示进度；常驻或交互任务使用明确的 terminal task 与事件驱动等待。 | **审批绑定真实副作用**<br>同一份不可变权限计划贯穿命令解析、目标、隔离、策略、审批、审计和执行，并由 Desktop 与 TUI 共用。 |

## 一分钟内开始

```bash
npm install -g @sigil-ai/sigil@alpha
cd /path/to/your/project
sigil
```

缺少配置时，Sigil 会打开 Quick Setup。选择 provider 和 model、填写认证信息；如果状态不完整，运行 `sigil doctor`。按照[快速开始](docs/zh-CN/quickstart.md)，可以从第一次只读任务走到一个经过检查的小改动。

希望使用原生应用？[GitHub prerelease](https://github.com/JimmyDaddy/sigil/releases) 提供面向 Apple 芯片与 Intel Mac、已签名并完成 Apple 公证的 DMG。精确资源名和更新方式见[安装指南](docs/zh-CN/installation.md)。

只有当某个 release 为自身 binary 携带 exact-route qualified manifest 时，Quick Setup 才会为匹配的新安装启用自动 Task routing 和主动只读 Explore 子智能体。其他 route、缺少 sidecar 的 release，以及所有已有配置都继续保持保守的 `manual + explicit_request_only`。这只改变编排方式，不会授予文件、Shell、网络、MCP、外部目录或 merge 权限。见[高级配置](docs/zh-CN/advanced-configuration.md#任务规划)。

## 深入了解

| 指南 | 内容 |
| --- | --- |
| [界面导览](docs/zh-CN/visual-tour.md)与 [TUI 用户指南](docs/zh-CN/user-guide.md) | Desktop 工作区，以及 TUI 日常操作、审批、session 与恢复。 |
| [配置指南](docs/zh-CN/configuration.md) | 常用设置路径和精确字段。 |
| [Provider 指南](docs/zh-CN/providers.md)与 [MCP](docs/zh-CN/mcp.md) | 模型、认证与集成。 |
| [安全](docs/zh-CN/safety.md)、[权限](docs/zh-CN/permissions-and-sandbox.md)与[隐私](docs/zh-CN/privacy.md) | 决策、限制和数据处理。 |
| [故障排查](docs/zh-CN/troubleshooting.md) | 从症状到检查与恢复动作。 |
| [参考](docs/zh-CN/reference.md) | 命令、键位、路径和退出行为。 |

## 项目

[项目状态](https://sigil.corerobin.com/zh-CN/docs/status/) · [参与贡献](CONTRIBUTING.md) · [开发者文档](dev/docs/index.md) · [安全报告](SECURITY.md) · [MIT License](LICENSE)
