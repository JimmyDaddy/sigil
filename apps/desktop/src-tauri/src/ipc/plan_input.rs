use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopPlanDetailInput {
    pub(crate) session_id: String,
    pub(crate) plan_id: String,
    pub(crate) expected_plan_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanReviewDetailSummary {
    pub(crate) plan_id: String,
    pub(crate) plan_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_snapshot_id: Option<String>,
    pub(crate) source: &'static str,
    pub(crate) summary: String,
    pub(crate) steps: Vec<DesktopPlanReviewStepSummary>,
    pub(crate) target_paths: Vec<String>,
    pub(crate) suggested_checks: Vec<DesktopPlanSuggestedCheckSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) risk: Option<String>,
    pub(crate) notes: Vec<String>,
    pub(crate) lineage: DesktopPlanLineageSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) legacy_markdown: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanReviewStepSummary {
    pub(crate) step_id: String,
    pub(crate) title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<&'static str>,
    pub(crate) depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) isolation: Option<&'static str>,
    pub(crate) target_paths: Vec<String>,
    pub(crate) suggested_checks: Vec<DesktopPlanSuggestedCheckSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) risk: Option<String>,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanSuggestedCheckSummary {
    pub(crate) check_spec_id: String,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    pub(crate) effect: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_line: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanLineageSummary {
    pub(crate) source: DesktopPlanSourceSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_review_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attempt_id: Option<String>,
    pub(crate) created_at_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanSourceSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) final_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_turn: Option<DesktopPlanSourceTurnSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) route_decision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_review_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPlanSourceTurnSummary {
    pub(crate) session_scope_id: String,
    pub(crate) message_id: String,
    pub(crate) logical_run_id: String,
}

impl From<sigil_desktop::DesktopPlanReviewDetail> for DesktopPlanReviewDetailSummary {
    fn from(value: sigil_desktop::DesktopPlanReviewDetail) -> Self {
        Self {
            plan_id: value.plan_id,
            plan_hash: value.plan_hash,
            workspace_snapshot_id: value.workspace_snapshot_id,
            source: plan_source_label(value.source),
            summary: value.summary,
            steps: value.steps.into_iter().map(Into::into).collect(),
            target_paths: value.target_paths,
            suggested_checks: value.suggested_checks.into_iter().map(Into::into).collect(),
            risk: value.risk,
            notes: value.notes,
            lineage: value.lineage.into(),
            legacy_markdown: value.legacy_markdown,
        }
    }
}

impl From<sigil_desktop::DesktopPlanReviewStepDetail> for DesktopPlanReviewStepSummary {
    fn from(value: sigil_desktop::DesktopPlanReviewStepDetail) -> Self {
        Self {
            step_id: value.step_id,
            title: value.title,
            display_name: value.display_name,
            detail: value.detail,
            role: value.role.map(plan_role_label),
            depends_on: value.depends_on,
            mode: value.mode.map(plan_mode_label),
            isolation: value.isolation.map(plan_isolation_label),
            target_paths: value.target_paths,
            suggested_checks: value.suggested_checks.into_iter().map(Into::into).collect(),
            risk: value.risk,
            notes: value.notes,
        }
    }
}

impl From<sigil_desktop::DesktopPlanSuggestedCheck> for DesktopPlanSuggestedCheckSummary {
    fn from(value: sigil_desktop::DesktopPlanSuggestedCheck) -> Self {
        Self {
            check_spec_id: value.check_spec_id,
            command: value.command.command,
            args: value.command.args,
            cwd: value.command.cwd,
            effect: plan_effect_label(value.effect),
            source_line: value.source_line,
        }
    }
}

impl From<sigil_desktop::DesktopPlanLineage> for DesktopPlanLineageSummary {
    fn from(value: sigil_desktop::DesktopPlanLineage) -> Self {
        Self {
            source: value.source.into(),
            plan_review_id: value.plan_review_id,
            attempt_id: value.attempt_id,
            created_at_ms: value.created_at_ms,
        }
    }
}

impl From<sigil_desktop::DesktopPlanSourceRef> for DesktopPlanSourceSummary {
    fn from(value: sigil_desktop::DesktopPlanSourceRef) -> Self {
        Self {
            session_ref: value.session_ref,
            run_id: value.run_id,
            final_message_id: value.final_message_id,
            source_turn: value.source_turn.map(Into::into),
            route_decision_id: value.route_decision_id,
            plan_review_id: value.plan_review_id,
        }
    }
}

impl From<sigil_desktop::DesktopPlanSourceTurn> for DesktopPlanSourceTurnSummary {
    fn from(value: sigil_desktop::DesktopPlanSourceTurn) -> Self {
        Self {
            session_scope_id: value.session_scope_id,
            message_id: value.message_id,
            logical_run_id: value.logical_run_id,
        }
    }
}

fn plan_source_label(value: sigil_desktop::DesktopPlanReviewSource) -> &'static str {
    match value {
        sigil_desktop::DesktopPlanReviewSource::ExplicitPlanCommand => "explicit_plan_command",
        sigil_desktop::DesktopPlanReviewSource::AutomaticConversationRoute => {
            "automatic_conversation_route"
        }
    }
}

fn plan_role_label(value: sigil_desktop::DesktopPlanAgentRole) -> &'static str {
    match value {
        sigil_desktop::DesktopPlanAgentRole::Planner => "planner",
        sigil_desktop::DesktopPlanAgentRole::Executor => "executor",
        sigil_desktop::DesktopPlanAgentRole::SubagentRead => "subagent_read",
        sigil_desktop::DesktopPlanAgentRole::SubagentWrite => "subagent_write",
    }
}

fn plan_mode_label(value: sigil_desktop::DesktopPlanStepMode) -> &'static str {
    match value {
        sigil_desktop::DesktopPlanStepMode::Read => "read",
        sigil_desktop::DesktopPlanStepMode::Write => "write",
        sigil_desktop::DesktopPlanStepMode::Review => "review",
        sigil_desktop::DesktopPlanStepMode::Verify => "verify",
    }
}

fn plan_isolation_label(value: sigil_desktop::DesktopPlanIsolationMode) -> &'static str {
    match value {
        sigil_desktop::DesktopPlanIsolationMode::SharedReadOnly => "shared_read_only",
        sigil_desktop::DesktopPlanIsolationMode::SequentialWorkspaceWrite => {
            "sequential_workspace_write"
        }
        sigil_desktop::DesktopPlanIsolationMode::ChangesetOnly => "changeset_only",
        sigil_desktop::DesktopPlanIsolationMode::Worktree => "worktree",
    }
}

fn plan_effect_label(value: sigil_desktop::DesktopPlanCheckEffect) -> &'static str {
    match value {
        sigil_desktop::DesktopPlanCheckEffect::ReadOnly => "read_only",
        sigil_desktop::DesktopPlanCheckEffect::WorkspaceWrite => "workspace_write",
        sigil_desktop::DesktopPlanCheckEffect::ExternalWrite => "external_write",
        sigil_desktop::DesktopPlanCheckEffect::Network => "network",
        sigil_desktop::DesktopPlanCheckEffect::Unknown => "unknown",
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopUserInputReadInput {
    pub(crate) session_id: String,
    pub(crate) request_id: String,
    pub(crate) generation: u32,
    pub(crate) expected_request_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopUserInputDecisionInput {
    pub(crate) session_id: String,
    pub(crate) request_id: String,
    pub(crate) generation: u32,
    pub(crate) expected_request_hash: String,
    pub(crate) decision: DesktopUserInputDecisionInputKind,
    pub(crate) permission_mode: Option<sigil_desktop::DesktopPermissionMode>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DesktopUserInputDecisionInputKind {
    Submitted {
        answers: Vec<DesktopUserInputAnswerInput>,
    },
    Declined,
    RunCancelled,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopUserInputAnswerInput {
    pub(crate) question_id: String,
    pub(crate) value: DesktopUserInputAnswerValueInput,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum DesktopUserInputAnswerValueInput {
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
        option_id: Option<String>,
        other: Option<String>,
    },
    MultiSelect {
        option_ids: Vec<String>,
    },
}

impl DesktopUserInputDecisionInputKind {
    pub(crate) fn into_native(self) -> sigil_desktop::DesktopUserInputDecision {
        match self {
            Self::Submitted { answers } => sigil_desktop::DesktopUserInputDecision::Submitted {
                answers: answers.into_iter().map(Into::into).collect(),
            },
            Self::Declined => sigil_desktop::DesktopUserInputDecision::Declined,
            Self::RunCancelled => sigil_desktop::DesktopUserInputDecision::RunCancelled,
        }
    }
}

impl From<DesktopUserInputAnswerInput> for sigil_desktop::DesktopUserInputAnswer {
    fn from(value: DesktopUserInputAnswerInput) -> Self {
        Self {
            question_id: value.question_id,
            value: match value.value {
                DesktopUserInputAnswerValueInput::Text { value } => {
                    sigil_desktop::DesktopUserInputAnswerValue::Text { value }
                }
                DesktopUserInputAnswerValueInput::Number { value } => {
                    sigil_desktop::DesktopUserInputAnswerValue::Number { value }
                }
                DesktopUserInputAnswerValueInput::Integer { value } => {
                    sigil_desktop::DesktopUserInputAnswerValue::Integer { value }
                }
                DesktopUserInputAnswerValueInput::Boolean { value } => {
                    sigil_desktop::DesktopUserInputAnswerValue::Boolean { value }
                }
                DesktopUserInputAnswerValueInput::SingleSelect { option_id, other } => {
                    sigil_desktop::DesktopUserInputAnswerValue::SingleSelect { option_id, other }
                }
                DesktopUserInputAnswerValueInput::MultiSelect { option_ids } => {
                    sigil_desktop::DesktopUserInputAnswerValue::MultiSelect { option_ids }
                }
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputRequestSummary {
    pub(crate) identity: DesktopUserInputIdentitySummary,
    pub(crate) request_hash: String,
    pub(crate) source: DesktopUserInputSourceSummary,
    pub(crate) purpose: &'static str,
    pub(crate) prompt: String,
    pub(crate) questions: Vec<DesktopUserInputQuestionSummary>,
    pub(crate) allowed_actions: Vec<&'static str>,
    pub(crate) requested_at_unix_ms: u64,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) answer_receipt: Option<DesktopUserInputAnswerReceiptSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolution: Option<DesktopUserInputResolutionSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputIdentitySummary {
    pub(crate) session_scope_id: String,
    pub(crate) root_logical_run_id: String,
    pub(crate) source_thread_id: String,
    pub(crate) request_id: String,
    pub(crate) generation: u32,
    pub(crate) source_binding_hash: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputSourceSummary {
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) base_plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) base_plan_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plan_review_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) call_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputQuestionSummary {
    pub(crate) id: String,
    pub(crate) header: String,
    pub(crate) question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    pub(crate) required: bool,
    pub(crate) field: DesktopUserInputFieldSummary,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum DesktopUserInputFieldSummary {
    Text {
        multiline: bool,
        max_chars: u32,
    },
    Number,
    Integer,
    Boolean,
    SingleSelect {
        options: Vec<DesktopUserInputOptionSummary>,
        allow_other: bool,
    },
    MultiSelect {
        options: Vec<DesktopUserInputOptionSummary>,
        max_selected: u32,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputOptionSummary {
    pub(crate) id: String,
    pub(crate) label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputAnswerReceiptSummary {
    pub(crate) command_id: String,
    pub(crate) decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) answer_hash: Option<String>,
    pub(crate) answered_question_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputResolutionSummary {
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failure_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) retryable: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUserInputDecisionSummary {
    pub(crate) command_id: String,
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) request: DesktopUserInputRequestSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) continuation_run_id: Option<String>,
    pub(crate) replayed: bool,
}

impl From<sigil_desktop::DesktopUserInputRequest> for DesktopUserInputRequestSummary {
    fn from(value: sigil_desktop::DesktopUserInputRequest) -> Self {
        Self {
            identity: DesktopUserInputIdentitySummary {
                session_scope_id: value.identity.session_scope_id,
                root_logical_run_id: value.identity.root_logical_run_id,
                source_thread_id: value.identity.source_thread_id,
                request_id: value.identity.request_id,
                generation: value.identity.generation,
                source_binding_hash: value.identity.source_binding_hash,
            },
            request_hash: value.request_hash,
            source: user_input_source(value.source),
            purpose: user_input_purpose_label(value.purpose),
            prompt: value.prompt,
            questions: value.questions.into_iter().map(Into::into).collect(),
            allowed_actions: value
                .allowed_actions
                .into_iter()
                .map(user_input_action_label)
                .collect(),
            requested_at_unix_ms: value.requested_at_unix_ms,
            status: user_input_status_label(value.status),
            answer_receipt: value.answer_receipt.map(|receipt| {
                DesktopUserInputAnswerReceiptSummary {
                    command_id: receipt.command_id,
                    decision: user_input_decision_kind_label(receipt.decision),
                    answer_hash: receipt.answer_hash,
                    answered_question_ids: receipt.answered_question_ids,
                }
            }),
            resolution: value.resolution.map(user_input_resolution),
        }
    }
}

impl From<sigil_desktop::DesktopUserInputQuestion> for DesktopUserInputQuestionSummary {
    fn from(value: sigil_desktop::DesktopUserInputQuestion) -> Self {
        Self {
            id: value.id,
            header: value.header,
            question: value.question,
            description: value.description,
            required: value.required,
            field: match value.field {
                sigil_desktop::DesktopUserInputField::Text {
                    multiline,
                    max_chars,
                } => DesktopUserInputFieldSummary::Text {
                    multiline,
                    max_chars,
                },
                sigil_desktop::DesktopUserInputField::Number => {
                    DesktopUserInputFieldSummary::Number
                }
                sigil_desktop::DesktopUserInputField::Integer => {
                    DesktopUserInputFieldSummary::Integer
                }
                sigil_desktop::DesktopUserInputField::Boolean => {
                    DesktopUserInputFieldSummary::Boolean
                }
                sigil_desktop::DesktopUserInputField::SingleSelect {
                    options,
                    allow_other,
                } => DesktopUserInputFieldSummary::SingleSelect {
                    options: options.into_iter().map(Into::into).collect(),
                    allow_other,
                },
                sigil_desktop::DesktopUserInputField::MultiSelect {
                    options,
                    max_selected,
                } => DesktopUserInputFieldSummary::MultiSelect {
                    options: options.into_iter().map(Into::into).collect(),
                    max_selected,
                },
            },
        }
    }
}

impl From<sigil_desktop::DesktopUserInputOption> for DesktopUserInputOptionSummary {
    fn from(value: sigil_desktop::DesktopUserInputOption) -> Self {
        Self {
            id: value.id,
            label: value.label,
            description: value.description,
        }
    }
}

fn user_input_source(
    value: sigil_desktop::DesktopUserInputSource,
) -> DesktopUserInputSourceSummary {
    let mut summary = DesktopUserInputSourceSummary {
        kind: "agent",
        base_plan_id: None,
        base_plan_hash: None,
        plan_review_id: None,
        attempt_id: None,
        task_id: None,
        server_id: None,
        call_id: None,
    };
    match value {
        sigil_desktop::DesktopUserInputSource::Agent => {}
        sigil_desktop::DesktopUserInputSource::PlanReviewResearch {
            plan_review_id,
            attempt_id,
        } => {
            summary.kind = "plan_review_research";
            summary.plan_review_id = Some(plan_review_id);
            summary.attempt_id = Some(attempt_id);
        }
        sigil_desktop::DesktopUserInputSource::PlanRevision {
            base_plan_id,
            base_plan_hash,
        } => {
            summary.kind = "plan_revision";
            summary.base_plan_id = Some(base_plan_id);
            summary.base_plan_hash = Some(base_plan_hash);
        }
        sigil_desktop::DesktopUserInputSource::Planner { task_id } => {
            summary.kind = "planner";
            summary.task_id = Some(task_id);
        }
        sigil_desktop::DesktopUserInputSource::Mcp { server_id, call_id } => {
            summary.kind = "mcp";
            summary.server_id = Some(server_id);
            summary.call_id = Some(call_id);
        }
    }
    summary
}

fn user_input_purpose_label(value: sigil_desktop::DesktopUserInputPurpose) -> &'static str {
    match value {
        sigil_desktop::DesktopUserInputPurpose::Clarification => "clarification",
        sigil_desktop::DesktopUserInputPurpose::Choice => "choice",
        sigil_desktop::DesktopUserInputPurpose::MissingConstraint => "missing_constraint",
        sigil_desktop::DesktopUserInputPurpose::RevisionGuidance => "revision_guidance",
        sigil_desktop::DesktopUserInputPurpose::ExternalElicitation => "external_elicitation",
    }
}

fn user_input_action_label(value: sigil_desktop::DesktopUserInputAction) -> &'static str {
    match value {
        sigil_desktop::DesktopUserInputAction::Submit => "submit",
        sigil_desktop::DesktopUserInputAction::Decline => "decline",
        sigil_desktop::DesktopUserInputAction::CancelRun => "cancel_run",
    }
}

fn user_input_status_label(value: sigil_desktop::DesktopUserInputStatus) -> &'static str {
    match value {
        sigil_desktop::DesktopUserInputStatus::Requested => "requested",
        sigil_desktop::DesktopUserInputStatus::DecisionAccepted => "decision_accepted",
        sigil_desktop::DesktopUserInputStatus::ContinuationClaimed => "continuation_claimed",
        sigil_desktop::DesktopUserInputStatus::ContinuationStarted => "continuation_started",
        sigil_desktop::DesktopUserInputStatus::Resolved => "resolved",
    }
}

fn user_input_decision_kind_label(
    value: sigil_desktop::DesktopUserInputDecisionKind,
) -> &'static str {
    match value {
        sigil_desktop::DesktopUserInputDecisionKind::Submitted => "submitted",
        sigil_desktop::DesktopUserInputDecisionKind::Declined => "declined",
        sigil_desktop::DesktopUserInputDecisionKind::RunCancelled => "run_cancelled",
    }
}

fn user_input_resolution(
    value: sigil_desktop::DesktopUserInputResolution,
) -> DesktopUserInputResolutionSummary {
    match value {
        sigil_desktop::DesktopUserInputResolution::Consumed => DesktopUserInputResolutionSummary {
            kind: "consumed",
            failure_class: None,
            retryable: None,
        },
        sigil_desktop::DesktopUserInputResolution::Declined => DesktopUserInputResolutionSummary {
            kind: "declined",
            failure_class: None,
            retryable: None,
        },
        sigil_desktop::DesktopUserInputResolution::RunCancelled => {
            DesktopUserInputResolutionSummary {
                kind: "run_cancelled",
                failure_class: None,
                retryable: None,
            }
        }
        sigil_desktop::DesktopUserInputResolution::Failed {
            failure_class,
            retryable,
        } => DesktopUserInputResolutionSummary {
            kind: "failed",
            failure_class: Some(failure_class),
            retryable: Some(retryable),
        },
    }
}
