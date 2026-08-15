use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    AgentThreadId, ControlEntry, PlanId, Session, SessionLogEntry, TaskId, ToolAccess,
    ToolArtifactSensitivity, ToolCategory, ToolExecutionStatus, ToolPreviewCapability, ToolResult,
    ToolResultMeta, ToolResultRecordedV3, ToolSpec,
};

pub const USER_INPUT_SCHEMA_VERSION: u16 = 1;
pub const REQUEST_USER_INPUT_TOOL_NAME: &str = "request_user_input";
pub const MAX_USER_INPUT_QUESTIONS: usize = 3;
pub const MAX_USER_INPUT_REQUESTS_PER_ROOT_RUN: usize = 3;
pub const MAX_USER_INPUT_OPTIONS: usize = 12;
pub const MAX_USER_INPUT_TEXT_CHARS: u32 = 4096;
pub const MAX_USER_INPUT_PROMPT_CHARS: usize = 512;
pub const MAX_USER_INPUT_DESCRIPTION_CHARS: usize = 512;
pub const MAX_USER_INPUT_REQUEST_BYTES: usize = 24 * 1024;
pub const MAX_USER_INPUT_ANSWER_BYTES: usize = 16 * 1024;

/// Model-visible schema for the host-owned user-input suspension tool.
///
/// Identity, lifecycle generation, source binding, allowed actions, and continuation ownership
/// are deliberately absent from the arguments. The host derives and persists those fields from
/// the exact assistant tool call before returning control to a product surface.
#[must_use]
pub fn request_user_input_tool_spec() -> ToolSpec {
    let options_schema = json!({
        "type": "array",
        "minItems": 2,
        "maxItems": MAX_USER_INPUT_OPTIONS,
        "items": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "minLength": 1, "maxLength": 48},
                "label": {"type": "string", "minLength": 1, "maxLength": 80},
                "description": {"type": "string", "maxLength": 240}
            },
            "required": ["id", "label"],
            "additionalProperties": false
        }
    });
    ToolSpec {
        name: REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        description: "Pause this run and ask the user for one to three missing decisions that are required before work can continue. Use only when the answer materially changes the implementation or safety boundary and cannot be discovered from the workspace. Keep questions short, mutually independent, and typed; do not ask for passwords, API keys, tokens, private keys, or other credentials. The host persists the request and resumes the same logical run after the user answers."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_USER_INPUT_PROMPT_CHARS},
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_USER_INPUT_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "minLength": 1, "maxLength": 48},
                            "header": {"type": "string", "minLength": 1, "maxLength": 32},
                            "question": {"type": "string", "minLength": 1, "maxLength": 512},
                            "description": {"type": "string", "maxLength": MAX_USER_INPUT_DESCRIPTION_CHARS},
                            "required": {"type": "boolean"},
                            "field": {
                                "oneOf": [
                                    {
                                        "type": "object",
                                        "properties": {
                                            "kind": {"const": "text"},
                                            "multiline": {"type": "boolean"},
                                            "max_chars": {"type": "integer", "minimum": 1, "maximum": MAX_USER_INPUT_TEXT_CHARS}
                                        },
                                        "required": ["kind", "multiline", "max_chars"],
                                        "additionalProperties": false
                                    },
                                    {
                                        "type": "object",
                                        "properties": {"kind": {"const": "number"}},
                                        "required": ["kind"],
                                        "additionalProperties": false
                                    },
                                    {
                                        "type": "object",
                                        "properties": {"kind": {"const": "integer"}},
                                        "required": ["kind"],
                                        "additionalProperties": false
                                    },
                                    {
                                        "type": "object",
                                        "properties": {"kind": {"const": "boolean"}},
                                        "required": ["kind"],
                                        "additionalProperties": false
                                    },
                                    {
                                        "type": "object",
                                        "properties": {
                                            "kind": {"const": "single_select"},
                                            "options": options_schema.clone(),
                                            "allow_other": {"type": "boolean"}
                                        },
                                        "required": ["kind", "options", "allow_other"],
                                        "additionalProperties": false
                                    },
                                    {
                                        "type": "object",
                                        "properties": {
                                            "kind": {"const": "multi_select"},
                                            "options": options_schema,
                                            "max_selected": {"type": "integer", "minimum": 1, "maximum": MAX_USER_INPUT_OPTIONS}
                                        },
                                        "required": ["kind", "options", "max_selected"],
                                        "additionalProperties": false
                                    }
                                ]
                            }
                        },
                        "required": ["id", "header", "question", "required", "field"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["prompt", "questions"],
            "additionalProperties": false
        }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SessionScopeId(String);

impl SessionScopeId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("session scope id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct LogicalRunId(String);

impl LogicalRunId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("logical run id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct UserInputRequestId(String);

impl UserInputRequestId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("user input request id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct UserInputCommandId(String);

impl UserInputCommandId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("user input command id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct UserInputClaimId(String);

impl UserInputClaimId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("user input continuation claim id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputIdentityV1 {
    pub session_scope_id: SessionScopeId,
    pub root_logical_run_id: LogicalRunId,
    pub source_thread_id: AgentThreadId,
    pub request_id: UserInputRequestId,
    pub generation: u32,
    pub source_binding_hash: String,
}

impl UserInputIdentityV1 {
    pub fn validate(&self) -> Result<()> {
        if self.generation == 0 {
            bail!("user input request generation must start at one");
        }
        validate_sha256("user input source binding hash", &self.source_binding_hash)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum UserInputSourceV1 {
    Agent,
    PlanReviewResearch {
        plan_review_id: crate::PlanReviewId,
        attempt_id: crate::PlanReviewAttemptId,
    },
    PlanRevision {
        base_plan_id: PlanId,
        base_plan_hash: String,
    },
    Planner {
        task_id: TaskId,
    },
    Mcp {
        server_id: String,
        call_id: String,
    },
}

impl UserInputSourceV1 {
    pub fn persists_answer_values(&self) -> bool {
        !matches!(self, Self::Mcp { .. })
    }

    pub fn starts_agent_continuation(&self) -> bool {
        matches!(
            self,
            Self::Agent | Self::PlanReviewResearch { .. } | Self::Planner { .. }
        )
    }

    fn counts_against_root_limit(&self) -> bool {
        matches!(
            self,
            Self::Agent | Self::PlanReviewResearch { .. } | Self::Planner { .. }
        )
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Agent => Ok(()),
            Self::PlanReviewResearch { .. } => Ok(()),
            Self::PlanRevision { base_plan_hash, .. } => {
                validate_sha256("base plan hash", base_plan_hash)
            }
            Self::Planner { .. } => Ok(()),
            Self::Mcp { server_id, call_id } => {
                validate_bounded_text("MCP server id", server_id, 128, false)?;
                validate_bounded_text("MCP call id", call_id, 128, false)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserInputPurposeV1 {
    Clarification,
    Choice,
    MissingConstraint,
    RevisionGuidance,
    ExternalElicitation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UserInputActionV1 {
    Submit,
    Decline,
    CancelRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputOptionV1 {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl UserInputOptionV1 {
    fn validate(&self) -> Result<()> {
        validate_field_id("user input option id", &self.id)?;
        validate_bounded_text("user input option label", &self.label, 80, false)?;
        if let Some(description) = &self.description {
            validate_bounded_text("user input option description", description, 240, true)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserInputFieldKindV1 {
    Text {
        multiline: bool,
        max_chars: u32,
    },
    Number,
    Integer,
    Boolean,
    SingleSelect {
        options: Vec<UserInputOptionV1>,
        allow_other: bool,
    },
    MultiSelect {
        options: Vec<UserInputOptionV1>,
        max_selected: u32,
    },
}

impl UserInputFieldKindV1 {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Text { max_chars, .. } => {
                if !(1..=MAX_USER_INPUT_TEXT_CHARS).contains(max_chars) {
                    bail!("user input text max_chars is outside the supported range");
                }
            }
            Self::Number | Self::Integer | Self::Boolean => {}
            Self::SingleSelect { options, .. } => validate_options(options)?,
            Self::MultiSelect {
                options,
                max_selected,
            } => {
                validate_options(options)?;
                let max_selected = usize::try_from(*max_selected)
                    .context("user input multi-select max exceeds usize")?;
                if max_selected == 0 || max_selected > options.len() {
                    bail!("user input multi-select max_selected is invalid");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputQuestionV1 {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub required: bool,
    pub field: UserInputFieldKindV1,
}

impl UserInputQuestionV1 {
    fn validate(&self) -> Result<()> {
        validate_field_id("user input question id", &self.id)?;
        validate_bounded_text("user input question header", &self.header, 32, false)?;
        validate_bounded_text(
            "user input question",
            &self.question,
            MAX_USER_INPUT_PROMPT_CHARS,
            false,
        )?;
        if let Some(description) = &self.description {
            validate_bounded_text(
                "user input question description",
                description,
                MAX_USER_INPUT_DESCRIPTION_CHARS,
                true,
            )?;
        }
        self.field.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputContinuationBindingV1 {
    pub assistant_message_id: String,
    pub tool_call_id: String,
    pub provider_name: String,
    pub model_name: String,
}

impl UserInputContinuationBindingV1 {
    fn validate(&self) -> Result<()> {
        validate_bounded_text(
            "user input assistant message id",
            &self.assistant_message_id,
            128,
            false,
        )?;
        validate_bounded_text("user input tool call id", &self.tool_call_id, 128, false)?;
        validate_bounded_text("user input provider name", &self.provider_name, 96, false)?;
        validate_bounded_text("user input model name", &self.model_name, 160, false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputRequestV1 {
    pub schema_version: u16,
    pub identity: UserInputIdentityV1,
    pub source: UserInputSourceV1,
    pub purpose: UserInputPurposeV1,
    pub prompt: String,
    pub questions: Vec<UserInputQuestionV1>,
    pub allowed_actions: Vec<UserInputActionV1>,
    pub requested_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<UserInputContinuationBindingV1>,
}

impl UserInputRequestV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != USER_INPUT_SCHEMA_VERSION {
            bail!(
                "unsupported user input request schema version {}",
                self.schema_version
            );
        }
        self.identity.validate()?;
        self.source.validate()?;
        validate_bounded_text(
            "user input request prompt",
            &self.prompt,
            MAX_USER_INPUT_PROMPT_CHARS,
            false,
        )?;
        if self.questions.is_empty() || self.questions.len() > MAX_USER_INPUT_QUESTIONS {
            bail!("user input request must contain one to three questions");
        }
        let mut question_ids = BTreeSet::new();
        for question in &self.questions {
            question.validate()?;
            if !question_ids.insert(question.id.as_str()) {
                bail!("user input request repeats a question id");
            }
        }
        let actions = self
            .allowed_actions
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if actions.len() != self.allowed_actions.len()
            || !actions.contains(&UserInputActionV1::Submit)
        {
            bail!("user input request actions must be unique and include submit");
        }
        match (&self.source, &self.continuation) {
            (UserInputSourceV1::Mcp { .. }, None) => {}
            (UserInputSourceV1::Mcp { .. }, Some(_)) => {
                bail!("MCP user input must not claim an agent continuation")
            }
            (UserInputSourceV1::PlanRevision { .. }, None) => {}
            (UserInputSourceV1::PlanRevision { .. }, Some(_)) => {
                bail!("host-owned plan revision guidance must not claim an agent continuation")
            }
            (_, Some(continuation)) => continuation.validate()?,
            (_, None) => bail!("agent-owned user input requires a continuation binding"),
        }
        let bytes = serde_json::to_vec(self).context("failed to size user input request")?;
        if bytes.len() > MAX_USER_INPUT_REQUEST_BYTES {
            bail!("user input request exceeds the durable size limit");
        }
        Ok(())
    }

    pub fn request_hash(&self) -> Result<String> {
        self.validate()?;
        let value = serde_json::to_value(self).context("failed to encode user input request")?;
        let bytes = crate::event::canonical_json_bytes(&value)
            .context("failed to canonicalize user input request")?;
        Ok(crate::stable_event_hash(&bytes))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputRequestedV1 {
    pub request: UserInputRequestV1,
    pub request_hash: String,
}

impl UserInputRequestedV1 {
    pub fn new(request: UserInputRequestV1) -> Result<Self> {
        let request_hash = request.request_hash()?;
        Ok(Self {
            request,
            request_hash,
        })
    }

    pub fn validate(&self) -> Result<()> {
        validate_sha256("user input request hash", &self.request_hash)?;
        if self.request.request_hash()? != self.request_hash {
            bail!("user input request hash does not match the normalized request");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserInputAnswerValueV1 {
    Text {
        value: String,
    },
    Number {
        value: String,
    },
    Integer {
        value: i64,
    },
    Boolean {
        value: bool,
    },
    SingleSelect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        other: Option<String>,
    },
    MultiSelect {
        option_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputAnswerV1 {
    pub question_id: String,
    pub value: UserInputAnswerValueV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserInputDecisionV1 {
    Submitted { answers: Vec<UserInputAnswerV1> },
    Declined,
    RunCancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserInputDurableDecisionV1 {
    Submitted {
        answer_hash: String,
        answered_question_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<Vec<UserInputAnswerV1>>,
    },
    Declined,
    RunCancelled,
}

impl UserInputDurableDecisionV1 {
    pub fn is_submitted(&self) -> bool {
        matches!(self, Self::Submitted { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputDecisionAcceptedV1 {
    pub schema_version: u16,
    pub identity: UserInputIdentityV1,
    pub request_hash: String,
    pub command_id: UserInputCommandId,
    pub decision: UserInputDurableDecisionV1,
    pub accepted_at_unix_ms: u64,
}

impl UserInputDecisionAcceptedV1 {
    pub fn new(
        requested: &UserInputRequestedV1,
        command_id: UserInputCommandId,
        decision: UserInputDecisionV1,
        accepted_at_unix_ms: u64,
    ) -> Result<Self> {
        requested.validate()?;
        let durable = match decision {
            UserInputDecisionV1::Submitted { answers } => {
                validate_answers(&requested.request, &answers)?;
                let value = serde_json::to_value(&answers)
                    .context("failed to encode user input answers")?;
                let bytes = crate::event::canonical_json_bytes(&value)
                    .context("failed to canonicalize user input answers")?;
                if bytes.len() > MAX_USER_INPUT_ANSWER_BYTES {
                    bail!("user input answer exceeds the durable size limit");
                }
                let answer_hash = crate::stable_event_hash(&bytes);
                let answered_question_ids = answers
                    .iter()
                    .map(|answer| answer.question_id.clone())
                    .collect();
                UserInputDurableDecisionV1::Submitted {
                    answer_hash,
                    answered_question_ids,
                    answers: requested
                        .request
                        .source
                        .persists_answer_values()
                        .then_some(answers),
                }
            }
            UserInputDecisionV1::Declined => UserInputDurableDecisionV1::Declined,
            UserInputDecisionV1::RunCancelled => UserInputDurableDecisionV1::RunCancelled,
        };
        let entry = Self {
            schema_version: USER_INPUT_SCHEMA_VERSION,
            identity: requested.request.identity.clone(),
            request_hash: requested.request_hash.clone(),
            command_id,
            decision: durable,
            accepted_at_unix_ms,
        };
        entry.validate_against(requested)?;
        Ok(entry)
    }

    pub fn validate_against(&self, requested: &UserInputRequestedV1) -> Result<()> {
        self.validate_shape()?;
        if self.identity != requested.request.identity
            || self.request_hash != requested.request_hash
        {
            bail!("user input decision does not bind the exact request");
        }
        match &self.decision {
            UserInputDurableDecisionV1::Submitted {
                answer_hash,
                answered_question_ids,
                answers,
            } => {
                validate_sha256("user input answer hash", answer_hash)?;
                let unique = answered_question_ids.iter().collect::<BTreeSet<_>>();
                if unique.len() != answered_question_ids.len() {
                    bail!("user input decision repeats an answered question id");
                }
                if requested.request.source.persists_answer_values() {
                    let answers = answers
                        .as_ref()
                        .context("agent user input decision omitted durable answers")?;
                    validate_answers(&requested.request, answers)?;
                    let value = serde_json::to_value(answers)
                        .context("failed to encode durable user input answers")?;
                    let bytes = crate::event::canonical_json_bytes(&value)
                        .context("failed to canonicalize durable user input answers")?;
                    if crate::stable_event_hash(&bytes) != *answer_hash {
                        bail!("durable user input answer hash does not match the answer");
                    }
                } else if answers.is_some() {
                    bail!("MCP user input decision must not persist answer values");
                }
            }
            UserInputDurableDecisionV1::Declined => {
                if !requested
                    .request
                    .allowed_actions
                    .contains(&UserInputActionV1::Decline)
                {
                    bail!("user input request does not allow decline");
                }
            }
            UserInputDurableDecisionV1::RunCancelled => {
                if !requested
                    .request
                    .allowed_actions
                    .contains(&UserInputActionV1::CancelRun)
                {
                    bail!("user input request does not allow run cancellation");
                }
            }
        }
        Ok(())
    }

    pub fn validate_shape(&self) -> Result<()> {
        if self.schema_version != USER_INPUT_SCHEMA_VERSION {
            bail!("unsupported user input decision schema version");
        }
        self.identity.validate()?;
        validate_sha256("user input decision request hash", &self.request_hash)?;
        match &self.decision {
            UserInputDurableDecisionV1::Submitted {
                answer_hash,
                answered_question_ids,
                answers,
            } => {
                validate_sha256("user input answer hash", answer_hash)?;
                let mut unique = BTreeSet::new();
                for question_id in answered_question_ids {
                    validate_field_id("answered user input question id", question_id)?;
                    if !unique.insert(question_id) {
                        bail!("user input decision repeats an answered question id");
                    }
                }
                if let Some(answers) = answers {
                    let bytes = serde_json::to_vec(answers)
                        .context("failed to size durable user input answers")?;
                    if bytes.len() > MAX_USER_INPUT_ANSWER_BYTES {
                        bail!("user input answer exceeds the durable size limit");
                    }
                }
            }
            UserInputDurableDecisionV1::Declined | UserInputDurableDecisionV1::RunCancelled => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputContinuationClaimedV1 {
    pub schema_version: u16,
    pub identity: UserInputIdentityV1,
    pub request_hash: String,
    pub claim_id: UserInputClaimId,
    pub supervisor_instance_id: String,
    pub claimed_at_unix_ms: u64,
}

impl UserInputContinuationClaimedV1 {
    pub fn validate(&self) -> Result<()> {
        validate_lifecycle_identity(self.schema_version, &self.identity, &self.request_hash)?;
        validate_bounded_text(
            "user input supervisor instance id",
            &self.supervisor_instance_id,
            128,
            false,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputContinuationStartedV1 {
    pub schema_version: u16,
    pub identity: UserInputIdentityV1,
    pub request_hash: String,
    pub claim_id: UserInputClaimId,
    pub continuation_logical_run_id: LogicalRunId,
    pub physical_attempt_id: String,
    pub started_at_unix_ms: u64,
}

impl UserInputContinuationStartedV1 {
    pub fn validate(&self) -> Result<()> {
        validate_lifecycle_identity(self.schema_version, &self.identity, &self.request_hash)?;
        validate_bounded_text(
            "user input continuation physical attempt id",
            &self.physical_attempt_id,
            128,
            false,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserInputResolutionV1 {
    Consumed,
    Declined,
    RunCancelled,
    Failed {
        failure_class: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputResolvedV1 {
    pub schema_version: u16,
    pub identity: UserInputIdentityV1,
    pub request_hash: String,
    pub resolution: UserInputResolutionV1,
    pub resolved_at_unix_ms: u64,
}

impl UserInputResolvedV1 {
    pub fn validate(&self) -> Result<()> {
        validate_lifecycle_identity(self.schema_version, &self.identity, &self.request_hash)?;
        if let UserInputResolutionV1::Failed { failure_class, .. } = &self.resolution {
            validate_bounded_text("user input failure class", failure_class, 128, false)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserInputLifecycleEntryV1 {
    Requested(Box<UserInputRequestedV1>),
    DecisionAccepted(Box<UserInputDecisionAcceptedV1>),
    ContinuationClaimed(UserInputContinuationClaimedV1),
    ContinuationStarted(UserInputContinuationStartedV1),
    Resolved(UserInputResolvedV1),
}

impl UserInputLifecycleEntryV1 {
    pub fn identity(&self) -> &UserInputIdentityV1 {
        match self {
            Self::Requested(entry) => &entry.request.identity,
            Self::DecisionAccepted(entry) => &entry.identity,
            Self::ContinuationClaimed(entry) => &entry.identity,
            Self::ContinuationStarted(entry) => &entry.identity,
            Self::Resolved(entry) => &entry.identity,
        }
    }

    pub fn request_hash(&self) -> &str {
        match self {
            Self::Requested(entry) => &entry.request_hash,
            Self::DecisionAccepted(entry) => &entry.request_hash,
            Self::ContinuationClaimed(entry) => &entry.request_hash,
            Self::ContinuationStarted(entry) => &entry.request_hash,
            Self::Resolved(entry) => &entry.request_hash,
        }
    }

    pub fn validate_shape(&self) -> Result<()> {
        match self {
            Self::Requested(entry) => entry.validate(),
            Self::DecisionAccepted(entry) => entry.validate_shape(),
            Self::ContinuationClaimed(entry) => entry.validate(),
            Self::ContinuationStarted(entry) => entry.validate(),
            Self::Resolved(entry) => entry.validate(),
        }
    }

    pub fn from_control(control: &ControlEntry) -> Option<Self> {
        match control {
            ControlEntry::UserInputRequested(entry) => Some(Self::Requested(entry.clone())),
            ControlEntry::UserInputDecisionAccepted(entry) => {
                Some(Self::DecisionAccepted(entry.clone()))
            }
            ControlEntry::UserInputContinuationClaimed(entry) => {
                Some(Self::ContinuationClaimed(entry.clone()))
            }
            ControlEntry::UserInputContinuationStarted(entry) => {
                Some(Self::ContinuationStarted(entry.clone()))
            }
            ControlEntry::UserInputResolved(entry) => Some(Self::Resolved(entry.clone())),
            _ => None,
        }
    }

    pub fn into_control(self) -> ControlEntry {
        match self {
            Self::Requested(entry) => ControlEntry::UserInputRequested(entry),
            Self::DecisionAccepted(entry) => ControlEntry::UserInputDecisionAccepted(entry),
            Self::ContinuationClaimed(entry) => ControlEntry::UserInputContinuationClaimed(entry),
            Self::ContinuationStarted(entry) => ControlEntry::UserInputContinuationStarted(entry),
            Self::Resolved(entry) => ControlEntry::UserInputResolved(entry),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserInputStatusV1 {
    Requested,
    DecisionAccepted,
    ContinuationClaimed,
    ContinuationStarted,
    Resolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct UserInputRequestStateV1 {
    pub requested: UserInputRequestedV1,
    pub status: UserInputStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<UserInputDecisionAcceptedV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<UserInputContinuationClaimedV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<UserInputContinuationStartedV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<UserInputResolvedV1>,
}

impl UserInputRequestStateV1 {
    pub fn is_terminal(&self) -> bool {
        self.status == UserInputStatusV1::Resolved
    }

    pub fn public_view(&self) -> PublicUserInputRequestV1 {
        let answer_receipt = self.decision.as_ref().map(|decision| {
            let (decision_kind, answer_hash, answered_question_ids) = match &decision.decision {
                UserInputDurableDecisionV1::Submitted {
                    answer_hash,
                    answered_question_ids,
                    ..
                } => (
                    PublicUserInputDecisionKindV1::Submitted,
                    Some(answer_hash.clone()),
                    answered_question_ids.clone(),
                ),
                UserInputDurableDecisionV1::Declined => {
                    (PublicUserInputDecisionKindV1::Declined, None, Vec::new())
                }
                UserInputDurableDecisionV1::RunCancelled => (
                    PublicUserInputDecisionKindV1::RunCancelled,
                    None,
                    Vec::new(),
                ),
            };
            PublicUserInputAnswerReceiptV1 {
                command_id: decision.command_id.clone(),
                decision: decision_kind,
                answer_hash,
                answered_question_ids,
            }
        });
        PublicUserInputRequestV1 {
            identity: self.requested.request.identity.clone(),
            request_hash: self.requested.request_hash.clone(),
            source: self.requested.request.source.clone(),
            purpose: self.requested.request.purpose,
            prompt: self.requested.request.prompt.clone(),
            questions: self.requested.request.questions.clone(),
            allowed_actions: self.requested.request.allowed_actions.clone(),
            requested_at_unix_ms: self.requested.request.requested_at_unix_ms,
            status: self.status,
            answer_receipt,
            resolution: self
                .resolution
                .as_ref()
                .map(|entry| entry.resolution.clone()),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct UserInputProjectionV1 {
    requests: BTreeMap<UserInputIdentityV1, UserInputRequestStateV1>,
}

impl UserInputProjectionV1 {
    pub fn from_lifecycle<'a>(
        entries: impl IntoIterator<Item = &'a UserInputLifecycleEntryV1>,
    ) -> Result<Self> {
        let mut projection = Self::default();
        for entry in entries {
            projection.apply(entry.clone())?;
        }
        Ok(projection)
    }

    pub fn from_session_entries(entries: &[SessionLogEntry]) -> Result<Self> {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(control) = entry else {
                continue;
            };
            if let Some(entry) = UserInputLifecycleEntryV1::from_control(control) {
                projection.apply(entry)?;
            }
        }
        Ok(projection)
    }

    pub fn apply(&mut self, entry: UserInputLifecycleEntryV1) -> Result<()> {
        entry.validate_shape()?;
        match entry {
            UserInputLifecycleEntryV1::Requested(entry) => self.apply_requested(*entry),
            UserInputLifecycleEntryV1::DecisionAccepted(entry) => self.apply_decision(*entry),
            UserInputLifecycleEntryV1::ContinuationClaimed(entry) => self.apply_claim(entry),
            UserInputLifecycleEntryV1::ContinuationStarted(entry) => self.apply_started(entry),
            UserInputLifecycleEntryV1::Resolved(entry) => self.apply_resolved(entry),
        }
    }

    pub fn request(&self, identity: &UserInputIdentityV1) -> Option<&UserInputRequestStateV1> {
        self.requests.get(identity)
    }

    pub fn pending(&self) -> impl Iterator<Item = &UserInputRequestStateV1> {
        self.requests.values().filter(|state| !state.is_terminal())
    }

    pub fn public_requests(&self) -> Vec<PublicUserInputRequestV1> {
        self.requests
            .values()
            .map(UserInputRequestStateV1::public_view)
            .collect()
    }

    pub fn request_for_command(
        &self,
        command_id: &UserInputCommandId,
    ) -> Option<&UserInputRequestStateV1> {
        self.requests.values().find(|state| {
            state
                .decision
                .as_ref()
                .is_some_and(|decision| &decision.command_id == command_id)
        })
    }

    fn apply_requested(&mut self, entry: UserInputRequestedV1) -> Result<()> {
        entry.validate()?;
        let identity = entry.request.identity.clone();
        if self.requests.contains_key(&identity) {
            bail!("user input request generation is already recorded");
        }
        let previous = self
            .requests
            .iter()
            .filter(|(candidate, _)| same_request(candidate, &identity))
            .max_by_key(|(candidate, _)| candidate.generation);
        match previous {
            None if identity.generation != 1 => {
                bail!("first user input request generation must be one")
            }
            Some((previous_identity, previous_state)) => {
                if !previous_state.is_terminal() {
                    bail!("previous user input request generation is not terminal");
                }
                if identity.generation != previous_identity.generation.saturating_add(1) {
                    bail!("user input request generation is not contiguous");
                }
            }
            _ => {}
        }
        if self.pending().any(|state| {
            state.requested.request.identity.source_thread_id == identity.source_thread_id
        }) {
            bail!("source agent already has a pending user input request");
        }
        if entry.request.source.counts_against_root_limit() {
            let prior_count = self
                .requests
                .values()
                .filter(|state| {
                    state.requested.request.identity.root_logical_run_id
                        == identity.root_logical_run_id
                        && state.requested.request.source.counts_against_root_limit()
                })
                .count();
            if prior_count >= MAX_USER_INPUT_REQUESTS_PER_ROOT_RUN {
                bail!("root logical run exhausted its user input request budget");
            }
        }
        self.requests.insert(
            identity,
            UserInputRequestStateV1 {
                requested: entry,
                status: UserInputStatusV1::Requested,
                decision: None,
                claim: None,
                continuation: None,
                resolution: None,
            },
        );
        Ok(())
    }

    fn apply_decision(&mut self, entry: UserInputDecisionAcceptedV1) -> Result<()> {
        let state = self
            .requests
            .get_mut(&entry.identity)
            .context("user input decision references an unknown request")?;
        if state.status != UserInputStatusV1::Requested {
            bail!("user input request already has a decision");
        }
        entry.validate_against(&state.requested)?;
        state.status = UserInputStatusV1::DecisionAccepted;
        state.decision = Some(entry);
        Ok(())
    }

    fn apply_claim(&mut self, entry: UserInputContinuationClaimedV1) -> Result<()> {
        let state = self
            .requests
            .get_mut(&entry.identity)
            .context("user input continuation claim references an unknown request")?;
        validate_exact_request_binding(state, &entry.request_hash)?;
        if state.status != UserInputStatusV1::DecisionAccepted
            || !state
                .decision
                .as_ref()
                .is_some_and(|decision| decision.decision.is_submitted())
        {
            bail!("user input continuation requires a submitted decision");
        }
        if !state.requested.request.source.starts_agent_continuation() {
            bail!("MCP user input cannot claim an agent continuation");
        }
        state.status = UserInputStatusV1::ContinuationClaimed;
        state.claim = Some(entry);
        Ok(())
    }

    fn apply_started(&mut self, entry: UserInputContinuationStartedV1) -> Result<()> {
        let state = self
            .requests
            .get_mut(&entry.identity)
            .context("user input continuation start references an unknown request")?;
        validate_exact_request_binding(state, &entry.request_hash)?;
        if state.status != UserInputStatusV1::ContinuationClaimed
            || state
                .claim
                .as_ref()
                .is_none_or(|claim| claim.claim_id != entry.claim_id)
        {
            bail!("user input continuation start does not match the live claim");
        }
        state.status = UserInputStatusV1::ContinuationStarted;
        state.continuation = Some(entry);
        Ok(())
    }

    fn apply_resolved(&mut self, entry: UserInputResolvedV1) -> Result<()> {
        let state = self
            .requests
            .get_mut(&entry.identity)
            .context("user input resolution references an unknown request")?;
        validate_exact_request_binding(state, &entry.request_hash)?;
        if state.is_terminal() {
            bail!("user input request is already resolved");
        }
        let decision = state
            .decision
            .as_ref()
            .context("user input resolution requires an accepted decision")?;
        let valid = match (&entry.resolution, &decision.decision, state.status) {
            (
                UserInputResolutionV1::Declined,
                UserInputDurableDecisionV1::Declined,
                UserInputStatusV1::DecisionAccepted,
            )
            | (
                UserInputResolutionV1::RunCancelled,
                UserInputDurableDecisionV1::RunCancelled,
                UserInputStatusV1::DecisionAccepted,
            ) => true,
            (
                UserInputResolutionV1::Consumed,
                UserInputDurableDecisionV1::Submitted { .. },
                UserInputStatusV1::ContinuationStarted,
            ) => true,
            (
                UserInputResolutionV1::Consumed,
                UserInputDurableDecisionV1::Submitted { .. },
                UserInputStatusV1::DecisionAccepted,
            ) if !state.requested.request.source.starts_agent_continuation() => true,
            (
                UserInputResolutionV1::Failed { .. },
                UserInputDurableDecisionV1::Submitted { .. },
                UserInputStatusV1::ContinuationClaimed | UserInputStatusV1::ContinuationStarted,
            ) => true,
            _ => false,
        };
        if !valid {
            bail!("user input resolution is not valid for the current state");
        }
        state.status = UserInputStatusV1::Resolved;
        state.resolution = Some(entry);
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputDecisionCommandV1 {
    pub identity: UserInputIdentityV1,
    pub request_hash: String,
    pub command_id: UserInputCommandId,
    pub decision: UserInputDecisionV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputDecisionReceiptV1 {
    pub request: PublicUserInputRequestV1,
    pub idempotent_replay: bool,
    pub continuation_required: bool,
}

/// Validates and projects one exact user-input decision without mutating the session.
///
/// Runtime adapters use this before fallible provider/tool-surface preparation so malformed or
/// stale answers fail early, while a successfully accepted answer remains the final durable
/// mutation immediately before its supervised continuation is handed to an owner.
pub fn preview_user_input_decision(
    session: &Session,
    command: &UserInputDecisionCommandV1,
    accepted_at_unix_ms: u64,
) -> Result<UserInputDecisionReceiptV1> {
    validate_sha256("user input decision request hash", &command.request_hash)?;
    let projection = session.user_input_projection()?;
    if let Some(previous) = projection.request_for_command(&command.command_id) {
        let previous_decision = previous
            .decision
            .as_ref()
            .context("user input command projection lost its decision")?;
        let candidate = UserInputDecisionAcceptedV1::new(
            &previous.requested,
            command.command_id.clone(),
            command.decision.clone(),
            previous_decision.accepted_at_unix_ms,
        )?;
        if previous.requested.request.identity != command.identity
            || previous.requested.request_hash != command.request_hash
            || &candidate != previous_decision
        {
            bail!("user input command id is already bound to a different request or decision");
        }
        return Ok(UserInputDecisionReceiptV1 {
            request: previous.public_view(),
            idempotent_replay: true,
            continuation_required: previous_decision.decision.is_submitted()
                && !previous.is_terminal(),
        });
    }
    let state = projection
        .request(&command.identity)
        .cloned()
        .context("user input decision references an unknown request")?;
    if state.requested.request_hash != command.request_hash {
        bail!("user input decision does not bind the exact request hash");
    }
    if state.status != UserInputStatusV1::Requested {
        bail!("user input request already has a different decision");
    }
    let accepted = UserInputDecisionAcceptedV1::new(
        &state.requested,
        command.command_id.clone(),
        command.decision.clone(),
        accepted_at_unix_ms,
    )?;
    let mut candidate = projection;
    candidate.apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(
        accepted,
    )))?;
    let current = candidate
        .request(&command.identity)
        .context("previewed user input decision was not projected")?;
    Ok(UserInputDecisionReceiptV1 {
        request: current.public_view(),
        idempotent_replay: false,
        continuation_required: current
            .decision
            .as_ref()
            .is_some_and(|decision| decision.decision.is_submitted())
            && !current.is_terminal(),
    })
}

/// Applies an exact user-input decision with command-level idempotency.
///
/// Submitted agent answers are intentionally committed before continuation ownership is claimed;
/// a crash in between is recoverable by [`prepare_user_input_continuation`]. Decline and cancel
/// decisions settle the pending internal tool call in the same durable bundle because no model
/// continuation will do that work later.
pub fn accept_user_input_decision(
    session: &mut Session,
    command: UserInputDecisionCommandV1,
    accepted_at_unix_ms: u64,
) -> Result<UserInputDecisionReceiptV1> {
    validate_sha256("user input decision request hash", &command.request_hash)?;
    let projection = session.user_input_projection()?;
    if let Some(previous) = projection.request_for_command(&command.command_id) {
        let previous_decision = previous
            .decision
            .as_ref()
            .context("user input command projection lost its decision")?;
        let candidate = UserInputDecisionAcceptedV1::new(
            &previous.requested,
            command.command_id,
            command.decision,
            previous_decision.accepted_at_unix_ms,
        )?;
        if previous.requested.request.identity != command.identity
            || previous.requested.request_hash != command.request_hash
            || &candidate != previous_decision
        {
            bail!("user input command id is already bound to a different request or decision");
        }
        return Ok(UserInputDecisionReceiptV1 {
            request: previous.public_view(),
            idempotent_replay: true,
            continuation_required: previous_decision.decision.is_submitted()
                && !previous.is_terminal(),
        });
    }
    let state = projection
        .request(&command.identity)
        .cloned()
        .context("user input decision references an unknown request")?;
    if state.requested.request_hash != command.request_hash {
        bail!("user input decision does not bind the exact request hash");
    }
    if state.status != UserInputStatusV1::Requested {
        bail!("user input request already has a different decision");
    }
    let accepted = UserInputDecisionAcceptedV1::new(
        &state.requested,
        command.command_id,
        command.decision,
        accepted_at_unix_ms,
    )?;
    if accepted.decision.is_submitted() {
        session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::DecisionAccepted(
            Box::new(accepted),
        )])?;
    } else if matches!(
        &state.requested.request.source,
        UserInputSourceV1::Mcp { .. }
    ) {
        let resolution = terminal_resolution_for_decision(&accepted)?;
        session.append_user_input_lifecycle(vec![
            UserInputLifecycleEntryV1::DecisionAccepted(Box::new(accepted)),
            UserInputLifecycleEntryV1::Resolved(resolution),
        ])?;
    } else {
        settle_terminal_user_input_decision(session, &state.requested, accepted)?;
    }
    let projected = session.user_input_projection()?;
    let current = projected
        .request(&command.identity)
        .context("accepted user input decision was not projected")?;
    Ok(UserInputDecisionReceiptV1 {
        request: current.public_view(),
        idempotent_replay: false,
        continuation_required: current
            .decision
            .as_ref()
            .is_some_and(|decision| decision.decision.is_submitted())
            && !current.is_terminal(),
    })
}

/// Reconstructs the exact retry-stable command for one durably accepted answer that has not yet
/// started its continuation.
///
/// Answer values are read only from private session truth and are never added to the public
/// projection. MCP requests intentionally return no recovery command because their durable policy
/// stores only a value hash and forbids replay to a disconnected server.
///
/// # Errors
///
/// Returns an error when more than one foreground-blocking accepted answer exists or the durable
/// decision is missing the answer values required for an ordinary agent continuation.
pub fn recoverable_user_input_decision(
    session: &Session,
) -> Result<Option<UserInputDecisionCommandV1>> {
    recoverable_user_input_decision_from_entries(session.entries())
}

/// Reconstructs an accepted answer recovery command from a validated append-only session stream.
///
/// This entry-based form lets adapters recover a historical session without constructing a
/// provider-bound mutable [`Session`].
pub fn recoverable_user_input_decision_from_entries(
    entries: &[SessionLogEntry],
) -> Result<Option<UserInputDecisionCommandV1>> {
    let projection = UserInputProjectionV1::from_session_entries(entries)?;
    let mut recoverable = projection.pending().filter(|state| {
        state.status == UserInputStatusV1::DecisionAccepted
            && !matches!(
                state.requested.request.source,
                UserInputSourceV1::Mcp { .. }
            )
    });
    let Some(state) = recoverable.next() else {
        return Ok(None);
    };
    if recoverable.next().is_some() {
        bail!("session contains multiple accepted user-input answers awaiting continuation");
    }
    let accepted = state
        .decision
        .as_ref()
        .context("accepted user-input request lost its durable decision")?;
    let decision = match &accepted.decision {
        UserInputDurableDecisionV1::Submitted { answers, .. } => UserInputDecisionV1::Submitted {
            answers: answers
                .clone()
                .context("accepted user-input answer omitted durable recovery values")?,
        },
        UserInputDurableDecisionV1::Declined | UserInputDurableDecisionV1::RunCancelled => {
            bail!("terminal user-input decision remained pending")
        }
    };
    Ok(Some(UserInputDecisionCommandV1 {
        identity: accepted.identity.clone(),
        request_hash: accepted.request_hash.clone(),
        command_id: accepted.command_id.clone(),
        decision,
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputContinuationPreparationV1 {
    pub request: PublicUserInputRequestV1,
    pub continuation: UserInputContinuationStartedV1,
    pub already_started: bool,
}

/// Derives the retry-stable logical run identity used after one accepted user-input decision.
///
/// # Errors
///
/// Returns an error when the request binding is malformed or the derived identity is invalid.
pub fn user_input_continuation_logical_run_id(
    identity: &UserInputIdentityV1,
    request_hash: &str,
) -> Result<LogicalRunId> {
    LogicalRunId::new(stable_continuation_id(
        "continuation",
        identity,
        request_hash,
    )?)
}

/// Claims and starts the one logical continuation for an accepted agent answer.
///
/// Claim, start, provider-visible tool result, and terminal tool audit are appended atomically.
/// If recovery observes an already-started continuation, this function verifies that the matching
/// tool result exists and returns the same durable identity without starting another logical run.
pub fn prepare_user_input_continuation(
    session: &mut Session,
    identity: &UserInputIdentityV1,
    request_hash: &str,
    supervisor_instance_id: &str,
    physical_attempt_id: &str,
    now_unix_ms: u64,
) -> Result<UserInputContinuationPreparationV1> {
    let projection = session.user_input_projection()?;
    let state = projection
        .request(identity)
        .cloned()
        .context("user input continuation references an unknown request")?;
    if state.requested.request_hash != request_hash {
        bail!("user input continuation does not bind the exact request hash");
    }
    if let Some(started) = state.continuation.as_ref() {
        let binding = state
            .requested
            .request
            .continuation
            .as_ref()
            .context("agent user input lost its continuation binding")?;
        if !session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::ToolResultV3(result)
                    if result.call_id == binding.tool_call_id
                        && result.tool_name == REQUEST_USER_INPUT_TOOL_NAME
            )
        }) {
            bail!("started user input continuation is missing its settled tool result");
        }
        return Ok(UserInputContinuationPreparationV1 {
            request: state.public_view(),
            continuation: started.clone(),
            already_started: true,
        });
    }
    if state.status != UserInputStatusV1::DecisionAccepted
        || !state
            .decision
            .as_ref()
            .is_some_and(|decision| decision.decision.is_submitted())
    {
        bail!("user input request is not ready for continuation");
    }
    validate_bounded_text(
        "user input supervisor instance id",
        supervisor_instance_id,
        128,
        false,
    )?;
    validate_bounded_text(
        "user input continuation physical attempt id",
        physical_attempt_id,
        128,
        false,
    )?;
    let binding = state
        .requested
        .request
        .continuation
        .as_ref()
        .context("agent user input lost its continuation binding")?;
    let call = exact_user_input_tool_call(session, binding)?;
    let claim_id = UserInputClaimId::new(stable_continuation_id("claim", identity, request_hash)?)?;
    let continuation_logical_run_id =
        user_input_continuation_logical_run_id(identity, request_hash)?;
    let claim = UserInputContinuationClaimedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: identity.clone(),
        request_hash: request_hash.to_owned(),
        claim_id: claim_id.clone(),
        supervisor_instance_id: supervisor_instance_id.to_owned(),
        claimed_at_unix_ms: now_unix_ms,
    };
    let started = UserInputContinuationStartedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: identity.clone(),
        request_hash: request_hash.to_owned(),
        claim_id,
        continuation_logical_run_id,
        physical_attempt_id: physical_attempt_id.to_owned(),
        started_at_unix_ms: now_unix_ms,
    };
    let result = submitted_answer_tool_result(&state, &call)?;
    let (recorded, _) = ToolResultRecordedV3::capture(
        &result,
        session.tool_artifact_store().as_ref(),
        ToolArtifactSensitivity::Ordinary,
    )?;
    let execution = crate::agent::tool_audit::durable_tool_execution_entry(
        &call,
        &[],
        ToolExecutionStatus::Completed,
        None,
        Some(&result),
    )?;
    session.append_user_input_tool_settlement_bundle(
        recorded,
        vec![
            UserInputLifecycleEntryV1::ContinuationClaimed(claim),
            UserInputLifecycleEntryV1::ContinuationStarted(started.clone()),
        ],
        execution,
    )?;
    let projected = session.user_input_projection()?;
    let current = projected
        .request(identity)
        .context("started user input continuation was not projected")?;
    Ok(UserInputContinuationPreparationV1 {
        request: current.public_view(),
        continuation: started,
        already_started: false,
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicUserInputDecisionKindV1 {
    Submitted,
    Declined,
    RunCancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicUserInputAnswerReceiptV1 {
    pub command_id: UserInputCommandId,
    pub decision: PublicUserInputDecisionKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answered_question_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicUserInputRequestV1 {
    pub identity: UserInputIdentityV1,
    pub request_hash: String,
    pub source: UserInputSourceV1,
    pub purpose: UserInputPurposeV1,
    pub prompt: String,
    pub questions: Vec<UserInputQuestionV1>,
    pub allowed_actions: Vec<UserInputActionV1>,
    pub requested_at_unix_ms: u64,
    pub status: UserInputStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_receipt: Option<PublicUserInputAnswerReceiptV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<UserInputResolutionV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UserInputRequestRefV1 {
    pub identity: UserInputIdentityV1,
    pub request_hash: String,
}

impl From<&UserInputRequestedV1> for UserInputRequestRefV1 {
    fn from(value: &UserInputRequestedV1) -> Self {
        Self {
            identity: value.request.identity.clone(),
            request_hash: value.request_hash.clone(),
        }
    }
}

fn terminal_resolution_for_decision(
    decision: &UserInputDecisionAcceptedV1,
) -> Result<UserInputResolvedV1> {
    let resolution = match decision.decision {
        UserInputDurableDecisionV1::Declined => UserInputResolutionV1::Declined,
        UserInputDurableDecisionV1::RunCancelled => UserInputResolutionV1::RunCancelled,
        UserInputDurableDecisionV1::Submitted { .. } => {
            bail!("submitted user input decision is not terminal before continuation")
        }
    };
    Ok(UserInputResolvedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: decision.identity.clone(),
        request_hash: decision.request_hash.clone(),
        resolution,
        resolved_at_unix_ms: decision.accepted_at_unix_ms,
    })
}

fn settle_terminal_user_input_decision(
    session: &mut Session,
    requested: &UserInputRequestedV1,
    decision: UserInputDecisionAcceptedV1,
) -> Result<()> {
    let binding = requested
        .request
        .continuation
        .as_ref()
        .context("agent user input lost its continuation binding")?;
    let call = exact_user_input_tool_call(session, binding)?;
    let resolution = terminal_resolution_for_decision(&decision)?;
    let content = match decision.decision {
        UserInputDurableDecisionV1::Declined => {
            "The user declined to answer this request. Continue only if a safe default is available; otherwise explain the unresolved decision."
        }
        UserInputDurableDecisionV1::RunCancelled => {
            "The user cancelled the run while this input request was pending."
        }
        UserInputDurableDecisionV1::Submitted { .. } => {
            bail!("submitted user input decision requires a continuation")
        }
    };
    let result = ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        content,
        user_input_result_metadata(requested),
    );
    let (recorded, _) = ToolResultRecordedV3::capture(
        &result,
        session.tool_artifact_store().as_ref(),
        ToolArtifactSensitivity::Ordinary,
    )?;
    let execution = crate::agent::tool_audit::durable_tool_execution_entry(
        &call,
        &[],
        ToolExecutionStatus::Completed,
        None,
        Some(&result),
    )?;
    session.append_user_input_tool_settlement_bundle(
        recorded,
        vec![
            UserInputLifecycleEntryV1::DecisionAccepted(Box::new(decision)),
            UserInputLifecycleEntryV1::Resolved(resolution),
        ],
        execution,
    )
}

fn submitted_answer_tool_result(
    state: &UserInputRequestStateV1,
    call: &crate::ToolCall,
) -> Result<ToolResult> {
    let answers = match state
        .decision
        .as_ref()
        .context("submitted user input state lost its decision")?
        .decision
        .clone()
    {
        UserInputDurableDecisionV1::Submitted {
            answers: Some(answers),
            ..
        } => answers,
        UserInputDurableDecisionV1::Submitted { answers: None, .. } => {
            bail!("agent user input answer values are unavailable for continuation")
        }
        _ => bail!("user input continuation requires a submitted answer"),
    };
    let content = serde_json::to_string(&json!({
        "decision": "submitted",
        "answers": answers,
    }))?;
    Ok(ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        content,
        user_input_result_metadata(&state.requested),
    ))
}

fn user_input_result_metadata(requested: &UserInputRequestedV1) -> ToolResultMeta {
    ToolResultMeta {
        details: json!({
            "request_id": requested.request.identity.request_id.as_str(),
            "generation": requested.request.identity.generation,
            "request_hash": requested.request_hash,
        }),
        ..ToolResultMeta::default()
    }
}

fn exact_user_input_tool_call(
    session: &Session,
    binding: &UserInputContinuationBindingV1,
) -> Result<crate::ToolCall> {
    session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Assistant(message) if message.id == binding.assistant_message_id => {
                message
                    .tool_calls
                    .iter()
                    .find(|call| {
                        call.id == binding.tool_call_id && call.name == REQUEST_USER_INPUT_TOOL_NAME
                    })
                    .cloned()
            }
            _ => None,
        })
        .context("user input continuation cannot find its exact assistant tool call")
}

fn stable_continuation_id(
    prefix: &str,
    identity: &UserInputIdentityV1,
    request_hash: &str,
) -> Result<String> {
    let bytes = crate::event::canonical_json_bytes(&json!({
        "contract": "sigil-user-input-continuation-v1",
        "prefix": prefix,
        "identity": identity,
        "request_hash": request_hash,
    }))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("input-{prefix}-{digest:x}"))
}

fn validate_answers(request: &UserInputRequestV1, answers: &[UserInputAnswerV1]) -> Result<()> {
    let mut by_id = BTreeMap::new();
    for answer in answers {
        validate_field_id("user input answer question id", &answer.question_id)?;
        if by_id.insert(answer.question_id.as_str(), answer).is_some() {
            bail!("user input answer repeats a question id");
        }
    }
    for answer_id in by_id.keys() {
        if !request
            .questions
            .iter()
            .any(|question| question.id == **answer_id)
        {
            bail!("user input answer references an unknown question");
        }
    }
    for question in &request.questions {
        let Some(answer) = by_id.get(question.id.as_str()) else {
            if question.required {
                bail!("required user input question is unanswered");
            }
            continue;
        };
        validate_answer_value(question, &answer.value)?;
    }
    Ok(())
}

fn validate_answer_value(
    question: &UserInputQuestionV1,
    answer: &UserInputAnswerValueV1,
) -> Result<()> {
    match (&question.field, answer) {
        (UserInputFieldKindV1::Text { max_chars, .. }, UserInputAnswerValueV1::Text { value }) => {
            validate_bounded_text(
                "user input text answer",
                value,
                usize::try_from(*max_chars).context("user input text bound exceeds usize")?,
                !question.required,
            )?;
        }
        (UserInputFieldKindV1::Number, UserInputAnswerValueV1::Number { value }) => {
            validate_bounded_text("user input number answer", value, 64, false)?;
            let parsed = value
                .parse::<f64>()
                .context("user input number answer is not numeric")?;
            if !parsed.is_finite() {
                bail!("user input number answer must be finite");
            }
        }
        (UserInputFieldKindV1::Integer, UserInputAnswerValueV1::Integer { .. })
        | (UserInputFieldKindV1::Boolean, UserInputAnswerValueV1::Boolean { .. }) => {}
        (
            UserInputFieldKindV1::SingleSelect {
                options,
                allow_other,
            },
            UserInputAnswerValueV1::SingleSelect { option_id, other },
        ) => match (option_id, other) {
            (Some(option_id), None) if options.iter().any(|option| option.id == *option_id) => {}
            (None, Some(other)) if *allow_other => {
                validate_bounded_text("user input other answer", other, 512, false)?;
            }
            _ => bail!("user input single-select answer is invalid"),
        },
        (
            UserInputFieldKindV1::MultiSelect {
                options,
                max_selected,
            },
            UserInputAnswerValueV1::MultiSelect { option_ids },
        ) => {
            let selected = option_ids.iter().collect::<BTreeSet<_>>();
            if selected.is_empty()
                || selected.len() != option_ids.len()
                || selected.len()
                    > usize::try_from(*max_selected)
                        .context("user input multi-select bound exceeds usize")?
                || selected
                    .iter()
                    .any(|id| !options.iter().any(|option| option.id == **id))
            {
                bail!("user input multi-select answer is invalid");
            }
        }
        _ => bail!("user input answer kind does not match the question"),
    }
    Ok(())
}

fn validate_options(options: &[UserInputOptionV1]) -> Result<()> {
    if !(2..=MAX_USER_INPUT_OPTIONS).contains(&options.len()) {
        bail!("user input select must contain two to twelve options");
    }
    let mut ids = BTreeSet::new();
    for option in options {
        option.validate()?;
        if !ids.insert(option.id.as_str()) {
            bail!("user input select repeats an option id");
        }
    }
    Ok(())
}

fn validate_exact_request_binding(
    state: &UserInputRequestStateV1,
    request_hash: &str,
) -> Result<()> {
    if state.requested.request_hash != request_hash {
        bail!("user input lifecycle entry does not bind the exact request");
    }
    Ok(())
}

fn validate_lifecycle_identity(
    schema_version: u16,
    identity: &UserInputIdentityV1,
    request_hash: &str,
) -> Result<()> {
    if schema_version != USER_INPUT_SCHEMA_VERSION {
        bail!("unsupported user input lifecycle schema version");
    }
    identity.validate()?;
    validate_sha256("user input lifecycle request hash", request_hash)
}

fn same_request(left: &UserInputIdentityV1, right: &UserInputIdentityV1) -> bool {
    left.session_scope_id == right.session_scope_id
        && left.root_logical_run_id == right.root_logical_run_id
        && left.source_thread_id == right.source_thread_id
        && left.request_id == right.request_id
}

fn validate_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 {
        bail!("{label} is empty or too long");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

fn validate_field_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 48 {
        bail!("{label} is empty or too long");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

fn validate_bounded_text(
    label: &str,
    value: &str,
    max_chars: usize,
    allow_empty: bool,
) -> Result<()> {
    let char_count = value.chars().count();
    if (!allow_empty && char_count == 0) || char_count > max_chars {
        bail!("{label} is empty or exceeds its character limit");
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        bail!("{label} contains unsupported control characters");
    }
    Ok(())
}

fn validate_sha256(label: &str, value: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        bail!("{label} must use the sha256 prefix");
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} is not a sha256 digest");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/user_input_tests.rs"]
mod tests;
