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
        prompt_origin: None,
        questions: vec![sigil_kernel::UserInputQuestionV1 {
            id: "revision_guidance".to_owned(),
            question: "Describe the changes you want before a new plan is prepared.".to_owned(),
            description: Some(
                "The original plan remains available until a revised draft succeeds.".to_owned(),
            ),
            required: true,
            options: Vec::new(),
            multiple: false,
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
        validation_error: None,
        plan_revision_editor: Default::default(),
        scroll: 0,
        scroll_extent: Default::default(),
        question_scroll_offsets: Default::default(),
    }
}

fn rendered_content(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for row in buffer.content().chunks(buffer.area.width as usize) {
        let mut column = 0;
        while let Some(cell) = row.get(column) {
            text.push_str(cell.symbol());
            column += crate::ui::terminal_cell_width(cell.symbol()).max(1);
        }
    }
    text
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
    assert!(rendered.contains("Your current plan stays available"));
    assert!(rendered.contains("REVISION REQUEST"));
    assert!(rendered.contains("Describe the changes you want"));
    assert!(rendered.contains("Enter  Submit revision"));
    assert!(rendered.contains("Esc  Back"));
    assert!(rendered.contains("Shift+Enter / Ctrl+J  New line"));
    assert!(!rendered.contains("Keep current plan"));
    assert!(!rendered.contains("Cancel plan run"));
    assert!(!rendered.contains("actions"));
    assert!(!rendered.contains("Input required"));
    terminal.backend_mut().assert_cursor_position((7, 9));
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
    assert!(rendered.contains("PLAN REVISION"));
    assert!(rendered.contains("Submit revision"));
    assert!(rendered.contains("Esc  Back"));
    assert!(!rendered.contains("Input required"));
    terminal.backend_mut().assert_cursor_position((2, 4));
    Ok(())
}

#[test]
fn plan_revision_submission_failure_keeps_the_draft_and_error_in_the_editor() -> anyhow::Result<()>
{
    let mut form = revision_form();
    form.drafts[0] = UserInputDraftValue::Text("Keep this exact request".to_owned());
    form.plan_revision_editor.cursor = "Keep this exact request".len();
    form.plan_revision_editor.error = Some("The request could not be submitted; retry".to_owned());
    let mut terminal = Terminal::new(TestBackend::new(64, 13))?;

    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Keep this exact request"));
    assert!(rendered.contains("The request could not be submitted; retry"));
    assert!(rendered.contains("Submit revision"));
    assert!(rendered.contains("Shift+Enter / Ctrl+J"));
    Ok(())
}

#[test]
fn plan_revision_submitting_replaces_the_submit_affordance() -> anyhow::Result<()> {
    let mut form = revision_form();
    form.drafts[0] = UserInputDraftValue::Text("Keep this exact request".to_owned());
    form.plan_revision_editor.submitting = true;
    let mut terminal = Terminal::new(TestBackend::new(64, 13))?;

    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Submitting revision"));
    assert!(rendered.contains("Keep this exact request"));
    assert!(rendered.contains("Esc  Back"));
    assert!(!rendered.contains("Submit revision"));
    Ok(())
}

#[test]
fn recovered_input_explains_saved_answer_without_claiming_owner_stopped() -> anyhow::Result<()> {
    let mut form = revision_form();
    let request = form.request.as_mut().expect("request");
    request.source = sigil_kernel::UserInputSourceV1::Agent;
    form.recovery_command = Some(sigil_kernel::UserInputDecisionCommandV1 {
        identity: request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("render-recovery")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "revision_guidance".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "private stored answer".to_owned(),
                },
            }],
        },
    });
    form.selected_action = UserInputFormAction::Resume;
    let mut terminal = Terminal::new(TestBackend::new(120, 30))?;
    terminal.draw(|frame| render_user_input_form(frame, frame.area(), &form, &Theme::default()))?;
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Your answer is saved"));
    assert!(rendered.contains("checks that the request can continue"));
    assert!(rendered.contains("Resume"));
    assert!(!rendered.contains("previous owner stopped"));
    assert!(!rendered.contains("private stored answer"));
    Ok(())
}

fn ordinary_form() -> PendingUserInputForm {
    let mut form = revision_form();
    form.request.as_mut().expect("request").source = sigil_kernel::UserInputSourceV1::Agent;
    form.view.questions[0].id = "internal_question_id".to_owned();
    form.view.questions[0].question = "你希望保留哪些内容？".to_owned();
    form
}

#[test]
fn ordinary_form_localizes_only_a_typed_host_question_group() -> anyhow::Result<()> {
    let mut form = ordinary_form();
    form.view.prompt = "Please answer the following questions before continuing.".to_owned();
    form.view.prompt_origin = Some(sigil_kernel::UserInputPromptOriginV1::HostQuestionGroup);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| render_user_input_form(frame, frame.area(), &form, &Theme::default()))?;
    assert!(rendered_content(&terminal).contains("请回答以下问题后继续"));
    assert!(!rendered_content(&terminal).contains("Please answer the following questions"));
    form.view.prompt_origin = None;
    terminal.draw(|frame| render_user_input_form(frame, frame.area(), &form, &Theme::default()))?;
    assert!(rendered_content(&terminal).contains(&form.view.prompt));
    Ok(())
}

#[test]
fn ordinary_form_hints_follow_field_and_action_focus_without_raw_ids() -> anyhow::Result<()> {
    let mut form = ordinary_form();
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("你希望保留哪些内容？"));
    assert!(!rendered.contains("internal_question_id"));
    assert!(rendered.contains("Enter Actions"));
    assert!(rendered.contains("Shift-Tab Previous"));
    assert!(rendered.contains("Esc Close"));
    assert!(!rendered.contains("Enter newline"));

    form.focus_actions = true;
    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("↑ Answers"));
    assert!(rendered.contains("←→ / Tab Actions"));
    assert!(rendered.contains("Enter Confirm"));
    assert!(rendered.contains("Esc Close"));
    assert!(!rendered.contains("Shift-Tab Previous"));
    Ok(())
}

#[test]
fn ordinary_form_keeps_validation_and_actions_visible_on_short_narrow_windows() -> anyhow::Result<()>
{
    for (width, height) in [(80, 24), (48, 12)] {
        let mut form = ordinary_form();
        form.validation_error = Some(crate::app::UserInputFormValidationError {
            question_index: Some(0),
            message: "Please answer question 1 before submitting".to_owned(),
        });
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| {
            render_user_input_form(frame, frame.area(), &form, &Theme::default());
        })?;
        let rendered = rendered_content(&terminal);
        assert!(rendered.contains("Please answer question 1 before submitting"));
        assert!(rendered.contains("Submit"));
        assert!(rendered.contains("Decline"));
        assert!(rendered.contains("Esc Close"));
        assert!(rendered.contains("Enter Actions"));
    }
    Ok(())
}

#[test]
fn ordinary_form_action_hints_wrap_and_returned_question_uses_rendered_rows() -> anyhow::Result<()>
{
    let mut form = ordinary_form();
    let mut second = form.view.questions[0].clone();
    form.view.questions[0].description = Some("Long question detail. ".repeat(36));
    second.id = "second".to_owned();
    second.question = "Second question is the focused return target".to_owned();
    form.view.questions.push(second);
    form.drafts
        .push(UserInputDraftValue::Text("Keep this answer".to_owned()));
    form.focused_question = 1;
    form.focus_actions = true;
    let mut terminal = Terminal::new(TestBackend::new(48, 12))?;
    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("↑ Answers"));
    assert!(rendered.contains("Enter Confirm"));
    assert!(rendered.contains("Esc Close"));
    assert!(form.question_scroll_offsets.borrow()[1] > 10);

    form.focus_actions = false;
    form.scroll = form.question_scroll_offsets.borrow()[1];
    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Second question is the focused return target"));
    assert!(rendered.contains("Keep this answer"));
    Ok(())
}

#[test]
fn ordinary_form_choice_hint_describes_other_input_and_next_question() -> anyhow::Result<()> {
    let mut form = ordinary_form();
    form.view.questions[0].options = vec![sigil_kernel::UserInputOptionV1 {
        id: "option".to_owned(),
        label: "Suggested option".to_owned(),
        description: None,
    }];
    form.drafts[0] = UserInputDraftValue::SingleSelect {
        selected: None,
        selected_by_user: false,
        other: String::new(),
    };
    form.view.questions.push(form.view.questions[0].clone());
    form.drafts.push(UserInputDraftValue::Text(String::new()));
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| {
        render_user_input_form(frame, frame.area(), &form, &Theme::default());
    })?;
    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Enter Next question"));
    assert!(rendered.contains("Space Select"));
    assert!(rendered.contains("Type / paste Other"));
    Ok(())
}
