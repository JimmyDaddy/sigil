//! Delegation follows immutable parent domain facts and opens only that existing child.
use super::*;
use crate::{AgentUserInputRouteProjectionV1, ApplicationOperationTargetV1, SessionRef};

impl SessionApplicationOperationOwner {
    /// Resolves the exact durable user-input owner. The caller receives an existing capability,
    /// never authority synthesized from an adapter-supplied path or session id.
    pub fn resolve_target(&self, target: &ApplicationOperationTargetV1) -> Result<Self> {
        let Some((child_ref, scope)) = self.routed_target(target)? else {
            return Ok(self.clone());
        };
        let parent = self
            .store
            .path()
            .parent()
            .context("session has no parent directory")?;
        let store = JsonlSessionStore::open_existing(child_ref.resolve(parent))?;
        validate_record_scope(&store.read_event_records_writer()?, &scope)?;
        Ok(Self {
            store,
            session_scope_id: scope,
            authority_scope_id: self.authority_scope_id.clone(),
        })
    }

    /// Resolves only an observation handle; querying an old operation never takes child tail
    /// recovery ownership. Missing or corrupt existing sources remain unavailable.
    pub fn observe_target(
        &self,
        target: &ApplicationOperationTargetV1,
    ) -> Result<(String, SessionRecordReadHandle)> {
        let Some((child_ref, scope)) = self.routed_target(target)? else {
            return Ok((self.session_scope_id.clone(), self.read_handle()));
        };
        let parent = self
            .store
            .path()
            .parent()
            .context("session has no parent directory")?;
        let reader = SessionRecordReadHandle::open_existing_observer(child_ref.resolve(parent))?;
        let first = reader.read_event_record_range(0, 0, Some(&scope), 1, 4 * 1024 * 1024)?;
        if first.records().is_empty() {
            bail!("child operation observation requires its existing source");
        }
        Ok((scope, reader))
    }

    pub fn observe_operation(
        &self,
        binding: &ApplicationOperationBindingV1,
    ) -> Result<(ApplicationOperationBindingV1, SessionRecordReadHandle)> {
        binding.validate()?;
        if binding.session_scope_id != self.authority_scope_id {
            bail!("application operation changed its original authority scope");
        }
        let (scope, reader) = self.observe_target(&binding.target)?;
        if binding
            .domain_session_scope_id
            .as_ref()
            .is_some_and(|prior| prior != &scope)
        {
            bail!("application operation observation changes its domain owner");
        }
        let mut resolved = binding.clone();
        resolved.domain_session_scope_id = (scope != binding.session_scope_id).then_some(scope);
        Ok((resolved, reader))
    }

    pub fn domain_session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    pub fn resolve_operation(
        &self,
        binding: &ApplicationOperationBindingV1,
    ) -> Result<(Self, ApplicationOperationBindingV1)> {
        binding.validate()?;
        if binding.session_scope_id != self.authority_scope_id {
            bail!("application operation changed its original authority scope");
        }
        let owner = self.resolve_target(&binding.target)?;
        if binding
            .domain_session_scope_id
            .as_ref()
            .is_some_and(|scope| scope != &owner.session_scope_id)
        {
            bail!("application operation changed its domain owner");
        }
        let mut resolved = binding.clone();
        resolved.domain_session_scope_id = (owner.session_scope_id != binding.session_scope_id)
            .then(|| owner.session_scope_id.clone());
        Ok((owner, resolved))
    }

    pub fn reconcile(
        &self,
        binding: &ApplicationOperationBindingV1,
    ) -> Result<Option<ApplicationOperationCommitProofV1>> {
        let (binding, reader) = self.observe_operation(binding)?;
        reconcile_application_operation(&reader, &binding)
    }

    fn routed_target(
        &self,
        target: &ApplicationOperationTargetV1,
    ) -> Result<Option<(SessionRef, String)>> {
        let (request_id, generation, request_hash) = match target {
            ApplicationOperationTargetV1::UserInputDecision {
                request_id,
                generation,
                request_hash,
                ..
            }
            | ApplicationOperationTargetV1::UserInputContinuation {
                request_id,
                generation,
                request_hash,
                ..
            } => (request_id, generation, request_hash),
            _ => return Ok(None),
        };
        let mut routes = AgentUserInputRouteProjectionV1::default();
        let reader = self.read_handle();
        let (mut offset, mut sequence) = (0, 0);
        let mut identity = None;
        loop {
            let page = reader.read_event_record_range(
                offset,
                sequence,
                Some(&self.session_scope_id),
                128,
                4 * 1024 * 1024,
            )?;
            for record in page.records() {
                if let Some(SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route))) =
                    record.session_log_entry()?
                    && route.request.identity.request_id.as_str() == request_id
                    && route.request.identity.generation == *generation
                    && &route.request.request_hash == request_hash
                {
                    if identity
                        .as_ref()
                        .is_some_and(|prior| prior != &route.request.identity)
                    {
                        bail!("application input target has ambiguous child owners");
                    }
                    identity = Some(route.request.identity.clone());
                    routes.apply(route)?;
                }
            }
            if !page.has_more() {
                break;
            }
            offset = page.end_offset();
            sequence = page
                .records()
                .last()
                .context("domain owner scan made no progress")?
                .stream_sequence();
        }
        Ok(identity.and_then(|identity| {
            routes
                .route_for_request(&identity, request_hash)
                .map(|route| {
                    (
                        route.child_session_ref.clone(),
                        identity.session_scope_id.as_str().to_owned(),
                    )
                })
        }))
    }
}

impl Session {
    /// Carries a parent-issued operation only into its exact existing child. Ordinary child
    /// construction remains with the runtime when this Session carries no matching operation.
    pub fn application_operation_child(&self, child_ref: &SessionRef) -> Result<Option<Session>> {
        let Some(binding) = self.runtime_attachments.application_operation.as_ref() else {
            return Ok(None);
        };
        if binding.domain_session_scope_id() == self.session_scope_id() {
            return Ok(None);
        }
        let parent = self.application_operation_owner()?;
        let Some((bound_ref, _)) = parent.routed_target(&binding.target)? else {
            bail!("delegated application operation lost its child route")
        };
        if &bound_ref != child_ref {
            return Ok(None);
        }
        let (owner, binding) = parent.resolve_operation(binding)?;
        let mut child =
            Session::load_from_store(self.provider_name(), self.model_name(), owner.store)?;
        child.bind_application_operation(binding)?;
        Ok(Some(child))
    }
}

impl SessionApplicationOperationOwner {
    /// Narrows an independently supplied existing child capability after checking the parent's
    /// immutable public mirror and the child's authoritative input identity. Managed resource
    /// owners retain their allocation/lease for the lifetime of this delegated operation.
    pub fn delegate_existing_child(
        &self,
        child: &Session,
        target: &ApplicationOperationTargetV1,
    ) -> Result<Self> {
        let child_request = child
            .user_input_projection()?
            .public_requests()
            .into_iter()
            .find(|request| target_matches_input(target, request))
            .context("delegated child does not own the exact application input target")?;
        if child_request.identity.session_scope_id.as_str() != child.session_scope_id() {
            bail!("delegated child request has a different source scope");
        }
        self.validate_child_request(&child_request, target)?;
        let store = child
            .durable_store()
            .context("delegated application child must be durable")?;
        validate_record_scope(
            &store.read_event_records_writer()?,
            child.session_scope_id(),
        )?;
        Ok(Self {
            store,
            session_scope_id: child.session_scope_id().to_owned(),
            authority_scope_id: self.authority_scope_id.clone(),
        })
    }
    fn validate_child_request(
        &self,
        child_request: &crate::PublicUserInputRequestV1,
        target: &ApplicationOperationTargetV1,
    ) -> Result<()> {
        let mut routes = AgentUserInputRouteProjectionV1::default();
        let mut reviews = crate::PlanReviewProjection::default();
        let mut mirrored = false;
        let reader = self.read_handle();
        let (mut offset, mut sequence) = (0, 0);
        loop {
            let page = reader.read_event_record_range(
                offset,
                sequence,
                Some(&self.session_scope_id),
                128,
                4 * 1024 * 1024,
            )?;
            for record in page.records() {
                match record.session_log_entry()? {
                    Some(SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route))) => {
                        if target_matches_input(target, &route.request) {
                            if route.request.identity != child_request.identity {
                                bail!("delegated child changes the parent input identity");
                            }
                            mirrored = true;
                            routes.apply(route)?;
                        }
                    }
                    Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))) => {
                        if let Some(pending) = attempt
                            .pending_user_input
                            .as_deref()
                            .filter(|pending| target_matches_input(target, pending))
                        {
                            if pending.identity != child_request.identity
                                || !matches!(&pending.source,crate::UserInputSourceV1::PlanReviewResearch {plan_review_id,attempt_id} if plan_review_id==&attempt.plan_review_id && attempt_id==&attempt.attempt_id)
                            {
                                bail!(
                                    "delegated research child changes its parent attempt binding"
                                );
                            }
                            mirrored = true;
                        }
                        reviews.apply(&attempt);
                    }
                    _ => {}
                }
            }
            if !page.has_more() {
                break;
            }
            offset = page.end_offset();
            sequence = page
                .records()
                .last()
                .context("delegation scan made no progress")?
                .stream_sequence();
        }
        if !mirrored || reviews.has_conflicts() {
            bail!("application child delegation lacks a valid immutable parent mirror");
        }
        Ok(())
    }

    /// Validates a read-only resource-owner snapshot without letting its physical handle or
    /// write authority escape the resource lease.
    pub fn bind_child_records(
        &self,
        records: &[SessionStreamRecord],
        binding: &ApplicationOperationBindingV1,
    ) -> Result<ApplicationOperationBindingV1> {
        binding.validate()?;
        if binding.session_scope_id != self.authority_scope_id {
            bail!("child proof changes application authority");
        }
        let scope = records
            .first()
            .context("child proof is empty")?
            .session_id();
        validate_record_scope(records, scope)?;
        let entries = records
            .iter()
            .map(SessionStreamRecord::session_log_entry)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let request = crate::UserInputProjectionV1::from_session_entries(&entries)?
            .public_requests()
            .into_iter()
            .find(|request| target_matches_input(&binding.target, request))
            .context("child proof lacks the exact input target")?;
        if request.identity.session_scope_id.as_str() != scope {
            bail!("child input proof changes its source scope");
        }
        self.validate_child_request(&request, &binding.target)?;
        if binding
            .domain_session_scope_id
            .as_ref()
            .is_some_and(|prior| prior != scope)
        {
            bail!("child proof changes bound domain source");
        }
        let mut resolved = binding.clone();
        resolved.domain_session_scope_id =
            (scope != binding.session_scope_id).then(|| scope.to_owned());
        Ok(resolved)
    }
}

impl Session {
    pub fn application_operation_binding(&self) -> Option<&ApplicationOperationBindingV1> {
        self.runtime_attachments.application_operation.as_ref()
    }

    /// Binds a capability that the parent owner has already narrowed to this exact child.
    pub fn bind_application_operation_from_owner(
        &mut self,
        binding: ApplicationOperationBindingV1,
        owner: &SessionApplicationOperationOwner,
    ) -> Result<()> {
        if self.session_scope_id() != owner.authority_scope_id
            && self.session_scope_id() != owner.session_scope_id
        {
            bail!("application operation binding has no authority over this attachment");
        }
        let (owner, binding) = owner.resolve_operation(&binding)?;
        if !validate_prepared(&owner.store.read_event_records_writer()?, &binding)? {
            bail!("delegated operation is not prepared");
        }
        if self
            .runtime_attachments
            .application_operation
            .as_ref()
            .is_some_and(|prior| prior != &binding)
        {
            bail!("another application operation owns this transition");
        }
        self.runtime_attachments.application_operation = Some(binding);
        Ok(())
    }

    pub fn delegate_application_operation_to_child(
        &self,
        child: &mut Session,
        binding: ApplicationOperationBindingV1,
    ) -> Result<()> {
        let owner = self
            .application_operation_owner()?
            .delegate_existing_child(child, &binding.target)?;
        owner.prepare(&binding)?;
        child.bind_application_operation_from_owner(binding, &owner)
    }
}

fn target_matches_input(
    target: &ApplicationOperationTargetV1,
    request: &crate::PublicUserInputRequestV1,
) -> bool {
    match target {
        ApplicationOperationTargetV1::UserInputDecision {
            request_id,
            generation,
            request_hash,
            ..
        }
        | ApplicationOperationTargetV1::UserInputContinuation {
            request_id,
            generation,
            request_hash,
            ..
        } => {
            request.identity.request_id.as_str() == request_id
                && request.identity.generation == *generation
                && &request.request_hash == request_hash
        }
        _ => false,
    }
}
