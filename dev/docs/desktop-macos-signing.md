# Sigil Desktop macOS 签名与公证

Sigil Desktop 的公开 macOS 安装包必须完成以下完整链路：

1. 使用 `Developer ID Application` 签名；
2. 对主进程和 `sigil-runtime` sidecar 启用 Hardened Runtime，并包含安全时间戳；
3. 向 Apple Notary Service 异步提交最终 DMG 和用于 updater 的 app archive；
4. 仅在状态为 `Accepted` 后 staple 票据；
5. 对最终 DMG 重新执行签名、架构、bundle identity、stapler 和 Gatekeeper 验证。
6. 发布 workflow 在 arm64 与 Intel runner 上重新下载草稿资产，绑定 tag 版本与
   tagged commit 复验 DMG 和 updater archive 内的 app；updater tar 在解包前拒绝
   绝对路径、`..`、根目录漂移以及逃逸的 symlink/hardlink。

普通 `Desktop Package` GitHub Actions 仍只生成 7 天保留的 ad-hoc dogfood
artifact。发布者的 Developer ID 私钥不上传到 GitHub；公开 macOS 包在受信发布者 Mac
上构建。

## 一次性准备

本机钥匙串必须存在有效的 `Developer ID Application`：

```bash
security find-identity -v -p codesigning
```

使用 Apple ID 的 app-specific password 创建独立的 notarytool profile：

```bash
xcrun notarytool store-credentials "Sigil-Notary" \
  --apple-id "<Apple ID>" \
  --team-id "<10-character Team ID>" \
  --password "<app-specific password>"
```

凭据只写入本机 Keychain，不得把 `.p12`、`.p8`、密码、Base64 私钥或 token
提交到仓库、日志、Issue 或聊天内容。

## 异步构建与提交

工作区必须干净。构建当前 Mac 架构：

```bash
pnpm --dir apps/desktop package:macos:signed
```

公开发布同时构建 Apple Silicon 和 Intel：

```bash
pnpm --dir apps/desktop package:macos:signed -- \
  --target all \
  --tag v0.0.1-beta.1
```

若 Keychain profile 使用了其他名称：

```bash
SIGIL_NOTARY_PROFILE="<profile>" \
  pnpm --dir apps/desktop package:macos:signed -- \
    --target all \
    --tag v0.0.1-beta.1
```

本地打包脚本会把当前 workspace version 与 `git rev-parse --short=12 HEAD`
传给两个 macOS verifier，然后创建不可变的 DMG/app submission bytes 并立即提交；它不调用
`notarytool wait`，也不会在 Apple 队列中长期占用终端。最终 release workflow 从 tag
重新计算同一 identity；签名、公证有效但版本或 commit 不匹配的手工上传资产会被拒绝。

候选写入 `.repo-local-dev/desktop-macos/<version>/<commit>/<timestamp>/`。`.notary/events.jsonl`
是 owner-only、append-only 的 V1 公证账本；每个 submission attempt 都绑定 tag、完整 commit、
Apple Team ID、Keychain profile label、目标架构、精确字节数和 SHA-256。Apple 原始响应先写入
临时文件，`fsync` 并原子 rename 后才追加 `submission_recorded`，不会留下可被误读为成功的
0 字节最终状态文件。若进程在 Apple 已接收上传但本地尚未记录 ID 时退出，下一次 status 会用
提交前 history frontier、文件名和提交时间窗口做唯一恢复；零个或多个候选都 fail closed。

可选的 HTTPS webhook 仅作为外部完成信号，不替代本地账本、Apple 状态复验或 finalize：

```bash
SIGIL_NOTARY_WEBHOOK_URL="https://<relay>/apple-notary/<secret>" \
  pnpm --dir apps/desktop package:macos:signed -- \
    --target all \
    --tag v0.0.1-beta.1
```

## 单次状态检查与离线 finalize

需要查看状态时执行一次：

```bash
scripts/status-desktop-macos-notarization.sh \
  --artifact-dir .repo-local-dev/desktop-macos/0.0.1-beta.1/<commit>/<timestamp>
```

该命令只查询尚未终止的 submission，每个 invocation 最多查询一次，不轮询。只看本地状态：

```bash
scripts/status-desktop-macos-notarization.sh \
  --artifact-dir <artifact-dir> \
  --summary
```

如果首次提交在四项全部发出前中断，先执行一次 status 完成 uncertain attempt 的恢复，再显式
继续尚未开始的项；已有 active attempt 会被跳过，不会重复上传：

```bash
scripts/status-desktop-macos-notarization.sh \
  --artifact-dir <artifact-dir> \
  --submit-pending
```

四项都记录为 `Accepted` 后运行离线 finalizer；finalizer 不连接 Apple：

```bash
scripts/finalize-desktop-macos-local.sh \
  --artifact-dir <artifact-dir>
```

它从不可变 submission bytes 重新生成或复用 stapled 输出，完成 Gatekeeper 验证、双架构
`.app.tar.gz`、updater 签名、交换签名 negative control、checksum 和 `build.txt`，再追加
`release_finalized` 事件绑定最终公开资产的 SHA-256。命令可在任意一步中断后重跑；上传入口还会
重新投影账本并拒绝已 finalize 后变化的字节。

## 发布

先推送版本 tag，让 `Release` workflow 构建一次 TUI/npm/Homebrew 资源，并用
commit-bound candidate manifest 建立 draft Release。随后用唯一上传入口把本机输出目录
中的双架构公开产物上传到同一个 draft：

```bash
scripts/upload-desktop-macos-release.sh \
  --tag v0.0.1-beta.1 \
  --artifact-dir .repo-local-dev/desktop-macos/0.0.1-beta.1/<commit>/<timestamp>
```

该命令会先复验本地与远端 tag/main、精确 SHA 的 CI、`build.txt`、双架构、checksum、
updater 签名、Developer ID、公证和内嵌公钥。远端已有相同字节时保持不动；不同字节
默认停止，只有发布者明确检查后传入 `--replace` 才会删除并替换 draft asset。不能先公开
Release 再补 Desktop 资源。最终手动以 `publish: true` 运行 `Release` workflow；
它会先在 arm64/Intel macOS runner 重新验证两套 DMG 和 updater app 的 Developer
ID、Team ID、Hardened Runtime、版本、commit、stapler 与 Gatekeeper，再在公开 job
复核下载字节仍等于 native verification receipt；随后确认 GitHub immutable releases
已启用，生成并冻结 `latest.json`、公开完整 Release、发布 npm，并通过完整 Pages
artifact 把同一 manifest 部署到：

```text
https://sigil.corerobin.com/updates/beta/latest.json
```

最终 publish 不再重编 TUI，而是按 candidate manifest 复用并核验 tag 阶段的 npm/TUI
产物。最后才同步 Homebrew；主 workflow 使用 `repository_dispatch` 自动启动真实公开安装
smoke，`release.published` 另行覆盖人工发布。完整顺序、必需资源名与失败恢复方式见
[`release-process.md`](release-process.md)。

## 缺失 submission 与显式重提

如果 Apple 对已记录 ID 返回 `Submission does not exist or does not belong to your team`，
账本保留原 attempt 和错误，不会静默套用同名历史记录。先确认 Keychain profile 仍属于账本中的
Team；只有确认旧 attempt 已不可恢复后，才能显式 orphan 并重提精确相同的不可变字节：

```bash
scripts/status-desktop-macos-notarization.sh \
  --artifact-dir <artifact-dir> \
  --resubmit x86_64_dmg
```

可用 key 为 `arm64_dmg`、`arm64_app`、`x86_64_dmg`、`x86_64_app`。重提会追加
`submission_orphaned` 和新的 attempt，不覆盖历史。不要用同名、同版本或更早 Accepted 的
submission 代替当前 SHA-256。

Apple 官方要求 Developer ID、Hardened Runtime、安全时间戳与 notarization；
Tauri 使用 `APPLE_SIGNING_IDENTITY` 选择本机钥匙串身份：

- [Apple: Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Apple: Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
- [Tauri: macOS Code Signing](https://v2.tauri.app/distribute/sign/macos/)
- [Tauri: Updater signing](https://v2.tauri.app/plugin/updater/#signing-updates)
