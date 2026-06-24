---
name: claude-compat-smoke
description: Claude-compatible workspace skill discovered from .claude/skills.
when-to-use: Use to validate Claude Code-style skill compatibility discovery.
trust: trusted
user-invocable: true
run-as: inline
allowed-tools: [read_file]
---

# Claude Compat Smoke

This compatibility skill should appear in Sigil's Skills browser when Claude compatibility sources are enabled.

When invoked, answer with `SIGIL_CLAUDE_COMPAT_SKILL_OK`.
