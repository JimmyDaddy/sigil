# Portable compaction 的协议计数与恢复

本契约扩展既有 `cache_aware_v3`，复用原 session、provider physical attempt 和 portable lifecycle。
Desktop/HTTP 的 manual prepare 与 TUI 的 manual/pressure/overflow 使用同一 runtime 远端计数 helper；
不引入另一套压缩状态机、独立预算或后台重试 owner。

## 入口与成本

普通请求先使用已观察 usage 和内存中的 context metadata 判断是否需要压力准备。没有压力时不发起
summary 或 remote count。manual 本地 preview 同样没有 provider consumption；进入 semantic prepare
后，远端计数 profile 才执行一次摘要和 before/target 各一次计数。摘要失败时，manual/idle 保持原
history；只有既有 fit-required/overflow emergency 能使用带审计的确定性 continuity floor。

计数能力与价格证明分开。未知价格保持 unknown，不推导免费、不声称节省费用，也不开启 cost-only
automatic。queued pre-turn 的本地准入规则不因这次协议扩展而扩大。

## 当前远端材料

| Adapter profile | Exact count | 必要约束 |
| --- | --- | --- |
| 官方 OpenAI Responses | `/responses/input_tokens` | 既有官方 endpoint、固定 `gpt-4.1-2025-04-14` snapshot 和原 wire profile |
| 官方 DeepSeek Anthropic Messages | `/anthropic/v1/messages/count_tokens` | `https://api.deepseek.com/anthropic`、`deepseek-flash`、version `2023-06-01`、无 beta、显式 output 32768 |

DeepSeek Messages 的计数与生成使用相同 request builder、system/tools/history 和 thinking replay，
计数 body 只去掉 `stream`。DeepSeek 忽略显式 cache breakpoints，因此 adapter 只声明 implicit cache。
profile 不接受 hosted tools 或其它 provider continuation。远端响应限制为 64KiB，沿用 provider timeout，
无透明 retry。模型名称是移动 key，不伪造 immutable revision 或 system fingerprint；每个冻结材料都
重新计数。context metadata 1048576 只用于压力估算，不能代替 exact proof。

DeepSeek 官方 Responses 普通生成与计数是不同能力。2026-09-27 实测 `/responses/input_tokens` 和
`/v1/responses/input_tokens` 返回 404；不会借用 OpenAI 的 endpoint、tokenizer 或错误合同来声称支持。
这只限制缺少证明的自动压缩，不阻断该协议的普通生成。

## Before、target 与 source 绑定

`PortableCompactionRequestRole` 只表达 provider-neutral 的 `Before` 和 `Target`。Before 计量原输入，
可以超过窗口；target 同时验证 exact input、output reservation 和 safety buffer 下的完整 fit。
二者各产生一条真实 `InputTokenMeasurement` start/terminal，分别绑定冻结 fingerprint。preflight
只接受这两条目的明确、顺序明确的计数审计；source cursor、请求材料、continuation 或其它持久追加的
并发变化仍使 activation 失败。不能从模型自报 token 数建立证明。

取消、计数失败或进程重启不自动补发远端请求。未完成的 compaction 不激活；已原子激活的 portable
checkpoint 仍按既有 append-only restore 规则恢复。overflow 的消费确认、单次重试和禁止递归恢复
规则保持不变。

## Thinking 与 tool continuation

官方 DeepSeek Messages 默认 thinking。真实 tool-call stream 的 thinking blocks 通过已有
`ProviderContinuationState` 保存为 provider 私有、带版本的 opaque payload，由 kernel 绑定到真实
assistant message。后续请求和 count materialization 都从同一 durable payload 重建这些 blocks。
不把 DeepSeek 字段加入 kernel 公共 API，不关闭默认 thinking，也不把 reasoning 文本伪装成普通回答。

tool continuation 缺少对应 source、model/schema 不匹配或超过既有 continuation payload 上限时拒绝
该不完整请求。stream 在 `message_stop` 前中断不会生成可用 replay state。plain text turn、Claude
官方 route 及其它兼容 endpoint 保持各自原有合同。

## 验证边界

真实协议探测确认了 text/system/Unicode/tools/default-thinking 的 count 与 generation usage 相等，
以及保存真实 thinking 后 tool-result 续轮的相等性。完整 Sigil 的 provider、portable activation、
恢复和产品路径仍须通过各自集成测试；协议探测不能代替它们，也不能推导跨厂商成功率或价格收益。
本次执行、实际消融与未完成 gates 记录在 repo-local B1 execution ledger。
