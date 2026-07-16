# Alpha Dogfood Campaign

Alpha dogfood 是发布证据，不是遥测。Sigil 不会上传 campaign artifact、session history、prompt、tool material、credential 或 workspace content。

## 公开分发 smoke

发布 alpha 后，手动运行 `Published Distribution Smoke`。该 workflow 会在 Linux、Windows、macOS ARM 和 macOS Intel 上安装当前 npm alpha，在两种 macOS 架构上安装公开 Homebrew formula，并验证 GitHub Release checksum 与 artifact attestation。

这个 workflow 是只读的：不能发布 package、创建 release、移动 npm dist-tag 或更新 tap。

## 离线 production-binary campaign

显式选择 standalone native Sigil executable，例如 Release archive 或 Homebrew 安装中的实体 binary：

```bash
python3 scripts/alpha-dogfood-campaign.py \
  --binary /path/to/sigil \
  --expected-version 0.0.1-alpha.4 \
  --expected-commit f4e6c5aeea86b3283988efe20db44a0f97454f97
```

已有 binary digest 时再传 `--expected-sha256`。创建任何 case state 之前，Runner 会把 executable 复制到私有临时目录，对冻结副本计算 SHA-256 并检查 `sigil --version`。所有 case 只执行同一份已准入副本，因此 campaign 期间替换源 binary 不会让 evidence 漂移；Runner 也不会隐式构建 binary。

Runner 有意不接收任何 script launcher，包括 npm JavaScript launcher，因为冻结 wrapper 并不能冻结它委托的 binary。准入只接受 Mach-O、ELF 或 PE executable。公开 npm 安装与 launcher 执行由 Public Distribution Smoke 覆盖；只有还需要本地离线 campaign 时，才显式选择已安装 platform package 内的 native binary。

默认 campaign 通过真实 headless 或 PTY 产品入口运行 Context V1、Web V1、feedback、terminal attention 和 image-input acceptance。每个 case 使用独立 HOME、XDG、state、cache 和 temp root。Runner 不继承 provider credential 或 Sigil config override，ambient proxy route 指向关闭的 loopback endpoint，并且每个 harness 都只配置和检查 case-owned loopback service。Feedback case 会记录 loopback request 数量，并拒绝 provider generation request。

这不是 OS-level socket sandbox。“离线”结论只覆盖这些已审查 case definition、它们的 loopback endpoint assertion，以及 ambient credential/config 不可见的边界。

在非 macOS 或 headless host 上，必须显式跳过图片剪贴板：

```bash
python3 scripts/alpha-dogfood-campaign.py \
  --binary /path/to/sigil \
  --skip-clipboard
```

重复使用 `--case` 可以只运行部分 case；`--list-cases` 会打印稳定 case id。

## Stateful TUI campaign

默认离线 campaign 之后，再单独运行 stateful campaign。它同时要求精确 native binary 和 checksum-pinned DeepSeek V4 Flash `tokenizer.json`。若尚未安装，先运行下面的命令；命令会打印验证后的路径：

```bash
sigil tokenizer install deepseek-v4-flash

python3 scripts/tui-stateful-pty-acceptance.py \
  --binary /path/to/sigil \
  --tokenizer-json /path/printed/by/tokenizer-install/tokenizer.json \
  --expected-version 0.0.1-alpha.4 \
  --expected-commit f4e6c5aeea86b3283988efe20db44a0f97454f97 \
  --expected-binary-sha256 <sha256>
```

campaign 会把两个输入冻结到 case-owned storage，并围绕同一组 durable session 运行三个真实 TUI process：

1. loopback provider 创建四个 finalized turn，其中包含一次受控 `write_file` mutation，以及曾经可能触发重复 reply 的 facts-before-final continuation；
2. 关闭 loopback server 后，以默认 transport config 和关闭的 ambient proxy 恢复 source session，在本地完成 compaction admission/apply，通过 Ctrl-R reverse-diff modal 恢复受控文件，再由 modal 的 `F` fork，且不得改变已恢复文件；
3. 新进程先从 source session 启动，再通过可见的 `/resume` selector 切换到唯一 non-current fork。

通过条件包括：最终 reply 在两次重建后的 VT screen 上都恰好出现一次，在 source/fork stream 中也都只有一条结构化 final-answer entry；同时必须恰好存在一个 `compaction_applied_v2`、一个 `checkpoint_restored`、一个 `conversation_forked`，fork 和 resume 前后文件 hash 不变。local compaction admission 有意不使用 custom provider route。

安全的 `manifest.json` 只包含公开 binary/tokenizer identity、计数、布尔结果、耗时，以及相对 evidence path/checksum。字节完全一致的 source/fork JSONL 与原始 PTY log 只保留在被忽略的本地输出中，可用于独立复算，且不会上传。仓库内输出若未被 Git ignore 会在执行前被拒绝；显式选择的仓库外路径会标记为 local-only。CI 只运行 parser、admission、process cleanup、durable structure 和 manifest privacy contract test；真实 release-binary campaign 因依赖已安装 tokenizer artifact，继续作为显式本地发布检查。

## Evidence

默认输出目录是 `.repo-local-dev/dogfood/offline-<timestamp>`。聚合的 `manifest.json`、`manifest.sha256` 和 `summary.md` 只包含时间戳、build identity、binary digest、case 状态、耗时和相对 evidence path。原始 case artifact 只保留在被忽略的本地输出目录中用于排障，不会自动上传。仓库外的自定义目录会记录为显式选择的本地输出；仓库内的自定义目录若未被 Git ignore，Runner 会拒绝执行。

外层 deadline 触发时，Runner 会先 interrupt case harness，让它的 cleanup handler 有机会恢复 terminal 或 clipboard 状态，再终止仍然存活的 detached descendant。

Loopback 通过只能证明本地 application wiring 与产品交互，不能证明远端 provider 质量或计费行为。Provider-backed campaign 继续显式执行，并使用[真实模型评测](model-evaluation.zh-CN.md)中的 cost/deadline 控制。
