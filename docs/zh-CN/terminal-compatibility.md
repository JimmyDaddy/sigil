<!-- public-doc-role: terminal-compatibility; authority: terminal-smoke-authority; sections: baseline,attention-notification-smoke,mouse-smoke,text-selection-and-copy,image-paste-smoke,tmux-screen-ssh-and-wsl,result-template; cta: open-troubleshooting -->

# 终端兼容性检查清单

[文档首页](README.md) · [故障排查](troubleshooting.md) · [English](../en/terminal-compatibility.md)

终端、multiplexer、远程 shell 和桌面设置可能在 Sigil 进程外阻止键位、鼠标、剪贴板序列、图片或通知。先运行 `sigil doctor`；需要保存本机报告时使用 `scripts/tui-mouse-smoke.sh`。

## 基线

1. 通过 Doctor 或[配置指南](configuration.md#解析顺序)找到当前用户 `sigil.toml`。
2. 除非下面测试失败，否则保持 `keyboard_enhancement = "auto"`、`mouse_capture = true`、`osc52_clipboard = true` 和 `scroll_sensitivity = 3`；这些 `[terminal]` 字段需要在 TOML 中修改，然后重启 Sigil。
3. 不需要失焦提示时保持 notification 关闭；notification 字段也可以在 `/config` → **Terminal** 中修改。
4. 测试可选功能前，先确认普通文本输入、transcript 滚动、`Esc` 和 `Ctrl-C` 正常。

Windows 上分别运行无害的 `Write-Output 'hello'` 和 `exit 7`；活动应显示实际 shell、UTF-8 输出和 exit code。Local execution 不是 OS sandbox。

## Attention Notification Smoke

临时开启 notification，并把长任务阈值设为 `1000` ms。启动一个超过一秒的 run，移出焦点，预期只收到一次固定完成提示。审批和 MCP 等待输入可以不受长任务阈值限制。如果 tmux 或 screen 显示控制文本或忽略提示，尝试 `bell` 或关闭 notification。

真实 binary 的 default-off 与 BEL 检查使用 `scripts/tui-attention-signals-pty-acceptance.py`。

## 鼠标 Smoke

修改 mouse capture 后重启，再检查：

1. 点击输入框并输入；
2. 打开 `/` 并点击 command candidate；
3. 滚动 transcript；
4. 修改一个 `/config` 字段；
5. 打开 `/resume`、选择一行，并分别用右键和 `Ctrl-O` 打开 Session Actions；
6. 使用审批中的 file、diff、allow 和 deny 控件。

点击与滚轮应只影响聚焦界面；键盘操作始终可用。

## 文本选择与复制

在 transcript 中分别拖选单行、多行和宽字符文本，按 `Ctrl-C`，再粘贴到其他位置。确认存在选区时 `Ctrl-L` 也复制该选区；然后点击 transcript 选区之外清除选择，再按 `Ctrl-L`，确认复制最新 assistant 回复。所有复制内容都不应包含右侧信息栏。OSC52 被关闭或拦截时，Sigil 会报告 clipboard 不可用。

## 图片粘贴 Smoke

配置识别为支持图片的 OpenAI Responses、Anthropic 或 Gemini model 后：

1. 复制 PNG，在空闲输入框按 `Ctrl-V`；
2. 确认 metadata chip 出现且不显示本机路径；
3. 选中并删除 chip；
4. 粘贴本机 PNG、JPEG 或 WebP 路径；
5. 提交纯图片或图文 turn。

不支持的 model 必须保留 draft，并在发送前拒绝图片。远程层可能无法暴露 host 图片剪贴板；此时粘贴本机路径。

## tmux、screen、SSH 与 WSL

在每层中重复 `/doctor`、鼠标和复制检查。键位异常时设置 `keyboard_enhancement = "off"` 并重启；鼠标异常时设置 `mouse_capture = false` 并重启；复制被阻止或出现可见控制文本时设置 `osc52_clipboard = false`。

## 结果模板

```text
Terminal / TERM:
Layers: none / tmux / screen / SSH / WSL
keyboard_enhancement / mouse_capture / osc52_clipboard:
notifications method / threshold:
Mouse smoke:
Selection copy / latest-response copy:
Image paste:
Notes:
```

<!-- public-doc-cta: open-troubleshooting -->
下一步：[继续排障](troubleshooting.md)。
