# 普通会话恢复资格与内部计划研究记录

普通恢复目录用于选择可独立继续的用户会话。PlanReview research child 的 JSONL 是有效且必须保留的审计记录，但不具备独立普通对话的恢复资格；它的恢复由所属 PlanReview 协调器负责。

新 research child 在开始研究前追加 `SessionPurposeBoundV1`，绑定 `PlanReviewResearch` 用途、父会话、review、attempt 和 Plan 身份。同一 child 重开必须匹配原绑定，不能重复改变用途。没有用途记录的普通历史会话继续按 Conversation 处理；缺少 route 不作为内部用途的判断依据。

已有研究记录使用精确 typed lineage 兼容：Plan draft/candidate 的 review source turn 指向另一父会话；或已验证父 `PlanReviewAttempt` 的 canonical child ref 与 authority namespace 的精确对应。父会话中指回自身的同一 Plan 不会被当成研究记录。不检查标题、语言、消息数或十六进制文件名来猜用途。

目录投影用 `Internal` 区分有效内部记录与 `Invalid` 损坏来源，保留原身份和审计源。它不计入 degraded 来源，也不能使用 invalid-source cleanup；普通目录查询不返回它。SQLite 继续使用原 `source_state` 列，投影 revision 递增后从 durable stream 重新分类，不迁移或重写研究日志。

精确 reopen 与 route inspection 同样检查资格。内部记录返回专门的内部研究拒绝或 `NotReady(Internal)`，不把 route 缺失伪装成损坏。未显式绑定用途的普通 route 缺失会话仍保留在目录，继续受到原 route、trust、权限及 authority 校验。

TUI 切换在目标激活前保留原 owner。内部记录、非法 stream 或其它激活拒绝不得转移原 attachment；拒绝提示不把健康 worker 设成不可用。真正需要确认/替换 route 的目标保留原 recovery binding 和动作，并额外保留原会话快照与 attachment 作为失败切换的返回上下文。用户从该恢复状态选择原会话、其它会话或 `/new` 时，先通过已有 runtime maintenance 正常 join 旧 worker、重建原 application owner并等待真实 Ready；选择其它目标或新建会话的意图随后走原 application prepare/admission/CAS。返回不伪造 Switch settled，不把目标 binding 用在原会话，不绕过 route 或独占 attachment 检查。

回归入口：

```sh
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-runtime internal_research_stream_is_retained_but_not_a_conversation_catalog_entry
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-runtime historical_plan_lineage_hides_only_research_child_and_keeps_route_missing_parent
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-runtime historical_attempt_lineage_hides_managed_research_before_a_draft_exists
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-runtime managed_plan_review_research_controls_stay_in_the_child_session
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host switch_to_internal_research_rejects_without_retiring_current_owner
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host real_route_recovery_retains_origin_attachment_and_can_return_without_losing_binding
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host route_recovery_return_rebuilds_original_application_before_another_switch
python3 scripts/run-isolated-tests.py -- cargo test -p sigil-tui-host route_recovery_new_session_rebuilds_original_application_before_new_admission
```

测试仅证明对应的隔离状态和宿主链路。原生 Plan → 历史列表 → 显式内部记录拒绝 → 原对话继续、真实 route 恢复后返回，以及退出重启的资格需另由不可变候选实机证据确认。
