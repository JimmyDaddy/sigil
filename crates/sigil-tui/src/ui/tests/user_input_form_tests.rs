use ratatui::{Terminal, backend::TestBackend};

use super::*;
use crate::app::{
    PendingUserInputForm, UserInputDraftValue, UserInputFormAction, UserInputFormSource,
    UserInputFormViewModel,
};

fn revision_form() -> PendingUserInputForm {
    let request = sigil_kernel::PublicUserInputRequestV1 {
        identity: sigil_kernel::UserInputIdentityV1 {
            session_scope_id: sigil_kernel::SessionScopeId::new("revision-form-session")
                .expect("valid session scope"),
            root_logical_run_id: sigil_kernel::LogicalRunId::new("revision-form-root")
                .expect("valid root run"),
            source_thread_id: sigil_kernel::AgentThreadId::new("main")
                .expect("valid source thread"),
            request_id: sigil_kernel::UserInputRequestId::new("revision-form-request")
                .expect("valid request id"),
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        request_hash: format!("sha256:{}", "b".repeat(64)),
        source: sigil_kernel::UserInputSourceV1::PlanRevision {
            base_plan_id: sigil_kernel::PlanId::new("plan-revision-form").expect("valid plan id"),
            base_plan_hash: format!("sha256:{}", "c".repeat(64)),
        },
        purpose: sigil_kernel::UserInputPurposeV1::RevisionGuidance,
        prompt: "What should change in this plan?".to_owned(),
        questions: vec![sigil_kernel::UserInputQuestionV1 {
            id: "revision_guidance".to_owned(),
            header: "Revision guidance".to_owned(),
            question: "Describe the changes you want before a new plan is prepared.".to_owned(),
            description: Some(
                "The original plan remains available until a revised draft succeeds.".to_owned(),
            ),
            required: true,
            field: sigil_kernel::UserInputFieldKindV1::Text {
                multiline: true,
                max_chars: 2_000,
            },
        }],
        allowed_actions: vec![
            sigil_kernel::UserInputActionV1::Submit,
            sigil_kernel::UserInputActionV1::Decline,
        ],
        requested_at_unix_ms: 1,
        status: sigil_kernel::UserInputStatusV1::Requested,
        answer_receipt: None,
        resolution: None,
    };
    PendingUserInputForm {
        view: UserInputFormViewModel::from(&request),
        request: Some(request),
        source: UserInputFormSource::DurableAgent,
        recovery_command: None,
        queue_position: 1,
        queue_length: 1,
        open: true,
        focused_question: 0,
        focus_actions: false,
        selected_action: UserInputFormAction::Submit,
        drafts: vec![UserInputDraftValue::Text(String::new())],
        scroll: 0,
        scroll_extent: Default::default(),
    }
}

fn rendered_content(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn plan_revision_form_makes_the_workflow_and_outcomes_explicit() -> anyhow::Result<()> {
    let form = revision_form();
    let mut terminal = Terminal::new(TestBackend::new(120, 30))?;

    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("PLAN REVISION"));
    assert!(rendered.contains("CURRENT PLAN"));
    assert!(rendered.contains("YOUR REVISION REQUEST"));
    assert!(rendered.contains("Describe the change you want to make"));
    assert!(rendered.contains("Prepare revised plan"));
    assert!(rendered.contains("Keep current plan"));
    assert!(!rendered.contains("Input required"));
    Ok(())
}

#[test]
fn regular_user_input_keeps_the_generic_form_presentation() -> anyhow::Result<()> {
    let mut form = revision_form();
    form.request.as_mut().expect("request").source = sigil_kernel::UserInputSourceV1::Agent;
    let mut terminal = Terminal::new(TestBackend::new(120, 30))?;

    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Input required"));
    assert!(rendered.contains("Submit"));
    assert!(rendered.contains("Decline"));
    assert!(!rendered.contains("PLAN REVISION"));
    Ok(())
}

#[test]
fn plan_revision_stays_identifiable_on_a_short_terminal() -> anyhow::Result<()> {
    let form = revision_form();
    let mut terminal = Terminal::new(TestBackend::new(64, 13))?;

    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Plan revision · current plan stays active"));
    assert!(rendered.contains("Prepare revised plan"));
    assert!(rendered.contains("Keep current plan"));
    assert!(!rendered.contains("Input required"));
    Ok(())
}
