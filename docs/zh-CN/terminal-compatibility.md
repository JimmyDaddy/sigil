# Terminal 兼容性检查清单

[English](../en/terminal-compatibility.md)

这份清单用于验证 Sigil 在真实终端里的 mouse capture 和 OSC52 剪贴板行为。它保留为人工 smoke checklist，因为 terminal multiplexer、远程 shell 和桌面终端偏好设置都可能在 Sigil 进程外拦截这些能力。

先运行诊断：

```bash
sigil doctor
```

在 TUI 里可以运行 `/doctor`，同一份 terminal 检查会渲染到 transcript。报告会读取 `[terminal].mouse_capture`、`[terminal].osc52_clipboard`、`[terminal].scroll_sensitivity`、`TERM`、常见终端 profile 变量、tmux/screen、SSH、WSL 和剪贴板桥接风险。

如果要复用一套本地流程来采集 `/doctor`、启动真实 TUI、逐项记录 pass/fail/skip，并生成 Markdown 报告，可以运行：

```bash
scripts/tui-mouse-smoke.sh
```

## 基线

1. 确认 `/doctor` 输出 `terminal`、`terminal:config`、`terminal:mouse` 和 `terminal:clipboard`。
2. 打开 `/config`，查看 `Terminal` 区块。
3. 除非终端或 multiplexer 不能正确处理 mouse mode，否则保持 `mouse_capture = true`。
4. 除非复制序列被拦截或被可见打印出来，否则保持 `osc52_clipboard = true`。
5. 除非 transcript 和 approval diff 的滚轮速度过快或过慢，否则保持 `scroll_sensitivity = 3`。

## 鼠标 Smoke

在 iTerm2、Terminal.app、WezTerm、kitty 和你需要支持的终端 profile 里分别检查：

1. 点击 composer，输入一个短 prompt。
2. 打开 `/`，点击一个 slash command 候选，然后按 `Esc`。
3. 用鼠标滚轮滚动 transcript。
4. 打开 `/config`，点击 section，点击 boolean 字段，确认焦点变化。
5. 有历史 session 时打开 `/resume`，单击候选表示选中，再单击表示确认。
6. 出现 approval modal 时，点击 file rows、diff controls 和 allow/deny actions。

预期结果：点击和滚轮只影响当前聚焦的 TUI 表面。每一步键盘操作仍然可用。

## 文本选择和复制

1. 在可见 transcript 文本上拖拽选择。
2. 至少覆盖一次短单行选择和一次多行选择。
3. 如果 transcript 里有 CJK 或宽字符，也要覆盖。
4. 按 `Ctrl-C`。
5. 粘贴到另一个应用或 shell prompt。

预期结果：OSC52 开启且终端接受序列时，Sigil 显示 `copied ...`。如果配置里关闭 OSC52，Sigil 显示 `clipboard unavailable: OSC52 disabled`。

## tmux、screen、SSH 和 WSL

这些层通常需要显式配置剪贴板或鼠标 pass-through：

1. 在对应层里运行 `/doctor`，查看 `terminal:mouse` / `terminal:clipboard`。
2. 在对应层里重复鼠标 smoke。
3. 重复复制检查，并粘贴到该层外部。
4. 如果鼠标事件不正常，设置 `[terminal].mouse_capture = false`，并重启 TUI。
5. 如果复制被拦截或控制序列可见，设置 `[terminal].osc52_clipboard = false`。

`mouse_capture` 下一次启动生效。`osc52_clipboard` 每次复制时都会读取当前配置。`scroll_sensitivity` 在保存配置并重新加载后生效。

## 结果模板

```text
Terminal:
TERM:
Layers: none / tmux / screen / SSH / WSL
mouse_capture:
osc52_clipboard:
scroll_sensitivity:
Doctor terminal status:
Mouse smoke:
Text selection:
OSC52 copy:
Notes:
```
