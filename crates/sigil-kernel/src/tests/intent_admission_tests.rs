use std::collections::BTreeSet;
use std::fs;

use anyhow::Result;
use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::{
    AgentRole, EventClass, IntentPlanProposalV1, IntentProposalUnitV1, TaskId, TaskIsolationMode,
    TaskStepId, TaskStepMode, TaskStepSpec,
};

const PROPOSAL: &str = include_str!("../../../../dev/fixtures/intent-stack-v1/proposal.json");

fn session_at(path: &std::path::Path) -> Result<(JsonlSessionStore, Session)> {
    let store = JsonlSessionStore::new(path)?;
    let session = Session::load_from_store("test-provider", "test-model", store.clone())?;
    Ok((store, session))
}

fn suggested_admission(session: &Session) -> Result<IntentPlanAdmissionV1> {
    let proposal: IntentPlanProposalV1 = serde_json::from_str(PROPOSAL)?;
    let context = IntentAdmissionContextV1::initial(
        IntentStackId::new("stack-admission-1")?,
        "workspace-demo",
        session.session_scope_id(),
    )?;
    let authority = IntentAcceptanceAuthorityV1::explicit_user_confirmation(
        &proposal.source_turn_id,
        "decision-event-1",
        proposal.proposal_digest.clone(),
    )?;
    admit_suggested_decomposition(&context, &proposal, &authority)
}

fn root_admission(session: &Session) -> Result<IntentPlanAdmissionV1> {
    let context = IntentAdmissionContextV1::initial(
        IntentStackId::new("stack-chat-root-1")?,
        "workspace-demo",
        session.session_scope_id(),
    )?;
    let authority =
        IntentAcceptanceAuthorityV1::user_declared_root("turn-chat-1", "user-message-event-1")?;
    admit_user_declared_root(
        &context,
        UserDeclaredIntentV1 {
            title: "Fix retry timeout".to_owned(),
            statement: "Fix the retry timeout in the existing request path.".to_owned(),
            acceptance_criteria: vec![IntentProposalCriterionV1 {
                criterion_alias: "root-check".to_owned(),
                statement: "The retry timeout is applied and the relevant check passes.".to_owned(),
                required: true,
            }],
        },
        &authority,
    )
}

fn accepted_task_plan() -> Result<TaskPlanEntry> {
    Ok(TaskPlanEntry {
        task_id: TaskId::new("task-intent-admission-1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("implement")?,
            title: "Implement accepted intents".to_owned(),
            display_name: None,
            detail: Some("Apply the accepted plan in the bound workspace.".to_owned()),
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            mode: Some(TaskStepMode::Write),
            isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
        }],
        reason: Some("accepted with the IntentPlan".to_owned()),
    })
}

#[test]
fn provider_proposal_cannot_serialize_or_supply_acceptance_authority() -> Result<()> {
    let proposal: IntentPlanProposalV1 = serde_json::from_str(PROPOSAL)?;
    let mut forged: serde_json::Value = serde_json::from_str(PROPOSAL)?;
    forged
        .as_object_mut()
        .expect("proposal fixture is an object")
        .insert(
            "acceptance_kind".to_owned(),
            json!("explicit_user_confirmation"),
        );
    assert!(
        serde_json::from_value::<IntentPlanProposalV1>(forged).is_err(),
        "provider JSON cannot extend the proposal schema with acceptance"
    );

    let context = IntentAdmissionContextV1::initial(
        IntentStackId::new("stack-proposal-authority")?,
        "workspace-demo",
        "session-demo",
    )?;
    let wrong_digest_authority = IntentAcceptanceAuthorityV1::explicit_user_confirmation(
        "turn-1",
        "decision-event-1",
        IntentDigest::new(format!(
            "{}{}",
            crate::INTENT_CANONICAL_DIGEST_PREFIX,
            "f".repeat(64)
        ))?,
    )?;
    assert!(
        admit_suggested_decomposition(&context, &proposal, &wrong_digest_authority)
            .expect_err("wrong proposal digest must not grant acceptance")
            .to_string()
            .contains("exact proposal")
    );
    let root_authority =
        IntentAcceptanceAuthorityV1::user_declared_root("turn-1", "user-message-event-1")?;
    assert!(
        admit_suggested_decomposition(&context, &proposal, &root_authority)
            .expect_err("user-root authority cannot accept a decomposition")
            .to_string()
            .contains("exact proposal")
    );

    let exact_authority = IntentAcceptanceAuthorityV1::explicit_user_confirmation(
        "turn-1",
        "decision-event-2",
        proposal.proposal_digest.clone(),
    )?;
    let admission = admit_suggested_decomposition(&context, &proposal, &exact_authority)?;
    admission.plan().validate_contract()?;
    assert_eq!(
        admission.acceptance_kind(),
        IntentAcceptanceKind::ExplicitUserConfirmation
    );
    assert!(
        admission
            .plan()
            .intents
            .iter()
            .all(|intent| !matches!(intent.intent_ref.intent_id.as_str(), "retry" | "telemetry")),
        "runtime ids must not reuse provider aliases"
    );
    Ok(())
}

#[test]
fn suggested_admission_properties_hold_across_bounded_graph_sizes() -> Result<()> {
    for count in 1..=16 {
        let mut proposal = IntentPlanProposalV1 {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            proposal_id: format!("proposal-{count}"),
            source_turn_id: "turn-property".to_owned(),
            intents: (0..count)
                .map(|index| IntentProposalUnitV1 {
                    intent_alias: format!("intent-{index}"),
                    title: format!("Intent {index}"),
                    statement: format!("Implement bounded intent {index}."),
                    acceptance_criteria: vec![IntentProposalCriterionV1 {
                        criterion_alias: format!("criterion-{index}"),
                        statement: format!("Intent {index} has deterministic evidence."),
                        required: true,
                    }],
                    depends_on_aliases: if index > 0 {
                        vec![format!("intent-{}", index - 1)]
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
            proposal_digest: IntentDigest::new(format!(
                "{}{}",
                crate::INTENT_CANONICAL_DIGEST_PREFIX,
                "0".repeat(64)
            ))?,
        };
        proposal.proposal_digest = proposal.computed_digest()?;
        proposal.validate_contract()?;
        let context = IntentAdmissionContextV1::initial(
            IntentStackId::new(format!("stack-property-{count}"))?,
            "workspace-property",
            "session-property",
        )?;
        let authority = IntentAcceptanceAuthorityV1::explicit_user_confirmation(
            "turn-property",
            format!("decision-property-{count}"),
            proposal.proposal_digest.clone(),
        )?;
        let admission = admit_suggested_decomposition(&context, &proposal, &authority)?;
        admission.plan().validate_contract()?;
        let ids = admission
            .plan()
            .intents
            .iter()
            .map(|intent| intent.intent_ref.intent_id.as_str())
            .collect::<BTreeSet<_>>();
        let criterion_ids = admission
            .plan()
            .intents
            .iter()
            .flat_map(|intent| {
                intent
                    .acceptance_criteria
                    .iter()
                    .map(|criterion| criterion.criterion_id.as_str())
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), count);
        assert_eq!(criterion_ids.len(), count);
        assert!(ids.iter().chain(criterion_ids.iter()).all(|id| {
            id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }));
    }
    Ok(())
}

#[test]
fn task_and_intent_acceptance_share_one_ordered_durable_batch_and_replay() -> Result<()> {
    let temp = tempdir()?;
    let path = temp.path().join("session.jsonl");
    let (store, mut session) = session_at(&path)?;
    let admission = suggested_admission(&session)?;
    let task_plan = accepted_task_plan()?;

    let first = append_task_intent_plan_admission(&mut session, &admission, task_plan.clone())?;
    assert!(first.appended);
    let second = append_task_intent_plan_admission(&mut session, &admission, task_plan.clone())?;
    assert!(!second.appended, "exact retry must be idempotent");
    let mut conflicting_task_plan = task_plan.clone();
    conflicting_task_plan.plan_version = 2;
    assert!(
        append_task_intent_plan_admission(&mut session, &admission, conflicting_task_plan,)
            .expect_err("one accepted IntentPlan cannot be silently rebound")
            .to_string()
            .contains("second semantic IntentPlan")
    );

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let tail = records
        .iter()
        .rev()
        .take(4)
        .map(|record| record.stored_event().event_kind())
        .collect::<Vec<_>>();
    assert_eq!(
        tail,
        vec![
            Some(DurableEventType::TaskStatusChanged),
            Some(DurableEventType::IntentPlanAccepted),
            Some(DurableEventType::IntentPlanRecorded),
            Some(DurableEventType::IntentStackCreated),
        ]
    );

    let projection = IntentStackProjectionV1::from_records(&records)?;
    assert!(!projection.has_incomplete_task_acceptance());
    let accepted = projection
        .latest_accepted_plan()
        .expect("batch replay should accept the IntentPlan");
    assert_eq!(accepted.plan, *admission.plan());
    assert_eq!(
        accepted.task_plan_binding,
        Some(IntentTaskPlanBindingV1 {
            task_id: task_plan.task_id.as_str().to_owned(),
            task_plan_version: task_plan.plan_version,
        })
    );
    assert!(matches!(
        projection.public_state()?,
        PublicIntentStackStateV1::Available { .. }
    ));
    let task_projection = session.task_state_projection();
    assert_eq!(
        task_projection
            .tasks
            .get(&task_plan.task_id)
            .and_then(|task| task.plans.get(&task_plan.plan_version))
            .map(|plan| plan.status),
        Some(TaskPlanStatus::Accepted)
    );

    drop(session);
    drop(store);
    let (_reopened_store, reopened) = session_at(&path)?;
    assert_eq!(reopened.intent_stack_projection()?, projection);
    assert!(matches!(
        reopened.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { .. }
    ));
    Ok(())
}

#[test]
fn crash_prefix_without_task_plan_never_activates_task_bound_intent() -> Result<()> {
    let temp = tempdir()?;
    let path = temp.path().join("session.jsonl");
    {
        let (_store, mut session) = session_at(&path)?;
        let admission = suggested_admission(&session)?;
        append_task_intent_plan_admission(&mut session, &admission, accepted_task_plan()?)?;
    }

    let content = fs::read_to_string(&path)?;
    let mut lines = content.lines().collect::<Vec<_>>();
    let removed = lines.pop().expect("batch has a TaskPlan tail");
    let removed_event = crate::StoredEvent::from_json_str(removed)?;
    assert_eq!(
        removed_event.event_kind(),
        Some(DurableEventType::TaskStatusChanged)
    );
    let truncated = format!("{}\n", lines.join("\n"));
    fs::write(&path, truncated)?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    let projection = IntentStackProjectionV1::from_records(&records)?;
    assert!(projection.has_incomplete_task_acceptance());
    assert!(projection.latest_accepted_plan().is_none());
    assert!(
        projection
            .public_state()
            .expect_err("incomplete admission must not render as accepted")
            .to_string()
            .contains("incomplete")
    );

    let (_store, reopened) = session_at(&path)?;
    assert!(
        !reopened
            .task_state_projection()
            .tasks
            .contains_key(&accepted_task_plan()?.task_id),
        "without the final TaskPlan record no task participant can be admitted"
    );
    assert!(
        reopened
            .intent_stack_projection()?
            .has_incomplete_task_acceptance()
    );
    Ok(())
}

#[test]
fn chat_root_admission_is_durable_idempotent_and_has_no_task_authority() -> Result<()> {
    let temp = tempdir()?;
    let path = temp.path().join("session.jsonl");
    let (_store, session) = session_at(&path)?;
    let admission = root_admission(&session)?;
    let first = append_chat_root_intent_admission(&session, &admission)?;
    let second = append_chat_root_intent_admission(&session, &admission)?;
    assert!(first.appended);
    assert!(!second.appended);
    let projection = session.intent_stack_projection()?;
    let accepted = projection
        .latest_accepted_plan()
        .expect("chat root should become accepted");
    assert_eq!(
        accepted.acceptance_kind,
        IntentAcceptanceKind::UserDeclaredRootAdmission
    );
    assert!(accepted.task_plan_binding.is_none());
    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { .. }
    ));
    Ok(())
}

#[test]
fn old_session_projects_explicit_history_unavailable_state() -> Result<()> {
    let temp = tempdir()?;
    let path = temp.path().join("legacy-session.jsonl");
    let (_store, session) = session_at(&path)?;
    let projection = session.intent_stack_projection()?;
    assert!(projection.latest_accepted_plan().is_none());
    assert_eq!(
        projection.public_state()?,
        PublicIntentStackStateV1::HistoryUnavailable {
            schema_version: INTENT_PUBLIC_DTO_SCHEMA_VERSION,
            safe_message: INTENT_HISTORY_UNAVAILABLE_MESSAGE.to_owned(),
        }
    );
    Ok(())
}

#[test]
fn durable_intent_event_decoder_rejects_wire_payload_mismatch_and_future_slice() -> Result<()> {
    assert_eq!(
        DurableEventType::from_event_type("intent_stack_created"),
        Some(DurableEventType::IntentStackCreated)
    );
    assert_eq!(
        DurableEventType::from_event_type("intent_plan_recorded"),
        Some(DurableEventType::IntentPlanRecorded)
    );
    assert_eq!(
        DurableEventType::from_event_type("intent_plan_accepted"),
        Some(DurableEventType::IntentPlanAccepted)
    );
    assert_eq!(
        DurableEventType::from_event_type("intent_execution_bound"),
        None,
        "R51.2 event types must remain unregistered"
    );

    let root_plan: IntentPlanV1 = serde_json::from_str(include_str!(
        "../../../../dev/fixtures/intent-stack-v1/user-root-plan.json"
    ))?;
    let mismatched = crate::StoredEvent::new(
        DurableEventType::IntentStackCreated,
        EventClass::Critical,
        "event-intent-mismatch".to_owned(),
        "session-chat".to_owned(),
        1,
        serde_json::to_value(IntentEventV1::PlanRecorded {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            plan: root_plan,
        })?,
    )?;
    assert!(
        decode_typed_stored_event(mismatched)
            .expect_err("wire type must bind the exact Intent variant")
            .to_string()
            .contains("mismatched")
    );

    let future = crate::StoredEvent::new(
        DurableEventType::IntentPlanRecorded,
        EventClass::Critical,
        "event-intent-future".to_owned(),
        "session-chat".to_owned(),
        1,
        serde_json::to_value(IntentEventV1::ChangeSetBound {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            intent_ref: IntentVersionRef::new(IntentId::new("root")?, 1)?,
            execution_id: crate::IntentExecutionId::new("execution-1")?,
            changeset_ids: vec!["changeset-1".to_owned()],
        })?,
    )?;
    assert!(
        decode_typed_stored_event(future)
            .expect_err("later-slice Intent events must remain unregistered")
            .to_string()
            .contains("later RFC-0051 slice")
    );
    Ok(())
}
