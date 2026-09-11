use super::*;
use crate::app::{PendingPlanApproval, PlanActionFeedback, PlanWorkbenchAction};

pub(super) fn ready_plan() -> Result<(
    AppState,
    sigil_kernel::Session,
    sigil_kernel::PlanDraftCreatedEntry,
)> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let snapshot = sigil_runtime::plan_handoff_workspace_snapshot_id(
        app.config_snapshot.as_ref().expect("test configuration"),
        &app.workspace_root,
    )?;
    let mut session = sigil_kernel::Session::new("plan-workbench", "planned-model");
    let request = sigil_runtime::PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut session,
        "Update README",
        "plan-workbench",
        snapshot.clone(),
        1,
    )?;
    let mut handler = sigil_kernel::NoopEventHandler;
    sigil_runtime::PlanReviewCoordinator::ensure_attempt_started(
        &mut session,
        &request,
        &mut handler,
        2,
    )?;
    let draft = sigil_kernel::plan_draft_created_entry_with_plan_id(
        request.plan_id.clone(),
        &super::worker_bridge_tests::structured_plan_text(
            "Update README",
            "Review the documentation",
            "README.md",
        ),
        request.plan_source_ref(),
        3,
        snapshot,
    )?
    .expect("structured draft");
    sigil_runtime::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &request,
        &mut handler,
        4,
    )?;
    app.sync_current_session_state(session.entries().to_vec());
    settle_session_auxiliary(&mut app);
    app.restore_durable_attention_surfaces();
    assert!(!app.pending_plan_approval().expect("current plan").stale);
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    Ok((app, session, draft))
}

#[test]
fn plan_save_keeps_review_pending_and_retries_after_exact_failure() -> Result<()> {
    let (mut app, mut session, draft) = ready_plan()?;
    let save_key = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(
        app.handle_key_event(save_key)?,
        Some(AppAction::SavePlan { .. })
    ));
    assert!(app.handle_key_event(save_key)?.is_none());
    let pending = app.pending_plan_approval().expect("review remains open");
    assert!(pending.workbench_open);
    assert_eq!(
        pending.action_feedback,
        Some(PlanActionFeedback::Pending(PlanWorkbenchAction::Save))
    );
    assert!(!pending.workbench_key_hint().contains("S save"));

    app.restore_durable_attention_surfaces();
    assert_eq!(
        app.pending_plan_approval()
            .expect("refreshed review")
            .action_feedback,
        Some(PlanActionFeedback::Pending(PlanWorkbenchAction::Save))
    );
    assert!(!app.fail_pending_plan_action(
        sigil_kernel::PublicPlanAction::Save,
        draft.plan_id.as_str(),
        "another-hash",
        "stale failure".to_owned()
    ));
    assert!(app.fail_pending_plan_action(
        sigil_kernel::PublicPlanAction::Save,
        draft.plan_id.as_str(),
        &draft.plan_hash,
        "storage is unavailable".to_owned()
    ));
    let pending = app
        .pending_plan_approval()
        .expect("failed save remains reviewable");
    assert!(pending.workbench_open);
    assert!(pending.action_enabled(PlanWorkbenchAction::Save));
    assert!(
        matches!(&pending.action_feedback, Some(PlanActionFeedback::Failed { message, .. })
        if message == "storage is unavailable")
    );

    assert!(matches!(
        app.handle_key_event(save_key)?,
        Some(AppAction::SavePlan { .. })
    ));
    let saved = sigil_runtime::PlanReviewCoordinator::record_plan_decision(
        &mut session,
        &sigil_runtime::PlanDecisionCommand {
            plan_id: draft.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash.clone(),
            decision: sigil_kernel::PlanDecision::SavedOnly,
        },
        5,
    )?;
    app.handle_worker_message(WorkerMessage::PlanSaved {
        entry: saved,
        entries: session.entries().to_vec(),
    })?;
    let pending = app
        .pending_plan_approval()
        .expect("saved plan stays available");
    assert!(pending.workbench_open);
    assert_eq!(pending.status_label(), "saved for later");
    assert!(matches!(
        pending.action_feedback,
        Some(PlanActionFeedback::Succeeded {
            action: PlanWorkbenchAction::Save,
            ..
        })
    ));
    assert!(pending.action_enabled(PlanWorkbenchAction::Run));
    assert!(!app.runtime.is_busy);
    Ok(())
}

#[test]
fn queued_revision_keeps_the_original_read_only_across_refreshes() -> Result<()> {
    let (mut app, mut session, draft) = ready_plan()?;
    let request = sigil_runtime::PlanReviewCoordinator::request_plan_revision_guidance(
        &mut session,
        &draft.plan_id,
        &draft.plan_hash,
        6,
    )?;
    let (_, revision) = sigil_runtime::PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: request.request.identity,
            request_hash: request.request_hash,
            command_id: sigil_kernel::UserInputCommandId::new("workbench-guidance-command")?,
            decision: sigil_kernel::UserInputDecisionV1::Submitted {
                answers: vec![sigil_kernel::UserInputAnswerV1 {
                    question_id: "revision_guidance".to_owned(),
                    value: sigil_kernel::UserInputAnswerValueV1::Text {
                        value: "Keep the public contract stable.".to_owned(),
                    },
                }],
            },
        },
        draft.workspace_snapshot_id.clone(),
        7,
    )?;
    let revision = revision.expect("revision dispatch request");
    app.sync_current_session_state(session.entries().to_vec());
    settle_session_auxiliary(&mut app);
    app.restore_durable_attention_surfaces();
    let pending = app.pending_plan_approval().expect("original plan");
    assert_eq!(pending.plan_id.as_deref(), Some(draft.plan_id.as_str()));
    assert!(pending.workbench_open);
    assert_eq!(pending.status_label(), "revision queued");
    assert!(
        pending
            .revision_detail()
            .expect("revision explanation")
            .contains("read-only")
    );
    assert!(pending.allowed_actions.is_empty());
    assert!(!pending.workbench_key_hint().contains("S save"));
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))?
            .is_none()
    );
    sigil_runtime::PlanReviewCoordinator::record_unstarted_plan_revision_failure(
        &mut session,
        &revision,
        "dispatch rejected before start",
        8,
    )?;
    app.sync_current_session_state(session.entries().to_vec());
    settle_session_auxiliary(&mut app);
    app.restore_durable_attention_surfaces();
    let pending = app
        .pending_plan_approval()
        .expect("original plan after failed dispatch");
    assert_eq!(pending.status_label(), "revision failed");
    assert!(pending.workbench_open);
    assert!(pending.action_enabled(PlanWorkbenchAction::Save));
    Ok(())
}

#[test]
fn new_plan_in_same_review_keeps_workbench_open_without_starting() -> Result<()> {
    let (mut app, _session, _draft) = ready_plan()?;
    let mut previous = app
        .composer
        .pending_plan_approval
        .take()
        .expect("current plan");
    previous.workbench_scroll = 20;
    previous.action_feedback = Some(PlanActionFeedback::Pending(PlanWorkbenchAction::Revise));
    let mut revised = previous.clone();
    revised.plan_id = Some("revised-plan".to_owned());
    revised.plan_hash = "revised-hash".to_owned();
    revised.workbench_open = false;
    revised.workbench_scroll = 0;
    revised.action_feedback = None;
    app.composer.pending_plan_approval = Some(revised);
    app.restore_pending_plan_presentation(Some(previous));
    let pending = app.pending_plan_approval().expect("new plan");
    assert!(pending.workbench_open);
    assert_eq!(pending.workbench_scroll, 0);
    assert!(
        matches!(&pending.action_feedback, Some(PlanActionFeedback::Succeeded {
        action: PlanWorkbenchAction::Revise, message
    }) if message.contains("not been started"))
    );
    assert!(!app.runtime.is_busy);
    Ok(())
}

#[test]
fn adopted_plan_refresh_does_not_restore_obsolete_pending_feedback() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut previous = PendingPlanApproval::test_fixture(
        "adopt-plan",
        "Plan body",
        "hash",
        "Plan",
        vec![],
        vec![],
        vec![],
    );
    previous.status = Some(sigil_kernel::PublicPlanReviewStatus::CompileFailed);
    previous.workbench_open = true;
    previous.action_feedback = Some(PlanActionFeedback::Pending(
        PlanWorkbenchAction::AdoptCandidate,
    ));
    let mut ready = previous.clone();
    ready.status = Some(sigil_kernel::PublicPlanReviewStatus::DraftReady);
    ready.action_feedback = None;
    app.composer.pending_plan_approval = Some(ready);
    app.restore_pending_plan_presentation(Some(previous));
    let pending = app.pending_plan_approval().expect("adopted plan");
    assert!(pending.workbench_open);
    assert!(matches!(
        pending.action_feedback,
        Some(PlanActionFeedback::Succeeded {
            action: PlanWorkbenchAction::AdoptCandidate,
            ..
        })
    ));
    assert!(pending.action_enabled(PlanWorkbenchAction::Run));
}
