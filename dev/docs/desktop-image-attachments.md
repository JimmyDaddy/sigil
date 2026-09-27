# Desktop 图片附件

Desktop 和 TUI 复用 kernel `ImageAttachment`、runtime `ControlledImageAttachmentCache` 与 application `SubmitPromptWithAttachments`。图片输入允许没有文字，纯空消息仍拒绝。每轮最多 4 张，每张 8 MiB、总计 24 MiB，沿用共享尺寸和视觉 token 上限。

同版本 shell/sidecar 握手 schema 为 15，`image_attachments` capability 表示实现了本机图片接口；不表示当前 provider 支持图片，模型能力只在图片操作时检查，不阻断普通文本工作流。

## 输入与权限

Desktop renderer 只调用 `desktop_pick_image`、`desktop_ingest_image`、`desktop_release_images`、`desktop_message_image`。文件选择由 native dialog 完成，native 打开用户明确选择的文件（允许符号链接目标），仅在 opened descriptor 确认 regular file 后读取有界 bytes；Unix nonblocking open 防止 FIFO 在类型检查前挂起；clipboard 只传 encoded bytes。renderer 不接收源路径、原始文件名、artifact reference、hash、bearer，也没有通用文件系统或 HTTP 权限。CSP 保持现有 `img-src 'self' data:`。

已认证的 `POST /image-attachments` 接收最多 8 MiB 的原始图片，runtime 依据真实字节识别类型、解码并导出元数据。其余 HTTP 请求仍使用原来的 body 上限。native 验证返回 hash/长度与上传 bytes 一致，生成 data URL 预览。draft registry 只存 bounded metadata：全进程最多 64 个引用、累计声明字节最多 384 MiB；单次发送仍使用共享 4 张/24 MiB 限制。显式移除、composer 退出和 workspace 关闭释放 native handle；持久图片只由既有 workspace controlled cache 管理，不新增长期 blob owner。

## 发送与恢复

renderer 发送 workspace-bound opaque handles，native 解析为已准入元数据；HTTP 经既有 application 命令、fresh route admission 进入 shared runtime。实际 provider/model 不支持图片时在接收运行前返回 typed rejection，composer 保留草稿；runtime 在最终装配时再确认实际 provider capability，防止配置变化。普通前台消息和 inline skill 使用同一已验证附件 input；技能仍执行 exact binding、trust 和 tool scope 校验。TUI 的 inline skill 命令也透传附件，允许没有文字参数，准入失败恢复技能 token 和图片。queue、原生 command、plan、agent/task durable 请求目前没有附件语义，入口保留草稿并说明当前能力范围；这不是权限政策，直接删除检查会丢图。

会话只持久化图片元数据，不记录编码内容。display 投影给 renderer 的引用只含 attachment ID、MIME、尺寸和大小。`GET /sessions/{session_id}/message-image` 通过 existing strict indexed source 校验 session + display ID + attachment ID，然后由受控 cache resolver 重新验证真实字节、hash、类型与尺寸。缺失、篡改或跨会话引用失败时保留文字并显示修复提示。

## 消融与回归

`image_ingress_ablation_removes_one_redundant_decode_and_preserves_recovery_validation` 使用真实 1920×1080 PNG，对照原先缓存命中后的重复解码与单次入口解码：每组 5 轮，检查元数据相同、decode 次数和耗时；恢复路径继续做完整解码，并有伪造尺寸 negative control。该优化没有移除字节/hash 校验。

相关回归覆盖 Composer 选择/粘贴/移除、未决和失败发送保留、image-only、历史查看；native 范围与配额；HTTP 实际 route capability 和 exact message 恢复；runtime cache 内容与持久化恢复；TUI image-only/inline-skill application bridge 和未准入恢复。执行记录位于 repo-local A3 execution artifact。

## 自定义本地 Responses 路线

已验证的 `custom` + loopback endpoint + `credential.source = "none"` 保持无凭证请求，
包括 Responses、compact 与 count 共用的 HTTP 装配。该选择由 runtime exact connection 转为
provider 私有的 runtime-only auth enum；provider JSON options 与缺失 API key 不能隐式开启它。
官方或配置为 Bearer 的路线仍要求凭据；无凭证路线不能指向远端，也不能取得官方 token-count
proof 资格。图片能力仍独立按实际 model 检查。本地脚本 provider 回归只证明传输/恢复契约，
不证明模型识图质量。
