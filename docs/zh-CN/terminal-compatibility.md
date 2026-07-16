# Terminal 兼容性检查清单

[文档首页](README.md) · [排障](troubleshooting.md) · [English](../en/terminal-compatibility.md)

这份清单用于验证 Sigil 在真实终端里的 mouse capture、OSC52 剪贴板行为和可选 attention notification。部分检查仍需人工完成，因为 terminal multiplexer、远程 shell 和桌面终端偏好设置都可能在 Sigil 进程外拦截这些能力。

先运行诊断：

```bash
sigil doctor
```

在 TUI 里可以运行 `/doctor`，同一份 terminal 检查会渲染到 transcript。报告会在 mouse、clipboard、scroll、profile、tmux/screen、SSH、WSL 和剪贴板桥接事实之外，显示配置的 notification 开关、method 和 threshold；不会打印 notification payload 或原始环境值。

如果要复用一套本地流程来采集 `/doctor`、启动真实 TUI、逐项记录 pass/fail/skip，并生成 Markdown 报告，可以运行：

```bash
scripts/tui-mouse-smoke.sh
```

## 基线

1. 确认 `/doctor` 输出 `terminal`、`terminal:config`、`terminal:mouse` 和 `terminal:clipboard`。
2. 打开 `/config`，查看 `Terminal` 区块。
3. 默认保持 `keyboard_enhancement = "auto"`；只有已确认某个 profile 稳定时才强制 `on`，遇到异常终端层时强制 `off`。
4. 默认保持 `mouse_capture = true` 以启用鼠标支持；如果终端或 multiplexer 不能稳定处理 mouse mode，再显式设为 `false`。
5. 除非复制序列被拦截或被可见打印出来，否则保持 `osc52_clipboard = true`。
6. 除非 transcript 和 approval diff 的滚轮速度过快或过慢，否则保持 `scroll_sensitivity = 3`。
7. 只有后台或长任务确实需要失焦提示时才开启 attention notification。优先使用 `method = "auto"`；只有在测试已知 terminal profile 时才显式使用 `bell`、`osc9` 或 `osc777`。

## Attention Notification Smoke

在 `/config` → `Terminal` 中开启通知，并临时把长任务阈值设为 `1000` ms。启动一个超过一秒的 run，然后把焦点移出终端。

- 预期：完成后只出现一次固定通知。审批和 MCP 等待输入不使用长任务阈值。
- Sigil 聚焦时：能可靠上报 focus 的终端会抑制通知。如果从未收到 focus event，Sigil 不会假装焦点检测可靠。
- tmux/screen：OSC method 会使用 multiplexer pass-through。如果出现可见控制文本或通知被忽略，改用 `bell` 或关闭该能力。
- 隐私：通知不包含 prompt、reply、路径、tool/MCP name、arguments、错误详情、provider 或 session id。

如需用真实 binary 确定性验证 default-off 与 explicit BEL 字节，运行：

```bash
scripts/tui-attention-signals-pty-acceptance.py
```

## 鼠标 Smoke

先确认 `/doctor` 报告 `mouse_capture=true`（或者移除显式的 `false` 覆盖）并重启 TUI，再在 iTerm2、Terminal.app、WezTerm、kitty 和你需要支持的终端 profile 里分别检查：

1. 点击 composer，输入一个短 prompt。
2. 打开 `/`，点击一个 slash command 候选，然后按 `Esc`。
3. 用鼠标滚轮滚动 transcript。
4. 打开 `/config`，点击 section，点击 boolean 字段，确认焦点变化。
5. 有历史 session 时打开 `/resume`。单击候选表示选中，再右键单击打开 Session Actions；关闭弹窗、重新选中后按 `Ctrl-O`，确认键盘路径会打开同一个独占弹窗。
6. 在 Session Actions 中执行 safe export 之类的无破坏操作，确认弹窗关闭前输入不会进入 composer。
7. 出现 approval modal 时，点击 file rows、diff controls 和 allow/deny actions。

预期结果：点击和滚轮只影响当前聚焦的 TUI 表面。每一步键盘操作仍然可用。

## 文本选择和复制

1. 在可见 transcript 文本上拖拽选择。
2. 至少覆盖一次短单行选择和一次多行选择。
3. 如果 transcript 里有 CJK 或宽字符，也要覆盖。
4. 按 `Ctrl-C`。
5. 粘贴到另一个应用或 shell prompt。

预期结果：OSC52 开启且终端接受序列时，Sigil 显示 `copied ...`。如果配置里关闭 OSC52，Sigil 显示 `clipboard unavailable: OSC52 disabled`。

## 图片粘贴 Smoke

图片输入与 OSC52 文本选择复制相互独立。先配置一个明确支持的 OpenAI Responses、Anthropic 或 Gemini model，再从空闲 Build composer 执行：

1. 将 PNG 图片放入系统剪贴板，按 `Ctrl-V`。
2. 确认 composer 上方出现 metadata chip，且不显示本地路径。
3. 用 `Up` 选中 chip；多张图时用 `Left/Right` 移动；用 `Backspace` 或 `Delete` 删除。
4. 粘贴本地 PNG、JPEG 或 WebP 路径，确认它变成 chip，而不是 prompt 文本。
5. 提交只含图片的 turn，或加入文本后提交；不支持的 model id 必须保留草稿，并在 provider transport 前失败。

tmux、screen、SSH、WSL 与远程终端应用可能无法向 Sigil 暴露 host 系统图片剪贴板；这类环境请改为粘贴已准入的本地文件路径。OSC52 只是 Sigil 用于向外复制文本选区的机制；开启它不代表系统图片剪贴板可用。

## tmux、screen、SSH 和 WSL

这些层通常需要显式配置剪贴板或鼠标 pass-through：

1. 在对应层里运行 `/doctor`，查看 `terminal:mouse` / `terminal:clipboard`。
2. 在对应层里重复鼠标 smoke。
3. 重复复制检查，并粘贴到该层外部。
4. 如果启动后键盘输入像是卡住，设置 `[terminal].keyboard_enhancement = "off"`，并重启 TUI。
5. 如果鼠标事件不正常或滚动很重，设置 `[terminal].mouse_capture = false`，并重启 TUI。
6. 如果复制被拦截或控制序列可见，设置 `[terminal].osc52_clipboard = false`。

`keyboard_enhancement` 在下一次启动时解析。`mouse_capture` 下一次启动生效。`osc52_clipboard` 每次复制时都会读取当前配置。`scroll_sensitivity` 在保存配置并重新加载后生效。

## 结果模板

```text
Terminal:
TERM:
Layers: none / tmux / screen / SSH / WSL
keyboard_enhancement:
mouse_capture:
osc52_clipboard:
scroll_sensitivity:
notifications enabled / method / threshold:
Long-run notification:
Focused suppression:
Doctor terminal status:
Mouse smoke:
Text selection:
OSC52 copy:
图片粘贴:
Notes:
```
