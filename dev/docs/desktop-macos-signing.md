# Sigil Desktop macOS 签名与公证

Sigil Desktop 的公开 macOS 安装包必须完成以下完整链路：

1. 使用 `Developer ID Application` 签名；
2. 对主进程和 `sigil-runtime` sidecar 启用 Hardened Runtime，并包含安全时间戳；
3. 向 Apple Notary Service 提交最终 DMG；
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

## 构建

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
传给两个 macOS verifier。最终 release workflow 从 tag 重新计算同一 identity；
签名、公证有效但版本或 commit 不匹配的手工上传资产会被拒绝。

产物写入 `.repo-local-dev/desktop-macos/<version>/<commit>/<timestamp>/`，包括
已公证并 stapled 的双架构 DMG、对应 SHA-256、双架构 `.app.tar.gz` 更新包、
更新包 SHA-256 / `.sig`，以及不含凭据的构建元数据。打包脚本会先确认发布者
公钥等于 `tauri.conf.json` 内嵌公钥，再对每个更新包做真实 Minisign 验签；
双架构发布还必须证明交换两份 `.sig` 后验签失败。

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

## 中断恢复

提交响应会保存在 DMG 旁的 `.notary-submit.json`。若等待 Apple 时终端中断，
使用其中的 `id` 恢复，不能重新上传或绕过公证：

```bash
scripts/notarize-desktop-macos.sh \
  --dmg "<DMG path>" \
  --expected-arch arm64 \
  --team-id "<Team ID>" \
  --submission-id "<submission UUID>"
```

Apple 官方要求 Developer ID、Hardened Runtime、安全时间戳与 notarization；
Tauri 使用 `APPLE_SIGNING_IDENTITY` 选择本机钥匙串身份：

- [Apple: Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Apple: Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
- [Tauri: macOS Code Signing](https://v2.tauri.app/distribute/sign/macos/)
- [Tauri: Updater signing](https://v2.tauri.app/plugin/updater/#signing-updates)
