# RFC-0030 Alpha Feedback and Supportability V1

状态：implemented / R30.0-R30.5 complete

创建日期：2026-07-16

基线：

- Depends on: [RFC-0001 Durable Event Stream and Event Taxonomy](0001-durable-event-stream-and-event-taxonomy.md)
- Depends on: [RFC-0026 Stable Machine Protocol and Real Local Serve](0026-stable-machine-protocol-and-real-serve.md)
- Depends on: [RFC-0027 Local Session Lifecycle V1](0027-local-session-lifecycle-v1.md)
- Release baseline: `v0.0.1-alpha.3` / `92eedf8f982a9450750fa8694fc49eb87c647f75`

## 1. Summary

本 RFC 为 alpha 用户建立一条可操作、默认私密、TUI-first 的问题反馈闭环。它在现有离线 `doctor` 检查之上增加版本化、脱敏的 JSON 投影；在 TUI 中提供 `/feedback` 预览与显式本地导出；在 GitHub 中提供结构化 issue forms；并持续验证 npm、GitHub Release 与 Homebrew 的公开安装产物。

V1 不自动上传诊断、不把会话内容写入支持包、不在 durable session/control stream 中新增 support 事件，也不建立遥测或崩溃收集服务。用户必须先看到将要导出的内容类别，再按 Enter 写入本地 cache；之后由用户自行检查并决定是否附加到 GitHub issue。

## 2. Why Now

`v0.0.1-alpha.3` 已经通过四平台构建、npm、GitHub Release 和 Homebrew 发布验证，但当前反馈链路仍有三个缺口：

1. `sigil doctor` 只有 human-readable 文本，用户与维护者无法依赖稳定结构收集安装、配置和本地能力证据。
2. TUI 没有一等反馈入口，用户需要离开第一产品表面并手工拼接版本、环境和问题上下文。
3. 发布验证集中在候选时点，不能持续发现 npm dist-tag、Release asset、checksum/attestation 或 Homebrew formula 的后续漂移。

竞品代码验证了这条路径的实用性，同时也暴露了隐私边界需要比“尽可能收集”更保守：

- OpenAI Codex 的 [CLI issue form](https://github.com/openai/codex/blob/main/.github/ISSUE_TEMPLATE/3-cli.yml) 明确请求 `codex doctor --json`，其 [doctor 实现](https://github.com/openai/codex/blob/main/codex-rs/cli/src/doctor.rs) 提供 JSON support report。
- Goose 的 [Diagnostics UI](https://github.com/aaif-goose/goose/blob/main/ui/desktop/src/components/ui/Diagnostics.tsx) 把下载 JSON 与报告问题放在同一表面，其 [session diagnostics command](https://github.com/aaif-goose/goose/blob/main/crates/goose-cli/src/commands/session.rs) 可生成可附加的报告。
- GitHub 官方支持用 `.github/ISSUE_TEMPLATE/*.yml` 定义 required structured fields，并用 `config.yml` 控制 template chooser 与安全报告入口：[Configuring issue templates](https://docs.github.com/en/communities/using-templates-to-encourage-useful-issues-and-pull-requests/configuring-issue-templates-for-your-repository)。

Sigil 采用其中的“结构化诊断 + 产品内入口 + issue form”组合，但不复制默认包含 session data、config file 或 recent logs 的宽收集策略。

## 3. Goals

1. 提供稳定、版本化、可测试的 `sigil doctor --output json` 本地诊断契约。
2. 让用户在 TUI 内通过 `/feedback` 预览包含/排除类别，并显式导出私密支持包。
3. 建立 bug、feature、documentation 三类 GitHub issue forms，并让 bug 表单引导附加诊断证据。
4. 持续验证公开 npm、GitHub Release 和 Homebrew 渠道，而不执行发布或使用私有 credential。
5. 用真实 binary + PTY 验证 `/feedback` 预览、确认、写盘和隐私边界。

## 4. Non-goals

- 不自动上传、自动打开 issue、自动打开浏览器或向任意网络端点发送支持包。
- 不采集 prompt、assistant reply、thought、tool input/output、文件内容、diff、环境变量名称或值、API key、配置文件正文或 session JSONL。非秘密 provider/model、MCP alias 和 capability/sandbox 状态会在预览中明确列为 included diagnostic metadata。
- 不把 support artifact 写进 workspace、Git 仓库、durable session/control stream 或 retention 管理范围。
- 不承诺诊断包绝对不包含用户主动写入非秘密配置字段的私人信息；UI 和文档必须要求用户附加前复核。
- 不建立 crash telemetry、usage analytics、后台 watcher、hosted support backend 或自动 issue triage bot。
- 不改变现有 `sigil doctor` 文本输出、退出码或离线无网络行为。
- 不把 TUI feedback 流程做成 command-only 功能；CLI JSON 仅是自动化和维护入口。

## 5. Architecture and Ownership

### 5.1 Crate boundaries

- `sigil-runtime` 拥有 support schema、doctor-to-support projection、字段级 allowlist、脱敏、大小上限和私密原子写盘。
- `sigil` 注入编译期 build metadata，并提供 `doctor --output text|json` 的薄适配。
- `sigil-tui` 拥有 `/feedback` 命令、独占 modal、预览/确认交互和当前 session 的无内容摘要。
- `.github` 拥有 issue forms 与公开渠道 smoke workflow。
- `sigil-kernel` 不变；support report 不是领域事件或 durable truth。

### 5.2 Data flow

```text
offline doctor checks ─┐
build/environment ─────┼─> runtime redacted projection ─> versioned JSON
coarse session facts ──┘                                  │
                                                          ├─> CLI stdout
                                                          └─> TUI preview -> Enter -> private cache file
```

投影只接受已有结构化 doctor checks、显式 build metadata、平台信息与 coarse session facts。它不得读取 session log、配置正文、workspace 文件或进程环境变量值。

## 6. Stable Diagnostic Contract

### 6.1 CLI surface

```bash
sigil doctor
sigil doctor --output text
sigil doctor --output json
```

默认保持 `text`。JSON 模式只向 stdout 写一个 JSON document；stderr 不混入进度动画。warnings/errors 仍是诊断结果而非命令执行失败，因此成功生成报告时保持 exit code 0；参数、配置读取或序列化失败才返回非零。

### 6.2 Schema V1

根对象至少包含：

- `schema_version: 1`
- `generated_at_unix_ms`
- `build`: version、commit、target、profile
- `environment`: OS、architecture、可选 terminal family
- `summary`: overall status 与 ok/warning/error counts
- `checks[]`: status、stable name、固定 support summary、可选固定 remediation
- `privacy`: included/excluded category labels

TUI support bundle 在该 doctor report 外增加可选 `session`：只包含 session id、durable entry count、provider/model label、run phase 和 busy flag。它不包含内容、路径、tool name、tool result 或 token payload。

`privacy` 必须诚实列出 support bundle 可能保留的非秘密配置摘要：provider/model label、MCP alias、capability/sandbox status。它只能声称排除配置文件正文、credential/environment name/value、conversation/tool/file content，不能笼统声称“无配置数据”。

所有 struct 使用显式 serde 字段名与 `deny_unknown_fields` 反序列化测试。Schema V1 的字段、枚举 token 与语义完全冻结；任何新增、删除、重命名或语义改变都需要新 schema version。精确 JSON fixture 同时锁定机器契约。

### 6.3 Sanitization and bounds

- 投影按精确 stable check name 或受控动态前缀使用字段级 allowlist，不允许把现有 doctor 的任意 message/remediation 原样透传。已知 check 只保留经 credential/path/endpoint 投影的 stable name，并按类别和 status 生成固定 support summary/remediation；未知 check 归并为 `other` 且省略详情。session 中允许的 coarse label 同样经过 safe-persistence、credential、path 与 endpoint 投影。
- 路径按长度降序替换，避免父路径先匹配；Windows 与 Unix 分隔符都必须覆盖。
- 未命中的 Unix/Windows absolute path、home-relative path 与 endpoint URL 使用保守占位符；support report 不保留私有 host、URL path 或 query。
- 单字段、check 数量和最终 JSON 总字节数都有硬上限；超限时 fail closed，不产生半截 JSON。
- terminal checks 只输出固定 status summary；`environment.terminal_family` 只能从 `iterm2`、`apple_terminal`、`wezterm`、`vscode`、`other`、`unknown` allowlist 中选择。不记录 `TERM` 原值、program/profile/version、shell arguments、working directory 或其他环境变量值。
- 测试必须注入 canary secret、home/workspace/config/cache/state path，以及 `TERM`、terminal program/profile/version canary，并证明序列化输出不包含原文。

## 7. TUI `/feedback` Flow

`/feedback` 打开独占 modal 并取得输入焦点。初始预览必须直接展示：

- 即将包含：build、OS/arch、doctor status/counts、coarse current-session facts；
- 明确排除：conversation、tool input/output、file content/diff、config file、credential/environment name/value；同时明确 metadata 可能包含 provider/model、MCP alias 与 capability/sandbox status；
- 当前报告大小和 check 数；
- `Enter` 导出、`Esc` 取消。

第一次 Enter 才写盘到 Sigil cache 下的 `support-bundles/`。文件名由 runtime 生成，不接受用户 path；目录权限在 Unix 上为 `0700`、文件为 `0600`，destination 使用 create-new/atomic publish，拒绝 symlink parent 和覆盖现有文件。成功后 modal 显示准确路径，并提供：`Enter` 在 modal 内分页检查实际写盘的脱敏 JSON、`O` 定位文件、`B` 显式打开 GitHub Bug 表单、`C` 复制报告路径、`U` 复制表单 URL。任何动作都不自动上传或附加报告。

生成报告不写 timeline/session/control event，不触发网络，也不改变 conversation。modal 打开期间所有字母键由 modal 独占，不能落入 composer。

## 8. GitHub Feedback Entry

新增：

- `bug-report.yml`：现象、期望、复现步骤、版本、OS/arch、terminal、安装方式、doctor JSON/support bundle、隐私复核确认。
- `feature-request.yml`：用户问题、期望结果、替代方案与 TUI 影响。
- `documentation.yml`：页面/语言、问题、建议与链接。
- `config.yml`：关闭 blank issues；把安全漏洞指向 `SECURITY.md`。

表单只使用仓库已有 labels。诊断附件是推荐项而不是强制项；敏感信息确认必须 required。

## 9. Published Distribution Smoke

新增独立 workflow，只在 `workflow_dispatch` 与定期 schedule 运行，不加入普通 PR gate：

1. npm：在 Linux、Windows、macOS arm64 与 macOS Intel runner 的隔离目录安装 `@sigil-ai/sigil@alpha`，验证 `--version` 与 `doctor --output json` schema。
2. GitHub Release：解析最新 prerelease，下载四平台 archive 与 `checksums.txt`，验证 aggregate checksum；attestation 通过 GitHub CLI 独立查询和验证，不假设它是 release asset。
3. Homebrew：在两个 macOS architecture 上从公开 tap 安装，验证版本与 doctor JSON。

workflow 使用最小只读权限、固定 action major/version、公开 token 和临时目录。它不 publish、不修改 dist-tag/tap、不访问用户 credential，也不把 schedule failure 误报为代码 PR blocker。

## 10. Implementation Slices

1. R30.0：冻结本 RFC、execution plan、状态板和 commit/gate 边界。
2. R30.1：runtime support schema/字段级 allowlist/脱敏投影、CLI `doctor --output json`、contract/process tests。
3. R30.2：GitHub issue forms、security route 与 YAML/schema validation。
4. R30.3：runtime 私密原子 writer 及权限/symlink/no-clobber/失败清理测试；TUI `/feedback` 独占 modal、预览、显式导出、copy action 与输入/渲染测试。
5. R30.4：npm/Release/Homebrew 持续 smoke workflow 与静态验证。
6. R30.5：真实 binary PTY acceptance、EN/ZH 用户文档、reference/troubleshooting/status/site 与完成审计。

每个 slice 独立提交。R30.1 是 R30.3 的 durable contract 依赖；R30.2 可在 R30.1 后独立落地；R30.4 不依赖 TUI；R30.5 在 R30.1-R30.4 全部完成后收口。

## 11. Acceptance Criteria

- `sigil doctor` 文本输出保持兼容；`--output json` 是唯一 stdout JSON，schema V1 可 round-trip。
- doctor 和 support bundle 中找不到 canary secret、prompt/reply/tool content、配置正文与已知私有绝对路径。
- `/feedback` 默认只预览，Enter 前不写文件；Enter 后只在 Sigil cache 生成一个权限收紧、不可覆盖的 JSON。
- `/feedback` modal 独占输入；Enter、B、C、O、U、滚动键与 Esc 不进入 composer。
- support artifact 不出现在 session/control log，不触发网络，不被自动上传。
- issue forms 可被 GitHub schema 接受，labels/security route 存在且诊断附件为可选。
- distribution smoke 覆盖四个平台 npm、Release checksum/attestation 和双 macOS Homebrew，且只读、无 publish path。
- 真实 binary + PTY 验证预览、确认、写盘、JSON schema 与 privacy canary。
- targeted tests、workspace fmt/check/test/Clippy、docs mirror/link/site、workflow syntax 与 diff gate 通过。

## 12. Validation

```bash
cargo test -p sigil-runtime support
cargo test -p sigil doctor
cargo test -p sigil-tui feedback
cargo run -p sigil -- doctor --output json
scripts/tui-feedback-pty-acceptance.py
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
./scripts/check-docs.sh
./scripts/check-pages-site.sh
git diff --check
```

公开 distribution smoke 在 GitHub-hosted runners 中单独执行；本地 full gate 不能替代其跨平台和已发布渠道证据。

## 13. Progress

- R30.0 complete：commit `cd4168ef` 冻结 privacy、schema、TUI、GitHub feedback entry 与 published-distribution smoke 边界，并完成独立实施前审查。
- R30.1 complete：commit `d9c1cf1d` 落地 doctor/support schema V1、精确 check-name allowlist、固定 support summary/remediation、secret/path/endpoint/terminal 投影和 `sigil doctor --output json`；默认 text 行为保持不变。
- R30.2 complete：commit `476fa177` 增加 bug、feature 和 documentation issue forms、required privacy confirmation 与 security route。
- R30.3 complete：commit `3b32b173` 增加私密原子 writer、Unix 权限和 symlink/no-clobber/失败清理防护，以及 TUI-first `/feedback` 独占预览、显式本地导出和 copy action。
- R30.4 complete：commit `359c31ac` 增加手动/定期只读 published-distribution smoke，覆盖 npm 四 runner、GitHub Release checksum/attestation 和双架构 Homebrew；静态 action/workflow gate 通过，未在本次本地实施中伪称已触发 GitHub-hosted run。
- R30.5 complete：真实 binary + PTY 验收证明 Enter 前整个隔离 state/cache tree 不变、导出后 state tree 不变、modal 输入独占、43 个生产 doctor checks 均命中 allowlist、privacy canary 不泄漏；EN/ZH 用户文档、README、status/changelog、site 与 issue entry 已同步。workspace full gate、docs/site/workflow gate 与两轮独立终审通过，未发现剩余 P1/P2 finding。Windows writer、远端 issue-form 提交和 GitHub-hosted scheduled run 仍由对应平台/服务执行，不被本地验证替代。

## 14. Deferred Work

- 自动 crash capture、opt-in telemetry 与 hosted support ingestion。
- 用户可选择的 session excerpt 或日志附件；若未来需要，必须另立 privacy/consent RFC。
- 自动创建 GitHub issue、OAuth、浏览器 deep link prefill 或上传附件。
- nightly real-model smoke；继续受显式 credential、预算和 provider policy gate 约束。
