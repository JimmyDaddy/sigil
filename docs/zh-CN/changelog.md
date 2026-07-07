# 用户 Changelog

[文档首页](README.md) · [当前支持状态](status.md) · [English](../en/changelog.md)

这一页是用户可读摘要。面向 maintainer 的实现细节仍在 `dev/docs/*` 和 release automation scripts 中。首个公开版本是 early preview：核心 TUI 工作流应该可用，但配置、插件 API、高级 sandbox 行为和自动化入口暂不承诺稳定兼容。

## 当前文档更新

用户文档已按任务路径重组：

- Quickstart 覆盖首次运行 setup。
- Workflows 和 Cookbook 覆盖实用 prompt。
- Safety 和 privacy 页面覆盖 permissions、secrets、MCP 和 session logs。
- Troubleshooting 增加 decision-tree 入口。
- Reference 页面集中列出 commands、keys、paths 和 environment variables。
- GitHub Pages site 提供 documentation hub 和生成的 docs pages。

## 当前能力快照

当前文档覆盖这些用户可见能力：

- 通过 `sigil` 使用 TUI-first workflow。
- npm、Homebrew tap、Cargo git-tag、GitHub release archive 和 checkout 安装路径。
- Quick Setup 和 `/config`。
- `sigil doctor` 和 `/doctor`。
- 通过 `/task` 使用 durable multi-step tasks，`/plan` 保持只读，直到用户显式接受 plan-to-task handoff。
- 从 append-only logs 恢复 session。
- 文件变更、shell execution、MCP 和 LSP edits 通过 approval 控制。
- DeepSeek、OpenAI-compatible、Anthropic 和 Gemini providers。
- stdio MCP servers。
- 可选 code intelligence。
- Terminal mouse capture 和 OSC52 clipboard 支持。
- 任务完成 verification 状态和显式用户批准的 checks。
- 支持的本地执行后端已有 core sandbox receipt，平台差异另见安全与配置文档。

## Release Archive Notes

Release archive 验证入口：

```bash
scripts/build-release-archive.sh
```

Tagged releases 会构建 archives、checksums、GitHub provenance attestations、用于 Homebrew tap 的 `sigil-ai.rb`，以及从 archives 派生的 npm package tarballs。除非后续 release 明确说明，self-update 仍是未来 packaging work。

## 更多细节

- 用户支持状态：[status.md](status.md)
- 安装与更新：[installation.md](installation.md)
- 完整配置：[configuration.md](configuration.md)
- 开发架构与 RFC 细节：`dev/docs/sigil-rust-agent-core-technical-solution.md` 和 `dev/docs/rfcs/`
