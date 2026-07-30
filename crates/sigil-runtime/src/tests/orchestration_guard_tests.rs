use anyhow::Result;
use sigil_kernel::{
    ControlEntry, ModelMessage, MultiAgentMode, OrchestrationHardInvariant, Session,
    SessionLogEntry, TaskConfig, TaskHandoffId, TaskHandoffRequestedEntry, TaskRoutingPolicy,
};

use super::{OrchestrationRouteGuard, orchestration_observation};
use crate::ConversationCoordinator;

#[test]
fn duplicate_handoff_disables_only_the_exact_route_and_build() -> Result<()> {
    let mut session = Session::new("provider", "model");
    let source =
        sigil_kernel::ConversationTurnRef::new(session.session_scope_id(), "message-1", "run-1")?;
    let duplicate = TaskHandoffRequestedEntry {
        handoff_id: TaskHandoffId::new("handoff-1")?,
        source_turn: source,
        trigger: sigil_kernel::TaskAdmissionTrigger::ModelRequested,
        reason_codes: vec![sigil_kernel::TaskAdmissionReason::MultiStageChange],
        recovery_objective: None,
        policy_snapshot_hash: format!("sha256:{}", "a".repeat(64)),
        requested_at_ms: 1,
    };
    session.append_control(ControlEntry::TaskHandoffRequested(duplicate.clone()))?;
    session.append_control(ControlEntry::TaskHandoffRequested(duplicate))?;

    let guard = OrchestrationRouteGuard::new("provider", "model", "build-1");
    assert_eq!(
        guard.effective_policy(&session, TaskRoutingPolicy::Auto),
        TaskRoutingPolicy::Manual,
        "preflight must not freeze an auto-routing request that enforce will disable"
    );
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::OrchestrationRouteDisabled(_))
    )));
    let disabled = guard
        .enforce(&mut session, 2)?
        .expect("duplicate handoff must disable the route");
    assert_eq!(
        disabled.invariant,
        OrchestrationHardInvariant::DuplicateHandoff
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::OrchestrationRouteDisabled(_))
            ))
            .count(),
        1
    );
    guard.enforce(&mut session, 3)?;
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::OrchestrationRouteDisabled(_))
            ))
            .count(),
        1,
        "rechecking a disabled route must not append duplicate kill-switch facts"
    );
    assert_eq!(
        guard.effective_policy(&session, TaskRoutingPolicy::Auto),
        TaskRoutingPolicy::Manual
    );
    assert_eq!(
        guard.effective_multi_agent_mode(&session, MultiAgentMode::Proactive),
        MultiAgentMode::ExplicitRequestOnly
    );
    let mut effective_task = TaskConfig {
        routing_policy: TaskRoutingPolicy::Auto,
        multi_agent_mode: MultiAgentMode::Proactive,
        ..TaskConfig::default()
    };
    guard.apply_effective_task_config(&session, &mut effective_task);
    assert_eq!(effective_task.routing_policy, TaskRoutingPolicy::Manual);
    assert_eq!(
        effective_task.multi_agent_mode,
        MultiAgentMode::ExplicitRequestOnly
    );
    assert_eq!(
        OrchestrationRouteGuard::new("provider", "other-model", "build-1")
            .effective_policy(&session, TaskRoutingPolicy::Auto),
        TaskRoutingPolicy::Auto
    );
    assert_eq!(
        OrchestrationRouteGuard::new("provider", "other-model", "build-1")
            .effective_multi_agent_mode(&session, MultiAgentMode::Proactive),
        MultiAgentMode::Proactive
    );
    assert_eq!(
        OrchestrationRouteGuard::new("provider", "model", "build-2")
            .effective_policy(&session, TaskRoutingPolicy::Auto),
        TaskRoutingPolicy::Auto
    );
    Ok(())
}

#[test]
fn coordinator_with_disabled_route_exposes_no_automatic_handoff() -> Result<()> {
    let mut session = Session::new("provider", "model");
    let duplicate = duplicate_final_entry()?;
    session.append_control(duplicate.clone())?;
    session.append_control(duplicate)?;
    let guard = OrchestrationRouteGuard::new("provider", "model", "build-1");
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_orchestration_route_guard(guard);
    coordinator.enforce_orchestration_route_kill_switch(&mut session, 2)?;

    let input = sigil_kernel::AgentRunInput::user("ordinary prompt");
    let bound = coordinator.bind_conversation_input(
        &session,
        input,
        sigil_kernel::SessionRef::new_relative("session.jsonl")?,
        "run-1",
        None,
        3,
    )?;
    let Some(sigil_kernel::AgentRunPurpose::Conversation(context)) = bound.purpose else {
        panic!("conversation purpose must be present");
    };
    assert_eq!(context.routing_policy, TaskRoutingPolicy::Manual);
    assert!(context.task_handoff.is_none());
    Ok(())
}

#[test]
fn orchestration_observation_never_classifies_user_prompt_text() {
    let mut session = Session::new("provider", "model");
    session
        .append_user_message(ModelMessage::user(
            "duplicate handoff spawn continuation merge wait_agent",
        ))
        .expect("append user message");

    assert_eq!(
        orchestration_observation(&session),
        sigil_kernel::OrchestrationEvalObservationV1::default()
    );
}

fn duplicate_final_entry() -> Result<ControlEntry> {
    Ok(ControlEntry::TaskFinalAnswerCommitted(
        sigil_kernel::TaskFinalAnswerCommittedEntry {
            task_id: sigil_kernel::TaskId::new("task-1")?,
            plan_version: 1,
            synthesis_attempt_id: sigil_kernel::TaskParticipantAttemptId::new("synthesis-1")?,
            message_id: "message-final".to_owned(),
            content_hash: format!("sha256:{}", "a".repeat(64)),
        },
    ))
}
