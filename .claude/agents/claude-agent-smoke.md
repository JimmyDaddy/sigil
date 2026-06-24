---
name: claude-agent-smoke
description: Claude-compatible agent discovered as a child-session skill.
trust: trusted
user-invocable: true
disable-model-invocation: true
tools: read_file, grep
paths:
  - .claude/**
---

# Claude Agent Smoke

This compatibility agent should be discovered from `.claude/agents`.

When invoked, answer with `SIGIL_CLAUDE_AGENT_OK`.
