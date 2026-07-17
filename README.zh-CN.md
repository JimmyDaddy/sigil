# Sigil

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/sigil-full-staff-glow-dark-mode.svg">
    <img src="assets/logo/sigil-full-staff-glow.svg" alt="Sigil 标志" width="560">
  </picture>
</p>

[English](README.md) | 简体中文

[![CI](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/ci.yml)
[![Pages](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml/badge.svg)](https://github.com/JimmyDaddy/sigil/actions/workflows/pages.yml)

Sigil 是一个 TUI-first coding agent，面向真实仓库工作。对话、修改、审批、diff、诊断和恢复都集中在同一个终端工作区；CLI 只保留轻量自动化入口。

[网站](https://sigil.corerobin.com/zh-CN/) · [文档](https://sigil.corerobin.com/zh-CN/docs/) · [视觉导览](https://sigil.corerobin.com/zh-CN/docs/visual-tour/) · [项目状态](https://sigil.corerobin.com/zh-CN/docs/status/)

Sigil 仍处于早期预览阶段。网站与用户文档跟随 `main`，可能领先于已发布的软件包。受支持的安装和更新方式以[安装指南](docs/zh-CN/installation.md)为准；依赖新功能前请查看[变更记录](docs/zh-CN/changelog.md)。

## 开始使用

安装预览包：

```bash
npm install -g @sigil-ai/sigil@alpha
```

然后进入要处理的仓库：

```bash
cd /path/to/your/project
sigil
```

缺少配置时，Sigil 会打开 Quick Setup。选择 provider 和 model、填写认证信息；如果状态不完整，运行 `sigil doctor`。按照[快速开始](docs/zh-CN/quickstart.md)，可以先完成一次只读任务，再做一个经过检查的小改动。

## 为什么选择 Sigil

- **TUI-first 工作流：** 在终端内同时查看对话、工具活动、修改内容和下一步操作。
- **风险操作先审查：** 写文件、运行命令、访问网络或外部集成前，先检查审批信息和 diff。
- **工作可恢复：** 回到已保存的 session，恢复中断任务时不会静默重跑未完成的工具。
- **模型与工具可组合：** 从支持的 provider 中选择模型，接入 MCP，并按需启用仓库感知能力。

## 文档

- [TUI 用户指南](docs/zh-CN/user-guide.md) — 日常操作、审批、session 与恢复。
- [配置指南](docs/zh-CN/configuration.md) — 常用设置路径和精确字段入口。
- [Provider 指南](docs/zh-CN/providers.md)与 [MCP](docs/zh-CN/mcp.md) — 模型、认证与集成。
- [安全](docs/zh-CN/safety.md)、[权限](docs/zh-CN/permissions-and-sandbox.md)与[隐私](docs/zh-CN/privacy.md) — 决策、限制和数据处理。
- [故障排查](docs/zh-CN/troubleshooting.md) — 从症状到检查与恢复动作。
- [参考](docs/zh-CN/reference.md) — 精确命令、键位、路径和退出行为。

## 项目

欢迎参与贡献；请从 [CONTRIBUTING.md](CONTRIBUTING.md) 和[开发者文档索引](dev/docs/index.md)开始。安全漏洞请按 [SECURITY.md](SECURITY.md) 私下报告。Sigil 使用 [MIT License](LICENSE)。
