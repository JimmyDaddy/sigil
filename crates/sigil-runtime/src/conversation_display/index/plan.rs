use super::*;

#[derive(Debug, Default)]
pub(super) struct PlanMetadataProjection {
    projection: PlanReviewDisplayProjection,
    drafts: BTreeMap<String, ConversationDisplayRecordPosition>,
    candidates: BTreeMap<String, ConversationDisplayRecordPosition>,
    versions: Vec<(u64, Option<PlanSnapshot>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanSnapshot {
    public: sigil_kernel::PublicPlanReview,
    draft: Option<ConversationDisplayRecordPosition>,
    candidate: Option<ConversationDisplayRecordPosition>,
    workspace_snapshot_id: Option<String>,
}

impl PlanMetadataProjection {
    pub(super) fn apply(
        &mut self,
        entry: &SessionLogEntry,
        position: &ConversationDisplayRecordPosition,
    ) -> Result<()> {
        let SessionLogEntry::Control(control) = entry else {
            return Ok(());
        };
        let control = match control {
            ControlEntry::PlanReviewAttempt(attempt) => {
                let mut compact = attempt.clone();
                compact.explicit_objective = None;
                compact.pending_user_input = None;
                ControlEntry::PlanReviewAttempt(compact)
            }
            ControlEntry::PlanDraftCreated(draft) => {
                self.drafts
                    .insert(draft.plan_id.as_str().to_owned(), position.clone());
                let mut compact = draft.clone();
                compact.summary.clear();
                compact.inline_text = None;
                compact.steps.clear();
                compact.intent_proposal = None;
                compact.target_paths.clear();
                compact.suggested_checks.clear();
                compact.risk = None;
                compact.notes.clear();
                ControlEntry::PlanDraftCreated(compact)
            }
            ControlEntry::PlanReviewCandidateRecordedV1(candidate) => {
                self.candidates
                    .insert(candidate.plan_id.as_str().to_owned(), position.clone());
                let mut compact = candidate.clone();
                compact.content.clear();
                compact.content_artifact = None;
                ControlEntry::PlanReviewCandidateRecordedV1(compact)
            }
            ControlEntry::PlanDecisionRecorded(decision) => {
                let mut compact = decision.clone();
                compact.reason = None;
                ControlEntry::PlanDecisionRecorded(compact)
            }
            ControlEntry::TaskMaterializationBlockedV1(blocked) => {
                let mut compact = blocked.clone();
                compact.blocker.summary.clear();
                ControlEntry::TaskMaterializationBlockedV1(compact)
            }
            ControlEntry::UserInputRequested(requested) => {
                let sigil_kernel::UserInputSourceV1::PlanRevision { base_plan_id, .. } =
                    &requested.request.source
                else {
                    return Ok(());
                };
                self.projection
                    .revision_guidance
                    .insert(requested.request.identity.clone(), base_plan_id.clone());
                self.projection
                    .pending_revision_guidance
                    .insert(base_plan_id.clone());
                self.projection.latest_revision_request.insert(
                    base_plan_id.clone(),
                    requested.request.identity.request_id.clone(),
                );
                self.record_version(position.sequence);
                return Ok(());
            }
            ControlEntry::UserInputResolved(resolved) => {
                let mut compact = resolved.clone();
                if matches!(
                    compact.resolution,
                    sigil_kernel::UserInputResolutionV1::Failed { .. }
                ) {
                    compact.resolution = sigil_kernel::UserInputResolutionV1::Failed {
                        failure_class: String::new(),
                        retryable: false,
                    };
                }
                ControlEntry::UserInputResolved(compact)
            }
            ControlEntry::TaskCreatedFromPlan(_)
            | ControlEntry::TaskMaterializationPreparedV1(_)
            | ControlEntry::PlanExecutionAdoptedV1(_) => control.clone(),
            _ => return Ok(()),
        };
        self.projection
            .apply_entry(SessionLogEntry::Control(control))?;
        self.record_version(position.sequence);
        Ok(())
    }

    fn record_version(&mut self, sequence: u64) {
        let snapshot = self.projection.clone().into_public(None).map(|public| {
            let draft = self.drafts.get(&public.plan_id).cloned();
            let candidate = public
                .candidate
                .as_ref()
                .and_then(|_| self.candidates.get(&public.plan_id))
                .cloned();
            let workspace_snapshot_id = self
                .projection
                .drafts
                .values()
                .find(|draft| draft.plan_id.as_str() == public.plan_id)
                .and_then(|draft| draft.workspace_snapshot_id.clone());
            PlanSnapshot {
                public,
                draft,
                candidate,
                workspace_snapshot_id,
            }
        });
        push_version(&mut self.versions, sequence, snapshot);
    }

    pub(super) fn at(&self, sequence: u64) -> Option<PlanSnapshot> {
        version_at(&self.versions, sequence).cloned().flatten()
    }
}

impl PlanSnapshot {
    pub(super) fn positions(&self) -> impl Iterator<Item = &ConversationDisplayRecordPosition> {
        self.draft.iter().chain(self.candidate.iter())
    }

    pub(super) fn hydrate(
        mut self,
        records: &BodyRecords<'_>,
        workspace: Option<&str>,
    ) -> Result<sigil_kernel::PublicPlanReview> {
        self.public.stale = self.draft.is_some()
            && crate::plan_review_coordinator::plan_handoff_stale_reason(
                self.workspace_snapshot_id.as_deref(),
                workspace,
            )
            .is_some();
        if let Some(position) = self.draft {
            let surface = records.surface(&position)?;
            let SurfaceBody::PlanDraft {
                plan_id,
                plan_hash,
                summary,
                summary_truncated,
                step_count,
                target_path_count,
                suggested_check_count,
                risk,
            } = surface.as_ref()
            else {
                bail!("conversation Plan summary lost its draft source");
            };
            if plan_id != &self.public.plan_id
                || self.public.plan_hash.as_deref() != Some(plan_hash)
            {
                bail!("conversation Plan summary source changed identity");
            }
            self.public.summary = Some(summary.clone());
            self.public.summary_truncated = *summary_truncated;
            self.public.step_count = Some(*step_count);
            self.public.target_path_count = Some(*target_path_count);
            self.public.suggested_check_count = Some(*suggested_check_count);
            self.public.risk = risk.clone();
        }
        if let Some(position) = self.candidate {
            let surface = records.surface(&position)?;
            let SurfaceBody::PlanCandidate {
                plan_id,
                content_hash,
                content,
            } = surface.as_ref()
            else {
                bail!("conversation Plan candidate lost its source");
            };
            let public = self
                .public
                .candidate
                .as_mut()
                .context("conversation Plan candidate is no longer visible")?;
            if plan_id != &self.public.plan_id || content_hash != &public.content_hash {
                bail!("conversation Plan candidate source changed identity");
            }
            public.content = content.clone();
        }
        Ok(self.public)
    }
}
