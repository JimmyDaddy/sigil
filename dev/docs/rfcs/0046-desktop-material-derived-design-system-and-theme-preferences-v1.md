# RFC-0046 Desktop Material-derived Design System and Theme Preferences V1

状态：active / R46.0 complete，R46.1 ready

创建日期：2026-07-20

基线：

- Desktop UI/UX foundation：[RFC-0045](0045-desktop-ui-ux-foundation-v1.md)
- Desktop shell：[RFC-0044](0044-desktop-shell-mvp-v1.md)
- Desktop runtime bridge：[RFC-0043](0043-desktop-runtime-bridge-v1.md)
- Stable local protocol：[RFC-0026](0026-stable-machine-protocol-and-real-serve.md)

## 1. Summary

RFC-0045 已经解决 desktop 的正确性与安全基础：bounded transcript、active-run reattach、single-final reducer、
approval/cancel/verification、scoped error、单一主滚动区、320px reflow、system light/dark、forced-colors 和焦点恢复均已落地。
这些结论继续有效，不因本 RFC 重开。

当前剩余问题属于第二层产品化缺口：界面虽然“能工作”，但仍没有一套能约束后续开发的组件与视觉系统。workspace、recent、
session、filter、状态和 action 在 navigation 内纵向堆叠；会话行仍是大型卡片加独立 `Open` 按钮；空 inspector 占据宽度；
缺失 metadata 以 `not recorded` 之类占位文案暴露；主题只能跟随系统；`styles.css`、`App.tsx` 和
`ConversationPanel.tsx` 继续集中承担大量样式、布局和交互责任。

本 RFC 冻结以下增量方案：

1. 用 **Material 3 派生的 semantic roles** 统一 color、type、shape、elevation、motion、state layer 与 compact desktop density。
2. 用 **Sigil-owned headless primitive API** 统一 dialog、drawer、menu、popover、select、checkbox、tooltip、collapsible 等行为；
   `@base-ui/react` 1.6.0 是首选行为来源，但必须先通过当前 Safari 13/macOS 11 与三平台 system-WebView 兼容性 pilot，
   业务组件不得直接依赖第三方 primitive。
3. 增加 application-scope 的 `system | light | dark` 主题偏好、native 持久化、首帧防闪烁和系统主题运行时跟随。
4. 采用“紧凑 session rail + conversation + 条件式 review pane”的 A/C 混合工作台，减少重复身份、常驻筛选器和空白面板。
5. 建立 internal UI kit、fixture catalog、import/token gate 和真实 Tauri system-WebView 验收，阻止界面再次退化为散落的原生控件和任意 CSS。

这里使用 **Material-derived** 而不是 **Material-compliant**：Sigil 借用 Material 3 的角色、状态、主题与自适应方法，
但保留 coding-agent 的高信息密度、安全审批、diff、terminal 和桌面平台习惯，不复制 Android 外观或移动端组件心智。

## 2. RFC-0045 baseline and current delta

### 2.1 已完成且不得回退的能力

| Contract | RFC-0046 requirement |
| --- | --- |
| HTTP/SSE 是唯一 runtime contract | 视觉与主题实现不得新增 kernel/runtime/OpenAPI 依赖 |
| Renderer 只消费 allowlisted Tauri DTO/event | 不开放 bearer、path、process、generic HTTP/filesystem/shell/window capability |
| Active run 可 reattach/cancel/approve | navigation、theme、pane 与 density 切换不得卸载 follower |
| Single-final reducer | 组件迁移不得再次产生 duplicate assistant reply |
| Approval/Tool/Verification 是领域对象 | 不得降级为普通 message/card 文本 |
| 三域 IA 与单一主滚动区 | 只调整层级、密度和 supporting pane 显隐，不引入双主滚动 |
| 320px、keyboard、focus restore、forced-colors | 继续作为回归底线，并补 200% zoom 与主题生命周期证据 |

### 2.2 2026-07-20 实现审计

| Finding | Current evidence | RFC-0046 response |
| --- | --- | --- |
| 主题只有系统跟随 | `styles.css` 以 dark `:root` 加 `prefers-color-scheme: light` 实现；没有显式偏好、持久化或主题入口 | 增加 native application preference 与 renderer resolved theme |
| 首帧与 native chrome 未绑定 | `index.html` 的 `theme-color` 是静态暗色；native window 创建没有读取 appearance preference | window build 前读取偏好并注入 enum-only pre-paint state |
| UI system 仍是单体 stylesheet | `styles.css` 约 604 行，组件和全局布局共享同一命名空间 | 拆为 foundation、primitive、layout、feedback 与 feature style boundary |
| 顶层状态与布局职责集中 | `App.tsx` 约 568 行，`ConversationPanel.tsx` 约 699 行 | 建立 AppFrame/SessionRail/Conversation/Review feature boundary |
| Navigation 信息密度过低 | 1379×850 dogfood 中通常只露出约 1–2 个完整 session card；workspace 身份、recent、filter 与 action 重复 | 一行可选择 session、合并 workspace selector、筛选 popover、异常渐进披露 |
| Supporting pane 缺少上下文 | verification 尚无 evidence 时仍可常驻空 inspector | 只有存在选中对象、失败 evidence 或显式打开时才占宽度 |
| Unknown metadata 成为产品文案 | ToolCard 显示 `duration not recorded`、`risk not classified` | 未知且不可行动的字段默认省略，Details 才解释来源 |
| Gate 只能发现少量字符串问题 | 当前脚本只检查 raw color、若干 media query 与 320px 字符串 | 增加 role parity、contrast、import boundary、theme lifecycle、fixture 与 packaged WebView gate |
| 候选 primitive 与平台基线未对齐 | Vite 仍以 `safari13` 为 build target，macOS bundle 最低版本是 11.0；Base UI 1.6 的官方 browserslist 从 Safari 16.4 开始 | 把兼容性与供应链 pilot 设为第一实现切片；本 RFC 不静默抬高平台最低版本 |

这些 finding 不表示 RFC-0045 当时的 completion audit 失效；RFC-0045 证明的是可恢复、安全、可访问的源码 dogfood
foundation，本 RFC 处理的是在真实 dogfood 后出现的可用性、密度和长期一致性压力。

## 3. Research and product decision

### 3.1 Official design evidence

- [Material 3 theming](https://developer.android.com/develop/ui/compose/designsystems/material3) 把 color scheme、typography
  和 shape 作为主题子系统，并以 primary/on-primary、surface/on-surface 等 role 驱动组件，而不是让业务组件直接选择颜色。
- [Material supporting pane](https://developer.android.com/develop/adaptive-apps/guides/build-a-supporting-pane-layout) 要求 supporting
  information 围绕主内容出现；大窗口可并排，小窗口一次只显示一个 pane。
- [Base UI](https://base-ui.com/react/overview/about) 提供 unstyled、accessible、composable React primitives，允许 Sigil 保留自己的
  CSS、密度与桌面外观；其 [release history](https://base-ui.com/react/overview/releases) 显示 1.0 自 2025-12 稳定，
  2026-06 的 1.6.0 继续维护 accessibility 和 Drawer/Combobox 行为。
- Base UI 只承诺 major release 时的 Baseline Widely Available 浏览器；其官方
  [browserslist](https://github.com/mui/base-ui/blob/master/.browserslistrc) 当前最低列出 Safari 16.4，不能直接等价于本项目
  `safari13`/macOS 11 的兼容承诺。因此它是有条件候选，不是 RFC-0046 的先验硬依赖。
- [Material Web](https://github.com/material-components/material-web) 官方仓库已注明 maintenance mode，因此不作为新运行时依赖。
- [MUI CSS theme variables](https://mui.com/material-ui/customization/css-theme-variables/configuration/) 证明
  `system | light | dark`、data selector 与 pre-render color-scheme 初始化是成熟模式；本项目只借鉴该状态模型，不引入完整 MUI。
- [VS Code sidebar guidance](https://code.visualstudio.com/api/ux-guidelines/sidebars) 要求相关内容分组，避免过多 view、重复功能和本可由
  一个 action 完成的常驻内容。
- [WAI-ARIA APG](https://www.w3.org/WAI/ARIA/apg/patterns/) 为 dialog、disclosure、menu、toolbar、tabs 等行为提供键盘和语义基线。
- [Tauri window theme API](https://v2.tauri.app/reference/javascript/api/namespacewindow/) 支持读取、设置以及监听系统主题；
  Sigil 通过收窄 native command 使用该能力，不向 renderer 开放 generic window control。

### 3.2 Frozen architecture and gated dependency decision

| Layer | Decision | Rationale |
| --- | --- | --- |
| Design language | Material 3-derived semantic system | 统一角色、状态和 adaptive 方法，不复制 Android 外观 |
| Behavior primitives | Sigil internal primitive contract；`@base-ui/react` 1.6.0 仅在 R46.1 go decision 后 exact-pin | 业务 API 与供应商解耦；不为组件库静默抬高 OS/WebView 基线 |
| Styling | CSS custom properties + CSS Modules + CSS layers | 复用现有 Vite，不引入 Tailwind/CSS-in-JS runtime |
| Iconography | internal `Icon` wrapper over an exact-pinned local SVG/icon source | 无远程字体；统一 size/stroke/label；业务不直接拼 SVG |
| Theme preference | native application store + renderer resolved theme | native 能在 window visible 前应用；renderer 保持 presentation truth |
| Component catalog | repo-owned dev-only fixture catalog in the real Vite/WebView stack | 避免 Storybook 大依赖面，并保留 system-WebView 真实性 |
| UI tests | Testing Library/Vitest + contract scripts + packaged WebView/AX smoke | 交互、token、几何和真实壳分别取证 |

本 RFC 不引入 `@material/web`、完整 MUI、shadcn runtime、Tailwind、remote font、remote icon 或 Chromatic cloud。
shadcn 等项目可以作为 recipe 参考，但任何代码都必须进入 Sigil 的 internal component boundary 并接受本仓库测试与供应链审计。

## 4. Goals and non-goals

### 4.1 Goals

1. 让后续 desktop feature 默认复用同一 primitive、token、density 与 feedback pattern，而不是继续增加 ad-hoc CSS。
2. 在不改 runtime contract 的前提下，让 30+ sessions、长中英文标题、异常 catalog 与 active run 都能高效浏览。
3. 用户可显式选择 system/light/dark，重启后保留，system 模式随 OS 更新，切换时不丢失任何 run/conversation 状态。
4. 让 review/verification/diff 成为按上下文出现的 supporting pane，不再以空 inspector 持续挤压 conversation。
5. 用可执行 gate 约束 raw colors、直接第三方 import、重复 focus logic、主题对比度、viewport 和代表性 domain state。

### 4.2 Non-goals

- 不重设计 TUI，也不复用或迁移 kernel `AppearanceConfig`；desktop preference 与 TUI theme 相互独立。
- 不改变 session、run、approval、verification、checkpoint、SQLite 或 HTTP/OpenAPI contract。
- 不实现 Material You wallpaper/dynamic color、任意 seed、自定义 token editor、主题插件或云同步。
- 不构建完整 Settings、command palette、多窗口、browser UI、IDE shell、updater或公开安装渠道。
- 不添加移动端 FAB、Android navigation rail/ripple，也不把 session 当作 app-level destination。
- 不声称撤销 shell/remote side effect，不改变 active-run close confirmation。
- 不把 screenshot diff 当作 accessibility 或行为正确性的替代品。

## 5. Hard invariants

1. Desktop 仍是 TUI 之外的 opt-in companion；TUI-first 定位与 TUI shortcut 不变。
2. Renderer 不获得 bearer、absolute path、child/process、generic HTTP/filesystem/shell 或 unrestricted window capability。
3. `ThemePreference` 是 application appearance，不进入 workspace `sigil.toml`、session/control log、provider context、HTTP/OpenAPI 或 SQLite。
4. 主题和 pane 切换只能改变 presentation；不得 remount active `ConversationPanel`、重建 follower、重复 attach、清空 draft、重置 timeline scroll 或焦点。
5. CSS/组件迁移不得改变 RFC-0045 的 single-final、approval guard、cancel acknowledgement、verification binding、bounded output 与 gap 语义。
6. 第三方 behavior primitive 只能由 `apps/desktop/src/ui/primitives/**` 直接 import；feature/domain 只消费 Sigil internal UI API。
7. Raw palette value 只能存在于 reference/theme token 文件和 forced-colors mapping；组件不得直接写 status hex/rgb。
8. 远程 font/icon/style/script 继续被 CSP 禁止；主题实现不得放宽 production CSP。
9. 新交互必须同时覆盖 loading、empty、degraded/error、active 和 terminal 中适用的状态；不能只实现 happy path。
10. 三平台人工 system-WebView、200% zoom、screen-reader 与签名/notarization仍保持真实状态；未执行不得写成 pass。
11. RFC-0046 不得把 Vite `safari13` target、macOS 11.0 minimum 或其他平台 floor 静默上调；若未来决定调整，必须单独给出
    用户影响、测试矩阵和分发证据。

## 6. Design-system architecture

### 6.1 Layering

```text
reference values
  -> semantic system roles
    -> Sigil domain roles
      -> primitive component tokens
        -> product patterns / feature surfaces
```

推荐目录：

```text
apps/desktop/src/
  appearance/
    contract.ts
    ThemeProvider.tsx
    resolveTheme.ts
  ui/
    foundations/
      reset.css
      reference.css
      themes.css
      typography.css
      density.css
      elevation.css
      motion.css
    primitives/
      Button.tsx
      IconButton.tsx
      TextField.tsx
      TextArea.tsx
      Select.tsx
      Checkbox.tsx
      Dialog.tsx
      Drawer.tsx
      Menu.tsx
      Popover.tsx
      Tooltip.tsx
      Collapsible.tsx
      Toast.tsx
    layout/
      AppFrame.tsx
      Pane.tsx
      PaneHeader.tsx
      Toolbar.tsx
      SupportingPane.tsx
    feedback/
      InlineAlert.tsx
      StatusIndicator.tsx
      EmptyState.tsx
      ProgressIndicator.tsx
    icons/
      Icon.tsx
      index.ts
    catalog/
      fixtures.ts
      UiCatalog.tsx
  features/
    workspaces/
    sessions/
    conversation/
    review/
```

`ui/primitives` 负责 DOM/APG/behavior source 适配；`ui/layout` 和 `ui/feedback` 负责跨领域 pattern；`features` 保留 workspace、
session、run、approval、verification 等产品语义。不得建立一个把所有 domain state 重新吞回去的 `components.tsx`。

### 6.2 Token taxonomy

Reference token 保存可替换的原始值，system role 表达用途，domain role 扩展 coding-agent 语义：

```css
/* reference/theme files only */
--sg-ref-color-green-40: ...;
--sg-ref-space-3: 12px;

/* Material-derived system roles */
--sg-sys-color-primary: ...;
--sg-sys-color-on-primary: ...;
--sg-sys-color-primary-container: ...;
--sg-sys-color-on-primary-container: ...;
--sg-sys-color-surface: ...;
--sg-sys-color-surface-container-low: ...;
--sg-sys-color-surface-container: ...;
--sg-sys-color-surface-container-high: ...;
--sg-sys-color-on-surface: ...;
--sg-sys-color-on-surface-variant: ...;
--sg-sys-color-outline: ...;
--sg-sys-color-outline-variant: ...;
--sg-sys-color-error: ...;
--sg-sys-color-on-error: ...;
--sg-sys-color-error-container: ...;
--sg-sys-color-on-error-container: ...;

/* Sigil domain roles */
--sg-domain-color-success: ...;
--sg-domain-color-warning: ...;
--sg-domain-color-info: ...;
--sg-domain-color-approval-container: ...;
--sg-domain-color-tool-container: ...;
--sg-domain-color-diff-added-container: ...;
--sg-domain-color-diff-removed-container: ...;
--sg-domain-color-verification-failed-container: ...;
```

每个 foreground/container role 必须在 light/dark 两套 scheme 中成对定义；forced-colors 优先覆盖显式 light/dark。
旧 `--color-*` 在迁移窗口内只能作为 alias 指向新 role，R46.6 完成后删除，不允许形成永久双 token 系统。

### 6.3 Typography

- UI 字体使用 platform system stack，不请求远程 Roboto/Inter。
- 正文默认 14/20；secondary metadata 12/16；label 11–12/16；pane title 16/24；空状态 headline 不超过 28/36。
- 代码、terminal、diff 使用 platform monospace stack；正文和代码不共享 letter spacing。
- ALL CAPS 只用于极短的状态 label；workspace/session title 不使用 eyebrow + heading 的重复层级。
- 长中文、英文、路径片段和 opaque evidence 必须分别验证 truncation 与 inspect，不靠缩小字号塞入一行。

### 6.4 Density and target sizes

V1 只提供一个 compact desktop density，不增加用户可配置 density：

| Element | Contract |
| --- | --- |
| Standard control | 36px visual height，至少 32×32px hit target |
| Critical/primary action | 至少 40px height |
| Icon glyph | 16–18px；业务不得用 `+`、`×` 充当未命名 icon |
| Session row | 56–68px，整行可选择，不再附加 full-width `Open` |
| Navigation width | 248–280px；不得随内容扩成半屏 |
| Review pane | 320–380px，仅有上下文或显式打开时出现 |
| Toolbar/section gap | 使用固定 4/8/12/16/24 spacing scale |
| Corner radius | 4/8/12/16/full；同层级不得各自发明半径 |

在 1280×720、30 条 ready session fixture 下，navigation 初始视图必须至少露出 5 个完整 session row；
正常 ready 不以独立 badge 反复占宽度，异常状态才提升视觉权重。

### 6.5 Shape, elevation and motion

- Pane 主要靠 surface-container 与 outline 分层；暗色主题优先 tonal elevation，不为每个 section 增加卡片边框。
- Dialog/drawer/popover 才使用高 elevation；普通 session row 和 metadata 不使用大 shadow。
- Hover/pressed/selected/disabled 使用统一 state layer；focus-visible 使用独立高对比 outline。
- Motion 只用于 pane/overlay/selection 的因果变化；不对 streaming token、status dot 或 loading 文案做装饰性动画。
- `prefers-reduced-motion` 时 transition/animation 关闭，信息与操作顺序保持不变。

## 7. Theme preference and native synchronization

### 7.1 State model

```ts
type ThemePreference = "system" | "light" | "dark";
type ResolvedTheme = "light" | "dark";
```

Ownership 分为两层：

- **Native durable owner**：保存 application-scope `ThemePreference`，在窗口 visible 前把偏好映射到 Tauri native theme。
- **Renderer presentation owner**：根据偏好与当前 OS theme 计算 `ResolvedTheme`，设置 `html[data-theme]`、CSS
  `color-scheme` 和 theme-color；所有 React component 只消费 resolved theme/context。

主题偏好不进入 server bootstrap、workspace manager 或 session projection。`desktop_bootstrap` 可以返回 renderer-safe appearance
snapshot，也可以由独立的 narrow appearance command 提供；两种实现都不得改 OpenAPI。

### 7.2 Native persistence contract

建议新增：

```text
apps/desktop/src-tauri/src/appearance.rs
<app-config-dir>/appearance-v1.json
```

文件只包含：

```json
{
  "schemaVersion": 1,
  "themePreference": "system"
}
```

约束：

- 最大 4 KiB，deny unknown fields，enum 之外的值拒绝。
- 使用同目录 temporary file + sync + atomic replace；不保存 workspace/path/user identity。
- missing、损坏、unknown version 或 I/O unavailable 都 fail-soft 到 `system`，不得阻止窗口、workspace 或 run。
- preference 修改错误只进入 Appearance surface，不复用 workspace/run global error。

### 7.3 First-paint sequence

```text
load bounded preference
  -> map system to None; light/dark to explicit Tauri theme
  -> build native window with theme before visible
  -> inject enum-only pre-paint initializer
  -> set data-theme-preference + resolved data-theme + color-scheme
  -> load CSS and React
  -> attach OS theme listener only for system preference
```

初始化脚本只能包含固定 enum 和常量逻辑，不能注入 path、token 或任意 serialized config；production CSP 不增加
`unsafe-inline` script。实现可以使用 Tauri initialization script 或 self-hosted pre-main asset，但必须证明 theme attribute
先于 app stylesheet/React paint 生效。

### 7.4 Runtime transition

| Current preference | Event | Result |
| --- | --- | --- |
| system | OS theme changed | 更新 resolved theme 与 native chrome，不持久化新偏好 |
| light/dark | OS theme changed | 忽略，保持 explicit resolved theme |
| any | user chooses light/dark | narrow native command 原子保存并同步 window；renderer 原位更新 |
| light/dark | user chooses system | 保存 system，native theme 设为 follow-system，立即解析当前 OS theme |
| any | save/apply failed | 保留上一已证明状态，显示 scoped retry，不触碰 conversation |

主题入口位于 application Appearance popover/dialog，并提供 `Cmd/Ctrl+,`；不在 topbar 常驻多个 sun/moon 按钮。
主题选择使用单选语义，键盘可操作，并明确显示 `System` 当前解析为 Light 或 Dark。

### 7.5 Presentation isolation proof

切换主题前后必须保持：

- workspace ID、session ID、run ID 与 follower attachment count；
- pending approval guard 与当前 verification selection；
- composer draft/IME 状态、timeline scroll anchor 和 focused element；
- navigation/review open state；
- durable/live row count 与 single-final identity。

React theme update 不允许通过改变 `key` 或重新 mount App/Conversation root 实现。

## 8. Primitive and component contracts

### 8.1 Primitive boundary

| Internal primitive | Behavior source | Required states |
| --- | --- | --- |
| Button/IconButton | native/internal wrapper；Base UI if pilot passes | default/hover/pressed/focus/disabled/loading/destructive |
| TextField/TextArea | native input wrapper | label/help/error/IME/disabled |
| Select/Checkbox | internal wrapper；Base UI if pilot passes | keyboard、form semantics、focus-visible、disabled |
| Dialog/Drawer | internal wrapper；Base UI if pilot passes | modal/inert、initial focus、Tab trap、Esc、restore |
| Menu/Popover | internal wrapper；Base UI if pilot passes | anchor、arrow keys、typeahead、dismiss、restore |
| Tooltip | internal wrapper；Base UI if pilot passes | hover/focus、delay、nonessential only |
| Collapsible | internal wrapper；Base UI if pilot passes | disclosure semantics、Enter/Space、controlled state |
| Toast | internal live region adapter；Base UI if pilot passes | bounded queue、dedupe、no critical decision |

`useFocusBoundary.ts` 在所有 dialog/drawer 成功迁移并通过行为测试后删除；迁移期间不能同时让 custom trap 和新的 primitive
管理同一 overlay。

### 8.2 Import and raw-element policy

- 任何获准的第三方 behavior primitive 只允许出现在 `src/ui/primitives/**`。
- icon source 只允许由 `src/ui/icons/**` import；feature 只使用 named internal icon。
- Feature 不直接写 modal/drawer/menu/select/checkbox/tooltip；应使用 internal primitive。
- 普通 semantic HTML 仍然允许；规则不禁止 article、section、list、code、pre、details 等内容结构。
- 原生 `textarea` 的 IME 行为由 internal TextArea 包装保留，不能为了统一外观破坏 composition contract。
- Raw interactive element gate 使用小型 allowlist 迁移债务；R46.6 结束时 allowlist 必须清零或逐项保留有理由的语义例外。

### 8.3 Product pattern mapping

| Current surface | Target pattern |
| --- | --- |
| Workspaces + Recent workspaces | 单一 `WorkspaceSwitcher`，open/recent 分组，状态和 close action 在 menu 内 |
| Permanent history filters | SearchField + FilterButton popover + active filter chips |
| Session card + Open button | 整行可选择的 `SessionRow`，异常 action 进入 context menu |
| Selected-session banner + conversation header | 一个 `ConversationHeader` |
| Empty permanent inspector | `SupportingReviewPane`，按 selection/evidence/explicit-open 出现 |
| Tool placeholders | 有值才渲染 metadata；风险未知不伪造 safe，也不刷无用文案 |
| Global ready statusbar | 正常状态并入 workspace indicator；底部只在可行动 global issue 时出现 |
| Repeated error boxes | scope-aware InlineAlert/ErrorCard pattern |
| Raw evidence IDs | Evidence disclosure/inspector，主表面保留行动和失败定位 |

ApprovalDock 继续位于 conversation 主流程并保持 sticky/high prominence；它不是普通 supporting-pane detail，也不能因 review pane
关闭而不可操作。

## 9. Adaptive workbench information architecture

### 9.1 A/C hybrid

```text
Expanded, contextual review open
┌────────────────┬───────────────────────────────┬──────────────────┐
│ Workspace      │ Conversation                  │ Review           │
│ Search/filter  │ Header                        │ Diff/tool/check  │
│ Session rows   │ Timeline                      │ Evidence         │
│                │ Approval + Composer           │                  │
└────────────────┴───────────────────────────────┴──────────────────┘

Expanded, no review context
┌────────────────┬──────────────────────────────────────────────────┐
│ Session rail   │ Conversation                                     │
└────────────────┴──────────────────────────────────────────────────┘
```

左侧使用方案 A 的 compact conversation rail；右侧使用方案 C 的 contextual review state。三栏不是默认永久布局，
而是“主内容 + 当下相关 supporting information”。

### 9.2 Navigation hierarchy

固定顺序：

1. App/Workspace toolbar：品牌、单一 WorkspaceSwitcher、New conversation、Application menu。
2. Session search：一个 SearchField；filter 收入 popover，只有 active filter 显示 chip。
3. Catalog notice：正常不显示；degraded summary 保持单行，可展开详情。
4. Session list：唯一主要 navigation scroll，整行选择，键盘上下移动/Enter 打开。

不得同时显示 topbar workspace chip、Workspaces section、Recent workspaces section、session eyebrow 和 footer workspace ready
来表达同一身份。Workspace close 仍保留 active-run confirm，并从 WorkspaceSwitcher 的 contextual action 进入。

### 9.3 Conversation hierarchy

- Header 只显示一次 session title、必要 run state 和少量 contextual actions。
- Timeline 继续是唯一主任务 scroll；reasoning/progress/tool/message 语义保持 RFC-0045 contract。
- Empty state 不使用超大 display heading；只保留一个说明、一个主 action，创建 session 后立即聚焦 composer。
- Composer 始终是 conversation primary action；active run 时仍可准备 follow-up，不以主题/布局变化重置 draft。
- `ready` 不重复显示；running、waiting approval、reconnecting、failed 等有行动含义的状态才占据 header prominence。

### 9.4 Supporting review pane

Review pane 在以下条件之一满足时出现：

- 用户选择 tool/diff/verification/evidence；
- verification failed/inconclusive 且存在可行动 recommendation；
- user 显式打开 Review。

没有 selection/evidence 时不占据宽度。关闭后 selection 可保留但 pane 退出 layout；再次打开恢复相同 detail、scroll 和焦点。
Approval 继续在主 conversation；blocking error 不应被关在已关闭的 inspector 中。

### 9.5 Layout classes

| Class | Effective width | Presentation |
| --- | --- | --- |
| Expanded | ≥1280 CSS px | 248–280px nav；review 有上下文时 320–380px inline |
| Medium | 840–1279 CSS px | nav + conversation；review 使用 focus-managed drawer |
| Compact | 320–839 CSS px | conversation 为主；nav/review 一次一个 overlay pane |

断点来自 main-content 最小宽度，不是设备名称。200% zoom 使 CSS viewport 变窄时必须自动进入对应 class；禁止依赖
horizontal document scroll 完成 daily loop。Pane visibility 改变不得 unmount conversation/follower。

## 10. Accessibility and platform behavior

1. Primary text contrast ≥4.5:1；large text、UI boundary、focus indicator ≥3:1。
2. Hover、selected、running、failed、disabled 不得只靠颜色；至少同时有 label、shape、icon 或 position cue。
3. Theme picker 使用 radio/selection semantics；Dialog/Drawer/Menu 遵循 APG keyboard model。
4. Drawer/Dialog/Popover 打开时 initial focus 可预测，Tab 不逃逸，Esc 关闭并恢复原触发点。
5. Session row 整行可键盘激活；row 内 secondary menu 不制造嵌套 interactive element。
6. Streaming 继续 `aria-live=off`，terminal summary 才进入 polite region；theme switch 不触发 transcript announcement storm。
7. `forced-colors: active` 高于显式 theme；`prefers-reduced-motion` 高于 motion token。
8. 320px、200% zoom、long EN/ZH、IME、VoiceOver/AX 与平台 system-WebView 都进入 evidence matrix。
9. Tooltip 只补充非关键说明；任何 action 不能仅靠 hover tooltip 才可理解。

## 11. Component catalog and governance gate

R46 不在 V1 引入 Storybook。原因不是 Storybook 无价值，而是当前最大风险来自 Tauri system-WebView、native theme、focus 与
runtime continuity；浏览器内 story pass 不能替代这些证据，并会一次性扩大 dev dependency 与维护面。

改为建立 repo-owned、dev-only fixture catalog：

- 复用真实 Vite/CSS/internal components，不持有 bearer 或真实 bridge。
- 使用 typed fixture bridge；catalog 代码在 production build 中不可达，并由 build marker gate 验证。
- fixture 同时供 Testing Library 与人工/packaged WebView smoke 使用。
- 至少覆盖：no-workspace、empty session、30/100 sessions、degraded catalog、running+tool、waiting high-risk approval、
  reconnecting+gap、completed+verification failed、diff、长中英文、missing optional metadata。
- 每个 fixture 可切换 light/dark/system resolution、forced-colors simulation 与 Expanded/Medium/Compact viewport。

升级 `check-desktop-ui-system.sh` 或增加等价脚本，至少检查：

1. light/dark role parity 与必需 foreground/container 对。
2. token contrast；raw color 只出现在允许的 foundation 文件。
3. 第三方 primitive/import/icon boundary。
4. raw interactive migration allowlist。
5. production bundle 不包含 catalog route/fixture payload。
6. representative geometry：session density、无 document overflow、一个主 scroll、conditional review width。

## 12. Migration and rollback

### 12.1 Migration policy

- 先建立 token/theme/primitive，再迁 workspace chrome，最后迁 coding-agent domain surface。
- 每个 feature 一次只迁 presentation，不同时改 server contract 或 reducer。
- 旧 class/token alias 在对应 feature 完成前保留；每片结束记录剩余 debt，R46.6 统一删除。
- `App.tsx` 和 `ConversationPanel.tsx` 的拆分必须沿 feature ownership 完成，不把所有状态搬进新的 global context。
- 新 primitive 应先有 catalog state 与 interaction test，再替换真实 surface。

### 12.2 Rollback boundary

- R46.1 只形成 go/no-go 记录、最小 spike 和 internal contract，不迁产品 surface；失败时直接移除 spike。
- R46.2 token 可通过 alias 回退，不改变 DOM/state。
- R46.3 appearance store/schema 独立；移除 UI 入口后旧文件可安全忽略，system 仍是默认。
- R46.4 primitive wrapper 可逐组件回退；不能在同一 commit 混入 feature state rewrite。
- R46.5/R46.6 每个 commit 保持 renderer DTO 与 bridge 不变，允许 presentation-level revert。
- 若 Base UI compatibility、package audit、CSP、bundle 或真实 system-WebView gate 不通过，R46.1 记录 no-go：不安装该依赖，
  后续以同一 internal primitive contract 的 repo-owned adapter 实现。不得静默换成完整 MUI、跳过行为测试或提高平台 floor。

## 13. Execution slices and commit boundaries

依赖拓扑：

```text
R46.0 → R46.1 → R46.2 → [R46.3 ∥ R46.4] → R46.5 → R46.6 → R46.7 → R46.8
```

1. **R46.0 Contract and RFC-0045 delta freeze** — complete
   - 本 RFC、调研、ownership、role map、IA、acceptance、non-goals 和 regression ledger。
   - Commit：`docs(rfc): open desktop material design system`
2. **R46.1 Primitive compatibility and supply-chain pilot** — ready
   - 冻结 internal primitive API；在 React 19、现有 Vite `safari13` target、macOS 11 system-WebView、Windows WebView2、
     WebKitGTK、CSP、portal/focus/IME、bundle/license/audit 上验证 Base UI 1.6.0。
   - 形成可复核 go/no-go decision：go 才 exact-pin 并同步 dependency supply-chain ledger；no-go 则移除 spike，采用
     repo-owned adapter，且不得改变平台 floor。
   - Safari 13-equivalent WebKit、macOS 11 minimum、Windows WebView2 或 Linux WebKitGTK 的所需运行时证据缺失/失败，
     都不能被 build target 或 browser test 代替，候选依赖必须判 no-go；签名/notarization和更完整人工发布矩阵可继续独立 pending。
   - 不迁真实 feature，不重写现有 focus 行为。
   - Commit：`chore(desktop): validate ui primitive compatibility`
3. **R46.2 Material-derived tokens and visual baseline** — gated by R46.1
   - foundation CSS layers、light/dark role parity、domain roles、typography/density/elevation/motion、旧 token alias、fixture catalog skeleton、contrast/import gate。
   - 不迁真实 feature layout，不增加主题持久化。
   - Commit：`refactor(desktop): establish material design tokens`
4. **R46.3 Theme preference lifecycle and native sync** — gated by R46.2
   - native appearance store、pre-window theme、enum-only pre-paint、renderer ThemeProvider、system/light/dark UI、OS change、failure isolation、presentation continuity tests。
   - 不改 session/OpenAPI/runtime。
   - Commit：`feat(desktop): add persistent theme preferences`
5. **R46.4 Accessible primitives and interaction states** — gated by R46.2，可与 R46.3 并行
   - 按 R46.1 decision 使用 exact-pinned Base UI 或 repo-owned adapter；建立 internal primitive/icon boundary、
     overlay/form/disclosure migration 与 custom focus boundary retirement gate；dependency ledger 已由 R46.1 原子完成。
   - 不迁 workspace/session domain state。
   - Commit：`refactor(desktop): standardize accessible ui primitives`
6. **R46.5 Workspace chrome and compact session rail** — gated by R46.3 + R46.4
   - AppFrame、WorkspaceSwitcher、toolbar/search/filter popover、SessionRow、degraded summary、scoped global status、Expanded/Medium/Compact navigation。
   - 保持 catalog/recent/close/run contracts。
   - Commit：`refactor(desktop): migrate workspace chrome`
7. **R46.6 Conversation and contextual review migration** — gated by R46.5
   - 合并 conversation identity、条件式 review pane、tool/diff/verification/approval/composer visual hierarchy、optional metadata、省略旧 token/class/focus debt。
   - 保持 single-final、reattach、approval、cancel、verification、IME/draft contract。
   - Commit：`refactor(desktop): migrate coding agent surfaces`
8. **R46.7 Cross-theme adaptive and accessibility evidence** — gated by R46.6
   - fixture matrix、contrast/import/raw-control gate、viewport/200% zoom、keyboard/AX/VoiceOver、active-run theme isolation、real macOS bundle；Linux/Windows 状态按实际 CI/manual evidence 记录。
   - Commit：`test(desktop): gate themes and adaptive accessibility`
9. **R46.8 Completion audit and documentation sync** — gated by R46.7
   - code/security/implementation completeness review；full workspace、OpenAPI unchanged、package、docs/site；功能落地后才更新双语用户文档与 dogfood guide。
   - Commit：`docs(rfc): close desktop material design system`

每个切片独立 commit。R46.1 先消除组件库与最低平台的不确定性；R46.3 和 R46.4 只有在 R46.2 role/API 冻结后可并行；
R46.5 不得在两者任一未完成时开始。

## 14. Validation matrix

| Area | Automated evidence | Real/manual evidence |
| --- | --- | --- |
| Token/theme roles | role parity、contrast、raw-color、forced-colors tests | light/dark representative surface review |
| Preference lifecycle | native store、renderer resolver、OS event、corrupt fallback tests | cold start in system light/dark + explicit override |
| Presentation isolation | active run/approval/draft/scroll/focus interaction test | switch theme during real run |
| Primitives | keyboard/focus/dismiss/restore tests | Tauri system-WebView menu/dialog/drawer smoke |
| Navigation density | fixture geometry and row count | 1280×720、1379×850、1440×900 dogfood |
| Adaptive layout | 1280、900、840/839、760、320 contracts | 200% zoom and resize in packaged app |
| Domain surfaces | single-final/tool/approval/verification regression suite | real transcript/run/approval/review workflow |
| Accessibility | Testing Library roles、contrast、reduced-motion、AX tree | VoiceOver/macOS；Linux/Windows如实记录 |
| Security/supply | capability diff、CSP、pnpm audit、dependency ledger | package contains no remote asset or debug catalog |
| Runtime boundary | existing native/real serve contract tests、OpenAPI drift clean | sidecar/package startup |

切片常用 gate：

```bash
pnpm --dir apps/desktop check
cargo test -p sigil-desktop-app
cargo clippy -p sigil-desktop-app --all-targets -- -D warnings
cargo fmt --all --check
pnpm --dir apps/desktop audit --audit-level high
node scripts/test-prepare-desktop-sidecar.mjs
./scripts/check-touched.sh --tier standard
./scripts/check-docs.sh
git diff --check
```

R46.8 再运行 full workspace、docs/site、package CI 与平台 evidence matrix。`pnpm audit` 只是供应链输入之一，不能替代
license、lockfile、advisory exception 和 production graph 审计。

## 15. Acceptance criteria

1. 业务组件不直接 import 第三方 primitive/icon source；新的 overlay/form/navigation action 默认复用 internal primitive。
2. light/dark role 完整且通过 contrast gate；forced-colors/reduced-motion 覆盖 explicit theme。
3. 用户可选择 system/light/dark；cold start、reload、restart、OS theme change 与 corrupt preference 有测试。
4. 首个可见 frame、CSS `color-scheme`、resolved theme 与 native chrome 一致，无可观察 dark↔light flash。
5. 切换主题不改变 run attachment、approval、draft、timeline scroll/focus 或 durable/live row identity。
6. 1280×720 的 30-session fixture 初始至少显示 5 个完整 session row；整行打开，无重复 full-width `Open`。
7. Workspace 身份只有一个 primary selector；recent、close、status 不再以多个常驻 section 重复。
8. Filter 默认折叠；仅 active filter 以 chip/summary 显示；catalog warning 不打印零项或占据大型卡片。
9. 没有 review context 时 inspector 不占宽度；有 context 时 expanded inline、medium/compact drawer，focus/scroll 可恢复。
10. Conversation title 不重复；missing optional tool metadata 默认省略；approval/verification/diff 的行动层级清晰。
11. 320px、200% zoom、long EN/ZH、keyboard、IME、VoiceOver/AX、system-WebView evidence 可复核。
12. RFC-0045 的 transcript/reattach/single-final/approval/cancel/verification/security regression 全部通过；OpenAPI 无无关 drift。
13. Production bundle 不含 debug catalog、remote font/icon、generic capability 或 CSP widening。
14. R46.1 有可复核的 Base UI go/no-go evidence；无论结果如何，都不提高当前 platform/build floor，业务组件 API 保持一致。
15. 双语用户文档只在功能实际落地后的 R46.8 宣传手动主题与新 navigation，不提前 overclaim。

## 16. Risks and deferred decisions

### 16.1 Risks

- **Base UI browser floor 高于当前产品 floor**：R46.1 在任何依赖落地前做 stop/go pilot；no-go 时使用 repo-owned adapter，
  不以设计系统为由放弃 macOS 11/Safari 13 build target。
- **Primitive migration churn**：通过 wrapper-only import 与逐 primitive 迁移隔离；不把第三方 API 泄漏到 feature。
- **Native/renderer theme race**：native 持久化 + pre-window apply + enum-only pre-paint；以 cold-start smoke 验证而不是只测 React mount。
- **Material mobile bias**：冻结 compact desktop density、无 ripple/FAB、platform system type 与 coding-agent domain roles。
- **CSS dual-system debt**：旧 token 仅允许临时 alias，R46.6 必须清零；gate 阻止新增旧 token。
- **Visual refactor breaking run control**：presentation isolation test 与 existing real-serve regression 是每片 hard gate。
- **System WebView drift**：浏览器测试不冒充三平台；R46.7/8 记录每个平台真实状态。

### 16.2 Deferred

- Storybook/Chromatic：只有组件数量、协作规模或 review 流程证明 dev-only catalog 不足时另开 decision slice。
- User-selectable density、font size、custom accent、dynamic color 与 theme packs。
- Native macOS/Windows application menu、完整 Settings、command palette、多窗口 layout persistence。
- 自动化跨平台 pixel screenshot baseline；当前优先 geometry/behavior/contrast 与 real WebView evidence。
- TUI/Desktop 共用 theme package；两者交互媒介和配置 ownership 不同，不能只因颜色相似就强行合并。

## 17. R46.0 result

R46.0 已于 2026-07-20 完成设计冻结。调研与代码审计确认：Material 3 适合作为 theme role、state layer 和 adaptive
layout 的规范来源；Base UI 是值得验证的 React headless behavior 候选，但其 Safari 16.4+ 官方 baseline 与当前
`safari13`/macOS 11 产品基线不一致，必须先完成 R46.1 stop/go pilot；`@material/web` maintenance mode 和完整 MUI 的
styled runtime/迁移成本不适合作为当前主路径。

本 RFC 已明确区分 RFC-0045 的已完成 correctness foundation 与新的 consistency/usability 增量，冻结 native preference、
renderer resolved theme、A/C hybrid workbench、internal primitive boundary、density/contrast/fixture gate 和 R46.1-R46.8
commit dependency。R46.1 现在是唯一 ready 切片，其余切片按依赖保持 gated。
