use std::sync::Arc;

use super::*;

/// A bounded projection of a source payload, never the source record or its hidden material.
#[derive(Debug, Serialize)]
pub(super) enum SurfaceBody {
    TaskTitles {
        task_id: String,
        titles: BTreeMap<String, String>,
    },
    TaskChecklist {
        task_id: String,
        items: Vec<sigil_kernel::PublicTaskChecklistItemV1>,
    },
    PlanDraft {
        plan_id: String,
        plan_hash: String,
        summary: String,
        summary_truncated: bool,
        step_count: usize,
        target_path_count: usize,
        suggested_check_count: usize,
        risk: Option<String>,
    },
    PlanCandidate {
        plan_id: String,
        content_hash: String,
        content: String,
    },
    Input(Box<sigil_kernel::PublicUserInputRequestV1>),
}

impl SurfaceBody {
    pub(super) fn from_entry(entry: &SessionLogEntry) -> Option<Self> {
        match entry {
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan)) => Some(Self::TaskTitles {
                task_id: plan.task_id.as_str().to_owned(),
                titles: plan
                    .steps
                    .iter()
                    .take(MAX_CONVERSATION_TASK_CONTROL_ITEMS)
                    .map(|step| {
                        (
                            step.step_id.as_str().to_owned(),
                            truncate_utf8(&step.title, MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES).0,
                        )
                    })
                    .collect(),
            }),
            SessionLogEntry::Control(ControlEntry::TaskChecklistUpdatedV1(checklist)) => {
                Some(Self::TaskChecklist {
                    task_id: checklist.task_id.as_str().to_owned(),
                    items: checklist
                        .items
                        .iter()
                        .map(sigil_kernel::PublicTaskChecklistItemV1::from)
                        .collect(),
                })
            }
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)) => {
                let (summary, summary_truncated) = compact_plan_review_summary(&draft.summary);
                Some(Self::PlanDraft {
                    plan_id: draft.plan_id.as_str().to_owned(),
                    plan_hash: draft.plan_hash.clone(),
                    summary,
                    summary_truncated,
                    step_count: draft.steps.len(),
                    target_path_count: draft.target_paths.len(),
                    suggested_check_count: draft.suggested_checks.len(),
                    risk: draft.risk.clone(),
                })
            }
            SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(candidate)) => {
                Some(Self::PlanCandidate {
                    plan_id: candidate.plan_id.as_str().to_owned(),
                    content_hash: candidate.content_hash.clone(),
                    content: candidate.content.clone(),
                })
            }
            SessionLogEntry::Control(ControlEntry::UserInputRequested(requested)) => {
                let request = &requested.request;
                Some(Self::Input(Box::new(
                    sigil_kernel::PublicUserInputRequestV1 {
                        identity: request.identity.clone(),
                        request_hash: requested.request_hash.clone(),
                        source: request.source.clone(),
                        purpose: request.purpose,
                        prompt: request.prompt.clone(),
                        questions: request.questions.clone(),
                        allowed_actions: request.allowed_actions.clone(),
                        requested_at_unix_ms: request.requested_at_unix_ms,
                        status: sigil_kernel::UserInputStatusV1::Requested,
                        answer_receipt: None,
                        resolution: None,
                    },
                )))
            }
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::WaitingForInput =>
            {
                attempt.pending_user_input.clone().map(Self::Input)
            }
            SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route)) => {
                Some(Self::Input(Box::new(route.request.clone())))
            }
            _ => None,
        }
    }
}

/// A fixed-size hot surface cache. Historical index versions contain only source positions.
#[derive(Debug, Default)]
pub(super) struct SurfaceCache {
    bodies: BTreeMap<u64, (usize, Arc<SurfaceBody>)>,
    bytes: usize,
}

impl SurfaceCache {
    pub(super) fn apply(&mut self, entry: &SessionLogEntry, sequence: u64) -> Result<()> {
        let Some(body) = SurfaceBody::from_entry(entry) else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&body)?.len();
        if bytes > MAX_CONVERSATION_DISPLAY_RESPONSE_BYTES {
            return Ok(());
        }
        self.bytes += bytes;
        self.bodies.insert(sequence, (bytes, Arc::new(body)));
        while self.bytes > MAX_CONVERSATION_DISPLAY_RESPONSE_BYTES {
            if let Some((_, (bytes, _))) = self.bodies.pop_first() {
                self.bytes -= bytes;
            }
        }
        Ok(())
    }

    pub(super) fn get(&self, sequence: u64) -> Option<Arc<SurfaceBody>> {
        self.bodies.get(&sequence).map(|(_, body)| Arc::clone(body))
    }
}

/// The newest bounded transcript window is retained; older row metadata never owns a body.
#[derive(Debug, Default)]
pub(super) struct RowCache {
    rows: BTreeMap<ConversationDisplayOrderV1, (usize, Arc<ConversationDisplayItemV1>)>,
    bytes: usize,
}

impl RowCache {
    pub(super) fn apply(&mut self, item: ConversationDisplayItemV1, bytes: usize) {
        if bytes > MAX_CONVERSATION_DISPLAY_PAGE_BYTES {
            return;
        }
        self.bytes += bytes;
        self.rows
            .insert(item.display_order, (bytes, Arc::new(item)));
        while self.bytes > MAX_CONVERSATION_DISPLAY_PAGE_BYTES
            || self.rows.len() > MAX_CONVERSATION_DISPLAY_PAGE_SIZE
        {
            if let Some((_, (bytes, _))) = self.rows.pop_first() {
                self.bytes -= bytes;
            }
        }
    }

    pub(super) fn get(
        &self,
        order: ConversationDisplayOrderV1,
    ) -> Option<Arc<ConversationDisplayItemV1>> {
        self.rows.get(&order).map(|(_, item)| Arc::clone(item))
    }
}
