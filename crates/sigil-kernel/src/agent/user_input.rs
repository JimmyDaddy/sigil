use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    AgentThreadId, ControlEntry, EventHandler, LogicalRunId, Session, SessionLogEntry, ToolCall,
    ToolErrorKind, ToolExecutionStatus, ToolResult, USER_INPUT_SCHEMA_VERSION, UserInputActionV1,
    UserInputContinuationBindingV1, UserInputIdentityV1, UserInputOptionV1,
    UserInputPromptOriginV1, UserInputPurposeV1, UserInputQuestionV1, UserInputRequestId,
    UserInputRequestRefV1, UserInputRequestV1, UserInputRequestedV1, UserInputSourceV1,
};

use super::{
    AgentRunOutcome,
    tool_audit::{
        append_tool_execution_audit, attach_tool_call_context, durable_tool_execution_entry,
    },
    tool_results::record_tool_result_to_batch,
};

#[derive(Debug, Deserialize)]
struct RequestUserInputArgsV1 {
    questions: Vec<RequestUserInputQuestionArgsV1>,
}

#[derive(Debug, Deserialize)]
struct RequestUserInputQuestionArgsV1 {
    id: String,
    question: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    options: Option<Vec<RequestUserInputOptionArgsV1>>,
    #[serde(default)]
    multiple: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RequestUserInputOptionArgsV1 {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug)]
pub(super) struct NormalizedRequestUserInputArgs {
    pub(super) prompt: String,
    pub(super) prompt_origin: UserInputPromptOriginV1,
    pub(super) questions: Vec<UserInputQuestionV1>,
}

impl RequestUserInputArgsV1 {
    fn normalize(self) -> Result<NormalizedRequestUserInputArgs> {
        if self.questions.is_empty() || self.questions.len() > crate::MAX_USER_INPUT_QUESTIONS {
            bail!("request_user_input requires one to three questions");
        }

        let prompt_origin = if self.questions.len() == 1 {
            UserInputPromptOriginV1::QuestionText
        } else {
            UserInputPromptOriginV1::HostQuestionGroup
        };
        let prompt = if self.questions.len() == 1 {
            self.questions[0].question.clone()
        } else {
            "Please answer the following questions before continuing.".to_owned()
        };

        let questions = self
            .questions
            .into_iter()
            .map(|question| {
                let multiple = question.multiple.unwrap_or(false);
                let options = match question.options {
                    None if multiple => {
                        bail!("request_user_input question `{}` sets multiple without options", question.id)
                    }
                    None => Vec::new(),
                    Some(options) => {
                        if options.len() < 2 || options.len() > crate::MAX_USER_INPUT_OPTIONS {
                            bail!("request_user_input question `{}` must contain two to twelve options", question.id)
                        }
                        options
                            .into_iter()
                            .enumerate()
                            .map(|(option_index, option)| UserInputOptionV1 {
                                id: format!("option-{}", option_index + 1),
                                label: option.label,
                                description: option.description,
                            })
                            .collect::<Vec<_>>()
                    }
                };
                Ok(UserInputQuestionV1 {
                    id: question.id,
                    question: question.question,
                    description: question.description,
                    required: true,
                    options,
                    multiple,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(NormalizedRequestUserInputArgs {
            prompt,
            prompt_origin,
            questions,
        })
    }
}

pub(super) fn parse_request_user_input_args(raw: &str) -> Result<NormalizedRequestUserInputArgs> {
    let args = serde_json::from_str::<RequestUserInputArgsV1>(raw).map_err(|error| {
        let category = if error.is_eof() {
            "truncated JSON"
        } else if error.is_syntax() {
            "invalid JSON"
        } else {
            "invalid argument shape"
        };
        anyhow!(
            "request_user_input {category} at line {} column {}: {error}; expected {{\"questions\":[{{\"id\":\"...\",\"question\":\"...\"}}]}}",
            error.line(),
            error.column()
        )
    })?;
    args.normalize()
}

pub(super) fn request_user_input_call_is_accepted(call: &ToolCall) -> bool {
    call.name == crate::REQUEST_USER_INPUT_TOOL_NAME
        && parse_request_user_input_args(&call.args_json).is_ok()
}

#[derive(Debug, thiserror::Error)]
#[error("failed to publish the durable user-input request")]
pub(super) struct RequestUserInputPublicationFailure;

pub(super) struct RequestUserInputContext<'a> {
    pub logical_run_id: &'a str,
    pub root_logical_run_id: &'a str,
    pub source_thread_id: &'a AgentThreadId,
    pub provider_name: &'a str,
    pub model_name: &'a str,
    pub source: UserInputSourceV1,
}

pub(super) fn handle_request_user_input_call<H>(
    session: &mut Session,
    handler: &mut H,
    call: &ToolCall,
    context: RequestUserInputContext<'_>,
) -> Result<UserInputRequestRefV1>
where
    H: EventHandler + Send,
{
    let args = parse_request_user_input_args(&call.args_json)?;
    reject_credential_collection(&args)?;

    let assistant_message_id = session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Assistant(message)
                if message
                    .tool_calls
                    .iter()
                    .any(|candidate| candidate.id == call.id) =>
            {
                Some(message.id.clone())
            }
            _ => None,
        })
        .context("request_user_input call is missing its durable assistant message")?;
    let root_logical_run_id = LogicalRunId::new(context.root_logical_run_id)?;
    let request_id = request_id_for_call(
        session.session_scope_id(),
        &root_logical_run_id,
        context.source_thread_id,
        &call.id,
    )?;
    let source_binding_hash = source_binding_hash(
        session.session_scope_id(),
        &root_logical_run_id,
        context.source_thread_id,
        &assistant_message_id,
        call,
    )?;
    let identity = UserInputIdentityV1 {
        session_scope_id: crate::SessionScopeId::new(session.session_scope_id())?,
        root_logical_run_id,
        source_thread_id: context.source_thread_id.clone(),
        request_id,
        generation: 1,
        source_binding_hash,
    };
    let projection = session.user_input_projection()?;
    let requested_at_unix_ms = projection
        .request(&identity)
        .map_or_else(super::unix_time_ms, |existing| {
            existing.requested.request.requested_at_unix_ms
        });
    // Replays retain the frozen presentation origin, including its historical absence.
    let prompt_origin = projection
        .request(&identity)
        .map_or(Some(args.prompt_origin), |existing| {
            existing.requested.request.prompt_origin
        });
    let requested = UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity,
        source: context.source.clone(),
        purpose: UserInputPurposeV1::Clarification,
        prompt: args.prompt,
        prompt_origin,
        questions: args.questions,
        allowed_actions: vec![
            UserInputActionV1::Submit,
            UserInputActionV1::Decline,
            UserInputActionV1::CancelRun,
        ],
        requested_at_unix_ms,
        continuation: Some(UserInputContinuationBindingV1 {
            assistant_message_id,
            tool_call_id: call.id.clone(),
            provider_name: context.provider_name.to_owned(),
            model_name: context.model_name.to_owned(),
        }),
    })?;

    if let Some(existing) = projection.request(&requested.request.identity) {
        if existing.requested == requested {
            return Ok((&existing.requested).into());
        }
        bail!("request_user_input replay conflicts with its durable request");
    }

    let started = ControlEntry::ToolExecution(Box::new(durable_tool_execution_entry(
        call,
        &[],
        ToolExecutionStatus::Started,
        None,
        None,
    )?));
    let request_control = ControlEntry::UserInputRequested(Box::new(requested.clone()));
    let mut controls =
        consumed_continuation_before_request(session, &projection, &requested, &context)?;
    controls.extend([started, request_control]);
    // Validate the whole transition before publication, including root budget and one pending
    // request. A malformed follow-up must never consume the previous answer by itself.
    let mut staged = projection;
    for control in &controls {
        if let Some(entry) = crate::UserInputLifecycleEntryV1::from_control(control) {
            staged.apply(entry)?;
        }
    }
    handler
        .commit_controls(session, controls)
        .context(RequestUserInputPublicationFailure)?;
    Ok((&requested).into())
}

/// Settles only an answer whose exact continuation has already consumed a provider turn.
/// The returned control is committed atomically with the new request, never on its own.
fn consumed_continuation_before_request(
    session: &Session,
    projection: &crate::UserInputProjectionV1,
    requested: &UserInputRequestedV1,
    context: &RequestUserInputContext<'_>,
) -> Result<Vec<ControlEntry>> {
    let identity = &requested.request.identity;
    let Some(state) = projection.pending().find(|state| {
        state.requested.request.identity.source_thread_id == identity.source_thread_id
    }) else {
        return Ok(Vec::new());
    };
    let previous = &state.requested.request;
    if state.status != crate::UserInputStatusV1::ContinuationStarted
        || state
            .decision
            .as_ref()
            .is_none_or(|decision| !decision.decision.is_submitted())
        || previous.identity.session_scope_id != identity.session_scope_id
        || previous.identity.root_logical_run_id != identity.root_logical_run_id
        || previous.source != requested.request.source
    {
        // The ordinary reducer rejects overlapping requests. Do not relax pending admission.
        return Ok(Vec::new());
    }
    let started = state
        .continuation
        .as_ref()
        .context("started user-input continuation lost its exact attempt")?;
    if started.continuation_logical_run_id.as_str() != context.logical_run_id {
        bail!("new user-input request does not belong to the live continuation");
    }
    let attempts = session.provider_physical_attempt_projection()?;
    let attempt = attempts
        .attempt(&started.physical_attempt_id)
        .context("new user-input request lacks its continuation provider evidence")?;
    if attempt.session_id() != session.session_scope_id()
        || attempt.entry.logical_run_id != context.logical_run_id
        || attempt.terminal.as_ref().is_none_or(|terminal| {
            terminal.outcome != crate::ProviderPhysicalAttemptOutcome::Completed
        })
    {
        bail!("new user-input request requires its exact completed continuation attempt");
    }
    Ok(vec![ControlEntry::UserInputResolved(
        crate::UserInputResolvedV1 {
            schema_version: USER_INPUT_SCHEMA_VERSION,
            identity: previous.identity.clone(),
            request_hash: state.requested.request_hash.clone(),
            resolution: crate::UserInputResolutionV1::Consumed,
            resolved_at_unix_ms: super::unix_time_ms(),
        },
    )])
}

pub(super) fn append_request_user_input_error(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    error: &anyhow::Error,
    assistant_batch_results: &mut Vec<(ToolCall, ToolResult)>,
) -> Result<()> {
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::InvalidInput,
        error.to_string(),
    );
    attach_tool_call_context(&mut result, call, &[]);
    append_tool_execution_audit(
        session,
        call,
        &[],
        ToolExecutionStatus::Failed,
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(())
}

pub(super) fn append_tool_ignored_after_user_input_request(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "request_user_input suspended this run; additional tool calls in the same assistant batch were ignored",
    );
    attach_tool_call_context(&mut result, call, &[]);
    append_tool_execution_audit(
        session,
        call,
        &[],
        ToolExecutionStatus::Cancelled,
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(())
}

fn request_id_for_call(
    session_scope_id: &str,
    root_logical_run_id: &LogicalRunId,
    source_thread_id: &AgentThreadId,
    call_id: &str,
) -> Result<UserInputRequestId> {
    let material = format!(
        "sigil-user-input-request-v1\0{session_scope_id}\0{}\0{}\0{call_id}",
        root_logical_run_id.as_str(),
        source_thread_id.as_str(),
    );
    let digest = Sha256::digest(material.as_bytes());
    UserInputRequestId::new(format!("input-{digest:x}"))
}

fn source_binding_hash(
    session_scope_id: &str,
    root_logical_run_id: &LogicalRunId,
    source_thread_id: &AgentThreadId,
    assistant_message_id: &str,
    call: &ToolCall,
) -> Result<String> {
    let material = serde_json::to_vec(&serde_json::json!({
        "contract": "sigil-user-input-source-v1",
        "session_scope_id": session_scope_id,
        "root_logical_run_id": root_logical_run_id.as_str(),
        "source_thread_id": source_thread_id.as_str(),
        "assistant_message_id": assistant_message_id,
        "tool_call_id": call.id,
        "tool_name": call.name,
        "args_json": call.args_json,
    }))?;
    Ok(format!("sha256:{:x}", Sha256::digest(material)))
}

fn reject_credential_collection(args: &NormalizedRequestUserInputArgs) -> Result<()> {
    let material = std::iter::once(args.prompt.as_str())
        .chain(args.questions.iter().flat_map(|question| {
            std::iter::once(question.question.as_str()).chain(question.description.as_deref())
        }))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let requests_value = [
        "enter", "provide", "paste", "type", "send", "输入", "提供", "粘贴", "发送",
    ]
    .iter()
    .any(|verb| material.contains(verb));
    let names_credential = [
        "password",
        "api key",
        "api_key",
        "access token",
        "private key",
        "密码",
        "密钥",
        "令牌",
    ]
    .iter()
    .any(|credential| material.contains(credential));
    if requests_value && names_credential {
        bail!("request_user_input cannot collect credentials or secret values");
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/agent_user_input_prompt_origin_tests.rs"]
mod prompt_origin_tests;

#[cfg(test)]
#[path = "../tests/agent_user_input_continuation_tests.rs"]
mod continuation_tests;
