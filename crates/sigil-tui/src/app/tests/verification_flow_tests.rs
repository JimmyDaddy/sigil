use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use sigil_kernel::{
    AgentRole, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, ControlEntry, EvidenceScope, ModelMessage, ReadinessEvaluatedEntry,
    ReadinessEvaluation, RequiredAction, RunStatus, SessionLogEntry, SessionRef, TaskId,
    TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId,
    TaskStepSpec, TaskStepStatus, ToolEffect, TrustedCheckSpec, VerificationVerdict,
    VisibleCompletionState,
};

use super::*;
use crate::{
    app::tests::common::test_config,
    mouse::{AppMouseOutcome, HitTarget, MouseInput, MouseInputKind},
    ui::LayoutSnapshot,
    view_model::UiViewModel,
};

#[test]
fn verification_card_keyboard_focus_inspect_and_exact_action() {
    let mut app = verification_app();

    let focus = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT))
        .expect("focus key");
    assert!(focus.is_none());
    assert!(app.verification_card_focused());
    assert!(
        UiViewModel::from_app(&app)
            .footer
            .hints
            .contains("I inspect")
    );

    let inspect = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
        .expect("inspect key");
    assert!(inspect.is_none());
    assert!(app.verification_inspect_open());

    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .expect("run key")
        .expect("rerun action");
    let AppAction::RerunTaskVerification { request } = action else {
        panic!("expected exact rerun action");
    };
    assert_eq!(request.task_id.as_str(), "task_1");
    assert_eq!(request.step_id.as_str(), "step_1");
    assert_eq!(request.check_spec_id, "cargo-test");
    assert_eq!(request.policy_hash, "policy-hash");
    assert_eq!(request.workspace_snapshot_id, "snapshot-1");

    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .expect("blur key");
    assert!(!app.verification_card_focused());
    assert!(!app.verification_inspect_open());
}

#[test]
fn verification_card_layout_and_mouse_focus_are_exact() {
    let mut app = verification_app();
    app.set_terminal_size(120, 32);
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 32), &app);
    let area = layout.verification_card.expect("verification hit area");
    assert_eq!(
        layout.hit_target(area.x, area.y),
        HitTarget::VerificationCard
    );

    let outcome = app
        .handle_mouse_event(
            MouseInput {
                column: area.x,
                row: area.y,
                kind: MouseInputKind::LeftDown,
                modifiers: KeyModifiers::NONE,
            },
            &layout,
        )
        .expect("mouse focus");
    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.verification_card_focused());
}

#[test]
fn verification_card_focus_clears_when_the_card_disappears() {
    let mut app = verification_app();
    assert!(app.focus_verification_card());
    app.session_browser.current_entries.clear();

    let outcome = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
        .expect("stale focus key");

    assert!(outcome.is_none());
    assert!(!app.verification_card_focused());
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input, "i");
}

#[test]
fn checkpoint_restore_marks_existing_verification_card_stale_without_old_action() {
    let mut app = verification_app();
    app.latest_checkpoint_restore_sequence = Some(10);
    app.readiness_sequences_by_scope
        .insert(EvidenceScope::Run("unrelated".to_owned()), 11);

    let card = app
        .task_strip_view()
        .and_then(|view| view.verification)
        .expect("verification card");

    assert_eq!(card.status, "stale after checkpoint restore");
    assert_eq!(
        card.why.as_deref(),
        Some("workspace changed; refresh verification evidence")
    );
    assert!(card.action.is_none());
    assert!(card.inspect_lines.iter().any(|line| line.contains("newer")));

    app.readiness_sequences_by_scope
        .insert(EvidenceScope::Step("task_1:step_1".to_owned()), 12);
    let refreshed = app
        .task_strip_view()
        .and_then(|view| view.verification)
        .expect("refreshed verification card");
    assert_ne!(refreshed.status, "stale after checkpoint restore");
    assert!(refreshed.action.is_some());
}

fn verification_app() -> AppState {
    let mut app = AppState::from_root_config(Path::new("config.toml"), &test_config());
    app.session_browser.current_entries = verification_entries();
    app
}

fn verification_entries() -> Vec<SessionLogEntry> {
    let task_id = TaskId::new("task_1").expect("task id");
    let step_id = TaskStepId::new("step_1").expect("step id");
    let check = CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        "task_step_default",
    );
    let trusted = TrustedCheckSpec {
        check_spec: check,
        source: CheckDiscoverySource::UserExplicitConfig,
        workspace_trust_snapshot_id: "trust-1".to_owned(),
        promoted_by: CheckPromotion::ExplicitUserConfig {
            config_event_id: "config-verification".to_owned(),
        },
        approval_event_id: None,
        sandbox_decision_id: None,
    };
    vec![
        SessionLogEntry::User(ModelMessage::user("/task verify")),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl").expect("session ref"),
            objective: "Verify changes".to_owned(),
            status: TaskRunStatus::Paused,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Run checks".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Blocked,
            title: Some("Run checks".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(
            CheckSpecRecordedEntry::new(
                EvidenceScope::Task(task_id.as_str().to_owned()),
                trusted,
                "config-verification",
            ),
        )),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(ReadinessEvaluatedEntry {
            scope: EvidenceScope::Step(format!("{}:{}", task_id.as_str(), step_id.as_str())),
            evaluation: ReadinessEvaluation {
                run_status: RunStatus::Completed,
                verification_verdict: VerificationVerdict::Missing,
                visible_state: VisibleCompletionState::NeedsUser,
                reasons: Vec::new(),
                required_actions: vec![RequiredAction::RunCheck {
                    check_spec_id: "cargo-test".to_owned(),
                }],
            },
            policy_hash: Some("policy-hash".to_owned()),
            workspace_snapshot_id: Some("snapshot-1".to_owned()),
        })),
    ]
}
