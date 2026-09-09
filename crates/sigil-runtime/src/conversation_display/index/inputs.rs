use super::*;

#[derive(Debug, Default)]
pub(super) struct InputMetadataProjection {
    direct: BTreeMap<sigil_kernel::UserInputIdentityV1, InputSnapshot>,
    requested: BTreeMap<sigil_kernel::UserInputIdentityV1, ConversationDisplayRecordPosition>,
    routes: sigil_kernel::AgentUserInputRouteProjectionV1,
    route_sources: BTreeMap<sigil_kernel::AgentRouteId, ConversationDisplayRecordPosition>,
    plans: BTreeMap<
        (
            sigil_kernel::PlanReviewId,
            sigil_kernel::PlanReviewAttemptId,
        ),
        InputSnapshot,
    >,
    versions: Vec<(u64, Vec<InputSnapshot>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InputSnapshot {
    position: ConversationDisplayRecordPosition,
    public: sigil_kernel::PublicUserInputRequestV1,
    direct: bool,
}

impl InputMetadataProjection {
    pub(super) fn apply(
        &mut self,
        entry: &SessionLogEntry,
        position: &ConversationDisplayRecordPosition,
        public: Option<sigil_kernel::PublicUserInputRequestV1>,
    ) -> Result<()> {
        if let SessionLogEntry::Control(ControlEntry::UserInputRequested(requested)) = entry {
            self.requested
                .insert(requested.request.identity.clone(), position.clone());
        }
        if let Some(public) = public {
            if public.status == sigil_kernel::UserInputStatusV1::Resolved {
                self.direct.remove(&public.identity);
            } else {
                let source = self
                    .requested
                    .get(&public.identity)
                    .context("public input has no durable requested source")?
                    .clone();
                self.direct.insert(
                    public.identity.clone(),
                    InputSnapshot {
                        position: source,
                        public: compact(public),
                        direct: true,
                    },
                );
            }
        }
        match entry {
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)) => {
                let key = (attempt.plan_review_id.clone(), attempt.attempt_id.clone());
                // Replace only this attempt's previous mirror, including a missing question.
                // Other attempts may still own independent pending requests.
                self.plans.remove(&key);
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::WaitingForInput
                    && let Some(public) = attempt.pending_user_input.as_deref()
                {
                    self.plans.insert(
                        key,
                        InputSnapshot {
                            position: position.clone(),
                            public: compact(public.clone()),
                            direct: false,
                        },
                    );
                }
            }
            SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route)) => {
                let mut compact = route.clone();
                compact.request.prompt =
                    format!("{:x}", Sha256::digest(serde_json::to_vec(&route.request)?));
                compact.request.questions.clear();
                compact.request.allowed_actions.clear();
                compact.request.answer_receipt = None;
                compact.request.resolution = None;
                self.routes.apply(compact)?;
                self.route_sources
                    .insert(route.route_id.clone(), position.clone());
            }
            _ => {}
        }
        let mut candidates = self
            .direct
            .values()
            .cloned()
            .chain(self.plans.values().cloned())
            .collect::<Vec<_>>();
        for route in self.routes.unresolved() {
            let position = self
                .route_sources
                .get(&route.route_id)
                .context("agent input lost its route source")?
                .clone();
            candidates.push(InputSnapshot {
                position,
                public: compact(route.request.clone()),
                direct: false,
            });
        }
        let mut by_identity = BTreeMap::new();
        for candidate in candidates {
            by_identity.insert(
                (
                    candidate.public.identity.clone(),
                    candidate.public.request_hash.clone(),
                ),
                candidate,
            );
        }
        let selected = stable_pending_user_inputs(
            by_identity
                .values()
                .map(|candidate| candidate.public.clone()),
        );
        let snapshot = selected
            .into_iter()
            .filter_map(|public| by_identity.remove(&(public.identity, public.request_hash)))
            .collect();
        push_version(&mut self.versions, position.sequence, snapshot);
        Ok(())
    }

    pub(super) fn at(&self, sequence: u64) -> Vec<InputSnapshot> {
        version_at(&self.versions, sequence)
            .cloned()
            .unwrap_or_default()
    }
}

fn compact(
    mut public: sigil_kernel::PublicUserInputRequestV1,
) -> sigil_kernel::PublicUserInputRequestV1 {
    public.prompt.clear();
    public.questions.clear();
    public.resolution = None;
    public
}

impl InputSnapshot {
    pub(super) fn position(&self) -> &ConversationDisplayRecordPosition {
        &self.position
    }

    pub(super) fn hydrate(
        self,
        records: &BodyRecords<'_>,
    ) -> Result<sigil_kernel::PublicUserInputRequestV1> {
        let surface = records.surface(&self.position)?;
        let SurfaceBody::Input(source) = surface.as_ref() else {
            bail!("conversation input source changed type");
        };
        if self.public.identity != source.identity
            || self.public.request_hash != source.request_hash
        {
            bail!("conversation input source changed identity");
        }
        if self.direct {
            let mut public = self.public;
            public.prompt = source.prompt.clone();
            public.questions = source.questions.clone();
            Ok(public)
        } else {
            Ok(source.as_ref().clone())
        }
    }
}
