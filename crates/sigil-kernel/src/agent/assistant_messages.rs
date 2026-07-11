use anyhow::Result;

use crate::{
    TransientMessageOverlay,
    event::{EventHandler, RunEvent},
    provider::{AssistantMessageKind, ModelMessage, ProviderContinuationState, ToolCall},
    session::{ControlEntry, Session},
    tool::{ToolCategory, ToolRegistry},
};

pub(super) fn append_tool_preamble_message<H>(
    session: &mut Session,
    handler: &mut H,
    tools: &ToolRegistry,
    assistant_text: &str,
    completed_calls: &[ToolCall],
    pending_states: Vec<ProviderContinuationState>,
) -> Result<TransientMessageOverlay>
where
    H: EventHandler,
{
    let completed_agent_tool_calls = count_agent_tool_calls(tools, completed_calls);
    let assistant_content = if completed_agent_tool_calls > 0 {
        None
    } else {
        (!assistant_text.trim().is_empty()).then(|| assistant_text.to_owned())
    };
    let exact_assistant_message = ModelMessage::assistant_with_kind(
        assistant_content,
        completed_calls.to_vec(),
        AssistantMessageKind::ToolPreamble,
    );
    let (assistant_message, exact_overlay) =
        crate::project_message_for_persistence(exact_assistant_message)?;
    let assistant_message_id = assistant_message.id.clone();
    session.append_assistant_message(assistant_message.clone())?;
    handler.handle(RunEvent::AssistantMessage(assistant_message))?;
    save_continuation_states(session, handler, pending_states, &assistant_message_id)?;
    Ok(exact_overlay)
}

pub(super) fn append_final_answer_message<H>(
    session: &mut Session,
    handler: &mut H,
    assistant_text: &str,
    pending_states: Vec<ProviderContinuationState>,
) -> Result<String>
where
    H: EventHandler,
{
    let exact_assistant_message = ModelMessage::assistant_with_kind(
        Some(assistant_text.to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    let (assistant_message, _) = crate::project_message_for_persistence(exact_assistant_message)?;
    let final_message_id = assistant_message.id.clone();
    session.append_assistant_message(assistant_message.clone())?;
    handler.handle(RunEvent::AssistantMessage(assistant_message))?;
    save_continuation_states(session, handler, pending_states, &final_message_id)?;
    Ok(final_message_id)
}

fn save_continuation_states<H>(
    session: &mut Session,
    handler: &mut H,
    mut pending_states: Vec<ProviderContinuationState>,
    message_id: &str,
) -> Result<()>
where
    H: EventHandler,
{
    for state in &mut pending_states {
        if state.message_id.is_none() {
            state.message_id = Some(message_id.to_owned());
        }
    }
    for state in pending_states {
        let control = ControlEntry::ContinuationStateSaved(state);
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    Ok(())
}

fn count_agent_tool_calls(tools: &ToolRegistry, calls: &[ToolCall]) -> usize {
    calls
        .iter()
        .filter(|call| {
            tools
                .spec_for(&call.name)
                .is_some_and(|spec| spec.category == ToolCategory::Agent)
        })
        .count()
}
