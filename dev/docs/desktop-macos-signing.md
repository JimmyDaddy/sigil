# Sigil Desktop macOS 签名与公证

Sigil Desktop 的公开 macOS 安装包必须完成以下完整链路：

1. 使用 `Developer ID Application` 签名；
2. 对主进程和 `sigil-runtime` sidecar 启用 Hardened Runtime，并包含安全时间戳；
3. 向 Apple Notary Service 提交最终 DMG；
4. 仅在状态为 `Accepted` 后 staple 票据；
5. 对最终 DMG 重新执行签名、架构、bundle identity、stapler 和 Gatekeeper 验证。

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
pnpm --dir apps/desktop package:macos:signed -- --target all
```

若 Keychain profile 使用了其他名称：

```bash
SIGIL_NOTARY_PROFILE="<profile>" \
  pnpm --dir apps/desktop package:macos:signed -- --target all
```

产物写入 `.repo-local-dev/desktop-macos/<version>/<commit>/<timestamp>/`，包括
已公证并 stapled 的 DMG、SHA-256 和不含凭据的构建元数据。

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
