use super::*;

struct MixedSubmissionProvider {
    calls: Arc<AtomicUsize>,
    valid_first: bool,
    invalid_draft: bool,
}

#[async_trait]
impl Provider for MixedSubmissionProvider {
    fn name(&self) -> &str {
        "mixed-plan-review"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        plan_review_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            request
                .tools
                .iter()
                .any(|tool| tool.name == sigil_kernel::PLAN_REVIEW_RESULT_TOOL_NAME)
        );
        if ordinal > 0 {
            return Ok(Box::pin(stream::iter(submitted_draft_chunks(
                "redundant-draft",
            ))));
        }

        let mut accepted = submitted_draft_chunks("accepted-draft");
        accepted.pop();
        let mut rejected = if self.invalid_draft {
            let mut chunks = invalid_draft_chunks("rejected-call");
            chunks.pop();
            chunks
        } else {
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "rejected-call".to_owned(),
                    name: "unavailable_inspection".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "rejected-call".to_owned(),
                    delta: "{}".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "rejected-call".to_owned(),
                    name: "unavailable_inspection".to_owned(),
                    args_json: "{}".to_owned(),
                })),
            ]
        };
        let mut chunks = if self.valid_first {
            accepted.append(&mut rejected);
            accepted
        } else {
            rejected.append(&mut accepted);
            rejected
        };
        chunks.push(Ok(ProviderChunk::Done));
        Ok(Box::pin(stream::iter(chunks)))
    }
}

struct CountSubmissionInspections(Arc<AtomicUsize>);

#[async_trait]
impl Tool for CountSubmissionInspections {
    fn spec(&self) -> ToolSpec {
        PlanReviewInspectionTool.spec()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        call_id: String,
        args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        PlanReviewInspectionTool.execute(ctx, call_id, args).await
    }
}

#[tokio::test]
async fn model_submitted_mixed_batch_preserves_draft_without_dispatching_unavailable_calls()
-> Result<()> {
    for valid_first in [false, true] {
        for invalid_draft in [false, true] {
            let temp = tempfile::tempdir()?;
            let parent_path = temp.path().join("sessions/parent.jsonl");
            let (mut parent, request) = seed_route_decision(
                Session::new("plan-review-test", "planned-model")
                    .with_store(JsonlSessionStore::new(&parent_path)?),
            )?;
            let provider_calls = Arc::new(AtomicUsize::new(0));
            let inspections = Arc::new(AtomicUsize::new(0));
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(CountSubmissionInspections(Arc::clone(
                &inspections,
            ))));
            let agent = Agent::new(
                MixedSubmissionProvider {
                    calls: Arc::clone(&provider_calls),
                    valid_first,
                    invalid_draft,
                },
                registry.clone(),
            );
            let cancellation = RunCancellationOwner::new();
            let outcome = PlanReviewCoordinator::run_plan_review(
                &mut parent,
                &request,
                &agent,
                plan_review_test_options(temp.path()),
                registry,
                &mut NoopEventHandler,
                &mut AutoApproveHandler,
                cancellation.handle(),
            )
            .await?;
            let PlanReviewRunOutcome::DraftReady { draft } = outcome else {
                bail!("mixed model tool batch must retain its committed draft");
            };
            assert_eq!(
                provider_calls.load(Ordering::SeqCst),
                1,
                "a committed draft ends the ordinary review loop"
            );
            assert_eq!(
                inspections.load(Ordering::SeqCst),
                0,
                "unavailable research calls must never execute"
            );
            assert!(cancellation.handle().is_naturally_finalized());
            let child = Session::load_from_store(
                "plan-review-test",
                "planned-model",
                JsonlSessionStore::new(
                    request
                        .child_session_ref
                        .resolve(parent_path.parent().expect("durable parent directory")),
                )?,
            )?;
            assert_eq!(
                child
                    .entries()
                    .iter()
                    .filter(|entry| matches!(
                        entry,
                        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(_))
                    ))
                    .count(),
                1
            );
            assert!(child.entries().iter().any(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.call_id == "rejected-call"
                        && matches!(execution.status,
                            sigil_kernel::ToolExecutionStatus::Failed | sigil_kernel::ToolExecutionStatus::Cancelled))));
            if !invalid_draft {
                assert!(!child.entries().iter().any(|entry| matches!(entry,
                    SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                        if execution.call_id == "rejected-call"
                            && execution.status == sigil_kernel::ToolExecutionStatus::Started)));
            }
            PlanReviewCoordinator::commit_draft_from_child(
                &mut parent,
                &draft,
                &request,
                &mut NoopEventHandler,
                99,
            )?;
            let restored = Session::load_from_store(
                "plan-review-test",
                "planned-model",
                JsonlSessionStore::new(parent_path)?,
            )?;
            assert_eq!(
                restored
                    .plan_artifact_projection()
                    .plans
                    .get(&request.plan_id),
                Some(draft.as_ref())
            );
            assert_eq!(
                PlanReviewProjection::from_entries(restored.entries())
                    .latest_attempt(&request.plan_review_id)
                    .map(|attempt| attempt.status),
                Some(PlanReviewAttemptStatus::DraftReady)
            );
        }
    }
    Ok(())
}
