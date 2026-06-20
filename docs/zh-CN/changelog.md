# 用户 Changelog

[文档首页](README.md) · [当前支持状态](status.md) · [English](../en/changelog.md)

这一页是用户可读摘要。面向 maintainer 的实现细节仍在 `dev/docs/*` 和 release automation scripts 中。

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
- 从 repository checkout 源码安装。
- Quick Setup 和 `/config`。
- `sigil doctor` 和 `/doctor`。
- 通过 `/task` 使用 durable multi-step tasks，`/plan` 保留给只读 planning prompts。
- 从 append-only logs 恢复 session。
- 文件变更、shell execution、MCP 和 LSP edits 通过 approval 控制。
- DeepSeek、OpenAI-compatible、Anthropic 和 Gemini providers。
- stdio MCP servers。
- 可选 code intelligence。
- Terminal mouse capture 和 OSC52 clipboard 支持。

## Release Archive Notes

Release archive 验证入口：

```bash
scripts/build-release-archive.sh
```

Tagged releases 可以构建 archives 和 checksums。除非后续 release 明确说明，包管理器分发和 self-update 仍是未来 packaging work。

## 更多细节

- 用户支持状态：[status.md](status.md)
- 安装与更新：[installation.md](installation.md)
- 完整配置：[configuration.md](configuration.md)
- 开发实现快照：`dev/docs/current-implementation-notes.md`
