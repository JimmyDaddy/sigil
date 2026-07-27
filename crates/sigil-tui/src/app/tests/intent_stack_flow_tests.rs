use ratatui::layout::Rect;
use sigil_kernel::{
    INTENT_PUBLIC_DTO_SCHEMA_VERSION, IntentApplicationState, IntentArtifactAvailability,
    IntentArtifactId, IntentArtifactKind, IntentArtifactOwnership, IntentAuthorityState,
    IntentConflictV1, IntentDefinitionState, IntentDigest, IntentDropRequestV1, IntentId,
    IntentOperationErrorCode, IntentOperationFileAction, IntentOperationFileSummaryV1,
    IntentOperationId, IntentOperationKind, IntentOperationPreviewV1, IntentStackId,
    IntentStackVersion, IntentVersionRef, PublicIntentArtifactSummaryV1, PublicIntentSourceV1,
    PublicIntentStackStateV1, PublicIntentStackV1, PublicIntentV1,
};

use super::*;
use crate::{
    mouse::{AppMouseOutcome, MouseInput, MouseInputKind},
    ui::LayoutSnapshot,
};

fn digest(fill: char) -> IntentDigest {
    IntentDigest::new(format!("sha256:jcs-v1:{}", fill.to_string().repeat(64)))
        .expect("valid test digest")
}

fn intent_ref() -> IntentVersionRef {
    IntentVersionRef::new(IntentId::new("intent_leaf").expect("intent id"), 1).expect("intent ref")
}

fn available_stack(
    authority_state: IntentAuthorityState,
    application_state: IntentApplicationState,
    include_conflict: bool,
) -> PublicIntentStackStateV1 {
    let intent_ref = intent_ref();
    let artifact_id = IntentArtifactId::new("artifact_leaf").expect("artifact id");
    let conflicts = include_conflict
        .then(|| IntentConflictV1 {
            code: IntentOperationErrorCode::SharedArtifact,
            intent_ref: Some(intent_ref.clone()),
            artifact_id: Some(artifact_id.clone()),
            safe_reason: "src/lib.rs is also owned by intent_root; split the hunk manually"
                .to_owned(),
        })
        .into_iter()
        .collect();
    PublicIntentStackStateV1::Available {
        schema_version: INTENT_PUBLIC_DTO_SCHEMA_VERSION,
        stack: PublicIntentStackV1 {
            schema_version: INTENT_PUBLIC_DTO_SCHEMA_VERSION,
            stack_id: IntentStackId::new("stack_test").expect("stack id"),
            stack_version: IntentStackVersion::new(7).expect("stack version"),
            authority_state,
            plan_digest: digest('a'),
            intents: vec![PublicIntentV1 {
                intent_ref,
                title: "Keep Intent Stack durable".to_owned(),
                statement: "Preserve the accepted intent across compaction and restart.".to_owned(),
                acceptance_criteria: Vec::new(),
                depends_on: Vec::new(),
                source: PublicIntentSourceV1::UserTurn {
                    source_turn_id: "turn_1".to_owned(),
                },
                definition_state: IntentDefinitionState::Accepted,
                application_state,
                exclusive_artifact_count: 1,
                shared_artifact_count: u32::from(include_conflict),
                unowned_artifact_count: 0,
                drifted_artifact_count: 0,
                unavailable_artifact_count: 0,
                advisory_criterion_count: 0,
                system_verified_criterion_count: 1,
                artifacts: vec![PublicIntentArtifactSummaryV1 {
                    artifact_id,
                    artifact_kind: IntentArtifactKind::FileHunk,
                    ownership: if include_conflict {
                        IntentArtifactOwnership::Shared
                    } else {
                        IntentArtifactOwnership::Exclusive
                    },
                    availability: IntentArtifactAvailability::Available,
                    normalized_relative_path: Some("src/lib.rs".to_owned()),
                }],
                available_actions: vec![IntentOperationKind::Drop],
            }],
            conflicts,
        },
    }
}

fn drop_preview(intent_ref: IntentVersionRef) -> IntentOperationPreviewV1 {
    IntentOperationPreviewV1 {
        schema_version: INTENT_PUBLIC_DTO_SCHEMA_VERSION,
        operation_id: IntentOperationId::new("operation_drop_leaf").expect("operation id"),
        operation_kind: IntentOperationKind::Drop,
        stack_id: IntentStackId::new("stack_test").expect("stack id"),
        stack_version: IntentStackVersion::new(7).expect("stack version"),
        target_intents: vec![intent_ref],
        target_is_leaf: true,
        workspace_revision: 12,
        expires_at_ms: Some(99_999),
        file_effects: vec![IntentOperationFileSummaryV1 {
            normalized_relative_path: "src/lib.rs".to_owned(),
            action: IntentOperationFileAction::Update,
            artifact_ids: vec![IntentArtifactId::new("artifact_leaf").expect("artifact id")],
        }],
        retained_intents: Vec::new(),
        verification_impacts: Vec::new(),
        conflicts: Vec::new(),
        preview_digest: digest('b'),
    }
}

fn open_and_load(app: &mut AppState, stack_state: PublicIntentStackStateV1) -> u64 {
    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT))
        .expect("open Intent Stack");
    let Some(AppAction::LoadIntentStack { request_id }) = action else {
        panic!("expected Intent Stack load");
    };
    assert!(app.apply_intent_stack_loaded(request_id, stack_state));
    request_id
}

#[test]
fn alt_s_opens_bounded_intent_review_with_retention_and_conflict_details() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    open_and_load(
        &mut app,
        available_stack(
            IntentAuthorityState::Active,
            IntentApplicationState::NeedsReview,
            true,
        ),
    );

    let view = app.intent_stack_modal_view().expect("Intent Stack view");
    assert_eq!(view.phase, super::super::IntentStackModalPhase::Ready);
    assert_eq!(view.rows.len(), 1);
    assert!(view.primary_action_enabled);
    assert!(
        view.detail_lines
            .iter()
            .any(|line| line == "Retained artifacts")
    );
    assert!(
        view.detail_lines
            .iter()
            .any(|line| line.contains("src/lib.rs · shared · retained"))
    );
    assert!(
        view.detail_lines
            .iter()
            .any(|line| line == "Blocked / manual resolution required")
    );
    assert!(
        view.detail_lines
            .iter()
            .any(|line| line.contains("split the hunk manually"))
    );
}

#[test]
fn exact_drop_confirmation_ignores_stale_preview_and_sends_only_digest_bound_request() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_and_load(
        &mut app,
        available_stack(
            IntentAuthorityState::Active,
            IntentApplicationState::Applied,
            false,
        ),
    );

    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
        .expect("preview key");
    let Some(AppAction::PreviewIntentDrop {
        request_id,
        intent_ref,
    }) = action
    else {
        panic!("expected exact Drop preview request");
    };
    let preview = drop_preview(intent_ref);
    assert!(!app.apply_intent_drop_preview(request_id + 1, preview.clone()));
    assert_eq!(
        app.intent_stack_modal_view().expect("view").phase,
        super::super::IntentStackModalPhase::PreviewingDrop
    );
    assert!(app.apply_intent_drop_preview(request_id, preview.clone()));

    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .expect("confirm exact preview");
    let Some(AppAction::ExecuteIntentDrop {
        request_id: execute_request_id,
        request:
            IntentDropRequestV1 {
                operation_id,
                stack_version,
                preview_digest,
            },
    }) = action
    else {
        panic!("expected digest-bound Drop request");
    };
    assert!(execute_request_id > request_id);
    assert_eq!(operation_id, preview.operation_id);
    assert_eq!(stack_version, preview.stack_version);
    assert_eq!(preview_digest, preview.preview_digest);
}

#[test]
fn read_only_provenance_disables_the_destructive_main_action() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_and_load(
        &mut app,
        available_stack(
            IntentAuthorityState::ReadOnlyProvenance,
            IntentApplicationState::ReadOnly,
            false,
        ),
    );

    let view = app.intent_stack_modal_view().expect("Intent Stack view");
    assert_eq!(view.phase, super::super::IntentStackModalPhase::ReadOnly);
    assert!(!view.primary_action_enabled);
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
            .expect("read-only key handling")
            .is_none()
    );
}

#[test]
fn session_switch_closes_intent_stack_and_late_load_response_is_ignored() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT))
        .expect("open Intent Stack");
    let Some(AppAction::LoadIntentStack { request_id }) = action else {
        panic!("expected load request");
    };

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: Path::new("session-switched.jsonl").to_path_buf(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries: Vec::new(),
    })
    .expect("switch session");
    assert!(!app.intent_stack_modal_open());

    app.handle_worker_message(WorkerMessage::IntentStackLoaded {
        request_id,
        stack_state: available_stack(
            IntentAuthorityState::Active,
            IntentApplicationState::Applied,
            false,
        ),
    })
    .expect("late response");
    assert!(!app.intent_stack_modal_open());
    assert!(
        app.events
            .iter()
            .any(|event| event.detail == format!("ignored stale load response {request_id}"))
    );
}

#[test]
fn mouse_primary_action_uses_the_same_exact_preview_path() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    open_and_load(
        &mut app,
        available_stack(
            IntentAuthorityState::Active,
            IntentApplicationState::Applied,
            false,
        ),
    );
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 100, 28), &app);
    let action_area = layout
        .intent_stack_modal_hit_areas
        .as_ref()
        .expect("Intent Stack hit areas")
        .primary_action;
    let outcome = app
        .handle_mouse_event(
            MouseInput {
                column: action_area.x,
                row: action_area.y,
                kind: MouseInputKind::LeftDown,
                modifiers: KeyModifiers::NONE,
            },
            &layout,
        )
        .expect("mouse preview action");

    assert!(matches!(
        outcome,
        AppMouseOutcome::Action(AppAction::PreviewIntentDrop { .. })
    ));
}
