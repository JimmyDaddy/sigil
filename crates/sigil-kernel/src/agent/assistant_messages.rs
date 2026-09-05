use anyhow::{Result, anyhow};

use crate::{
    TransientMessageOverlay,
    event::EventHandler,
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
    append_assistant_message_bundle(
        session,
        handler,
        assistant_message,
        pending_states,
        Vec::new(),
    )?;
    Ok(exact_overlay)
}

pub(super) fn append_final_answer_message<H>(
    session: &mut Session,
    handler: &mut H,
    assistant_text: &str,
    pending_states: Vec<ProviderContinuationState>,
    mut url_capability_registrations: Vec<crate::UserUrlCapabilityRegistration>,
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
    for registration in &mut url_capability_registrations {
        registration.durable_entry_id.clone_from(&final_message_id);
    }
    let registrar = session.user_url_capability_registrar();
    if !url_capability_registrations.is_empty() {
        let registrar = registrar.as_ref().ok_or_else(|| {
            anyhow!("hosted final answer produced URL capabilities without a session registrar")
        })?;
        for registration in &url_capability_registrations {
            if let Err(error) = registrar.stage(registration.clone()) {
                let _ = registrar.rollback_message(&final_message_id);
                return Err(error);
            }
        }
    }
    let mut controls = Vec::with_capacity(url_capability_registrations.len());
    for registration in &url_capability_registrations {
        let descriptor = registration.durable_descriptor(session.session_scope_id());
        descriptor.validate()?;
        controls.push(ControlEntry::WebUrlCapabilityDescriptor(descriptor));
    }
    if let Err(error) = append_assistant_message_bundle(
        session,
        handler,
        assistant_message,
        pending_states,
        controls,
    ) {
        if !url_capability_registrations.is_empty()
            && let Some(registrar) = registrar.as_ref()
        {
            let _ = registrar.rollback_message(&final_message_id);
        }
        return Err(error);
    }
    if !url_capability_registrations.is_empty()
        && let Some(registrar) = registrar.as_ref()
        && let Err(error) = registrar.commit_message(&final_message_id)
    {
        let rollback_error = registrar.rollback_message(&final_message_id).err();
        return Err(error.context(match rollback_error {
            Some(rollback_error) => format!(
                "failed to commit hosted URL capabilities; rollback also failed: {rollback_error:#}"
            ),
            None => "failed to commit hosted URL capabilities".to_owned(),
        }));
    }
    Ok(final_message_id)
}

pub(super) fn append_assistant_message_bundle<H>(
    session: &mut Session,
    handler: &mut H,
    assistant_message: ModelMessage,
    mut pending_states: Vec<ProviderContinuationState>,
    controls: Vec<ControlEntry>,
) -> Result<String>
where
    H: EventHandler,
{
    let message_id = assistant_message.id.clone();
    for state in &mut pending_states {
        if state.message_id.is_none() {
            state.message_id = Some(message_id.clone());
        }
    }
    let mut entries = Vec::with_capacity(1 + controls.len() + pending_states.len());
    let mut publications = Vec::with_capacity(1);
    entries.push(crate::SessionLogEntry::Assistant(assistant_message.clone()));
    publications.push(
        crate::session::SessionPublicEventProjectionV1::assistant_message(0, assistant_message),
    );
    entries.extend(controls.into_iter().map(crate::SessionLogEntry::Control));
    for state in pending_states {
        entries.push(crate::SessionLogEntry::Control(
            ControlEntry::ContinuationStateSaved(state),
        ));
    }
    handler.commit_session_publications(session, entries, publications)?;
    Ok(message_id)
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
