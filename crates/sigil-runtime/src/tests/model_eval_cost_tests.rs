use super::*;

#[test]
fn model_eval_usage_missing_or_partial_price_is_unknown_not_free() {
    let mut totals = ModelEvalUsageTotals::default();
    let priced = UsageStats {
        prompt_tokens: 10,
        input_cost: 0.1,
        ..UsageStats::default()
    };
    totals.record(&priced);
    assert_eq!(totals.priced_usage_total_usd(), Some(0.1));
    let mut unknown_turn = ModelEvalUsageTotals::default();
    unknown_turn.record(&UsageStats {
        prompt_tokens: 20,
        ..UsageStats::default()
    });
    totals.merge(&unknown_turn);
    assert_eq!(totals.priced_usage_total_usd(), None);
    assert_eq!(totals.priced_usage_events, 1);
    assert_eq!(totals.prompt_tokens, 30);
}

#[test]
fn model_eval_zero_price_requires_complete_price_and_cache_evidence() {
    let snapshot = sigil_kernel::ModelPricingSnapshotV1 {
        schema_version: 1,
        snapshot_id: "zero-fixture".into(),
        currency: "USD".into(),
        unit_tokens: 1_000_000,
        cache_read_per_unit: 0.0,
        cache_write_per_unit: None,
        uncached_input_per_unit: 0.0,
        output_per_unit: 0.0,
        source: "fixture".into(),
        verified_at: "2026-09-27".into(),
    };
    let mut usage = UsageStats {
        prompt_tokens: 10,
        pricing_snapshot: Some(snapshot),
        ..UsageStats::default()
    };
    let mut incomplete = ModelEvalUsageTotals::default();
    incomplete.record(&usage);
    assert_eq!(incomplete.priced_usage_total_usd(), None);
    usage.cache_usage =
        Some(sigil_kernel::CacheUsageV1::reported_read_with_derived_uncached(10, 0));
    let mut complete = ModelEvalUsageTotals::default();
    complete.record(&usage);
    assert_eq!(complete.priced_usage_total_usd(), Some(0.0));
    usage.cache_usage.as_mut().expect("cache").write =
        Some(sigil_kernel::CacheTokenCountV1::provider_reported(1));
    let mut missing_write_price = ModelEvalUsageTotals::default();
    missing_write_price.record(&usage);
    assert_eq!(missing_write_price.priced_usage_total_usd(), None);
}

#[derive(Clone, Copy)]
enum CostPipelineMode {
    Success,
    Unpriced,
    NoConsumption,
    Failed,
    Cancel,
}

struct CostPipelineProvider(CostPipelineMode);

#[async_trait::async_trait]
impl sigil_kernel::Provider for CostPipelineProvider {
    fn name(&self) -> &str {
        "cost-fixture"
    }
    fn capabilities(&self) -> sigil_kernel::ProviderCapabilities {
        sigil_kernel::ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: true,
            reasoning_stream: sigil_kernel::ReasoningStreamSupport::Unsupported,
            supports_reasoning_effort: false,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 128,
        }
    }
    fn usage_pricing_snapshot(&self, _model: &str) -> Option<sigil_kernel::ModelPricingSnapshotV1> {
        if matches!(self.0, CostPipelineMode::Unpriced) {
            return None;
        }
        Some(sigil_kernel::ModelPricingSnapshotV1 {
            schema_version: 1,
            snapshot_id: "cost-pipeline-fixture".into(),
            currency: "USD".into(),
            unit_tokens: 100,
            cache_read_per_unit: 0.0,
            cache_write_per_unit: None,
            uncached_input_per_unit: 1.0,
            output_per_unit: 0.0,
            source: "fixture".into(),
            verified_at: "2026-09-28".into(),
        })
    }
    fn classify_pre_generation_rejection(
        &self,
        _error: &anyhow::Error,
    ) -> Option<sigil_kernel::ProviderRequestRejection> {
        matches!(self.0, CostPipelineMode::NoConsumption)
            .then_some(sigil_kernel::ProviderRequestRejection::ContextWindowExceeded)
    }
    async fn stream(
        &self,
        _request: sigil_kernel::CompletionRequest,
    ) -> Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = Result<sigil_kernel::ProviderChunk>> + Send>>,
    > {
        use futures::StreamExt;
        use sigil_kernel::ProviderChunk;
        let prefix = futures::stream::iter(vec![Ok(ProviderChunk::TextDelta("answer".into()))]);
        match self.0 {
            CostPipelineMode::Success | CostPipelineMode::Unpriced => {
                Ok(Box::pin(prefix.chain(futures::stream::iter(vec![
                    Ok(ProviderChunk::Usage(UsageStats {
                        prompt_tokens: 10,
                        completion_tokens: 2,
                        cache_usage: Some(
                            sigil_kernel::CacheUsageV1::reported_read_with_derived_uncached(10, 0),
                        ),
                        ..Default::default()
                    })),
                    Ok(ProviderChunk::Done),
                ]))))
            }
            CostPipelineMode::NoConsumption => {
                Err(anyhow::anyhow!("fixture pre-generation rejection"))
            }
            CostPipelineMode::Failed => {
                Ok(Box::pin(prefix.chain(futures::stream::iter(vec![Err(
                    anyhow::anyhow!("controlled failure after output without usage"),
                )]))))
            }
            CostPipelineMode::Cancel => Ok(Box::pin(prefix.chain(futures::stream::pending()))),
        }
    }
}

#[derive(Default)]
struct CostPipelineEvents {
    usage: ModelEvalUsageTotals,
    cancellation: Option<sigil_kernel::RunCancellationOwner>,
}
impl sigil_kernel::EventHandler for CostPipelineEvents {
    fn handle(&mut self, event: sigil_kernel::RunEvent) -> Result<()> {
        match event {
            sigil_kernel::RunEvent::Usage(usage) => self.usage.record(&usage),
            sigil_kernel::RunEvent::TextDelta(_) => {
                if let Some(owner) = &self.cancellation {
                    owner.request_cancel();
                }
            }
            _ => {}
        }
        Ok(())
    }
}

async fn cost_pipeline(
    second: CostPipelineMode,
) -> Result<(
    tempfile::TempDir,
    ModelEvalCampaignExecution,
    Vec<sigil_kernel::SessionStreamRecord>,
)> {
    use sigil_kernel::{Agent, AgentRunInput, JsonlSessionStore, Session, ToolRegistry};
    let temp = tempfile::tempdir()?;
    let fixture = super::super::load_model_eval_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../dev/evals/model-fixtures/small-code-edit"),
    )?;
    let fixture =
        super::super::materialize_model_eval_fixture(&fixture, temp.path().join("workspace"))?;
    let path = temp.path().join("records.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("cost-fixture", "cost-model").with_store(store.clone());
    session.ensure_identity_entry()?;
    let root = crate::provider_connections::default_setup_root_config();
    let mut options = crate::build_run_options(
        &root,
        fixture.workspace_root.clone(),
        sigil_kernel::InteractionMode::Headless,
        None,
    );
    options.max_turns = Some(1);
    let mut events = CostPipelineEvents::default();
    Agent::new(
        CostPipelineProvider(CostPipelineMode::Success),
        ToolRegistry::new(),
    )
    .run(&mut session, "first", options.clone(), &mut events)
    .await?;
    let owner = sigil_kernel::RunCancellationOwner::new();
    let input = AgentRunInput::user("second").with_cancellation(owner.handle());
    if matches!(second, CostPipelineMode::Cancel) {
        events.cancellation = Some(owner);
    }
    let result = Agent::new(CostPipelineProvider(second), ToolRegistry::new())
        .run_with_input(&mut session, input, options, &mut events)
        .await;
    assert_eq!(
        result.is_ok(),
        matches!(
            second,
            CostPipelineMode::Success | CostPipelineMode::Unpriced
        )
    );
    let records = store.read_handle().read_event_records()?;
    let coverage = super::super::trajectory::observe_usage_coverage(
        &records,
        session.session_scope_id(),
        0,
        &events.usage,
    )?;
    let isolated = ModelEvalIsolatedConfig {
        config_path: temp.path().join("config.toml"),
        config_digest: sha256_digest(b"cost-test"),
        isolated_config_digest: sha256_digest(b"cost-test"),
        provider: "cost-fixture".into(),
        model: "cost-model".into(),
        session_path: path,
    };
    let mut execution = base_execution(
        &fixture,
        1,
        "cost-pipeline".into(),
        isolated,
        ModelEvalRunExecutionStatus::ExecutionFailed,
        Duration::ZERO,
        None,
    );
    execution.cost_confidence = coverage.confidence(&events.usage);
    execution.usage = events.usage;
    execution.usage_coverage = coverage;
    drop(session);
    drop(store);
    let campaign = ModelEvalCampaignExecution {
        campaign_id: "cost-pipeline".into(),
        started_at_unix_ms: 1,
        ended_at_unix_ms: 2,
        output_dir: temp.path().join("report"),
        planned_runs: 1,
        reservation_microusd_per_run: 1_000_000,
        charged_microusd: 1_000_000,
        orchestration_route_contract: None,
        orchestration_corpus_digest: None,
        runs: vec![execution],
    };
    Ok((temp, campaign, records))
}

fn assert_cost_report(campaign: &ModelEvalCampaignExecution, expected: Option<f64>) -> Result<()> {
    super::super::write_model_eval_campaign_report(campaign)?;
    super::super::trajectory::write_campaign_trajectory(campaign, None)?;
    let result: serde_json::Value = serde_json::from_str(
        fs::read_to_string(campaign.output_dir.join("results.jsonl"))?.trim(),
    )?;
    let trajectory: serde_json::Value = serde_json::from_str(
        fs::read_to_string(campaign.output_dir.join("trajectory.jsonl"))?.trim(),
    )?;
    assert_eq!(
        trajectory["billing"]["reported_or_priced_cost_usd"],
        serde_json::json!(expected)
    );
    assert_eq!(
        trajectory["billing"]["known_usage_cost_usd"],
        serde_json::json!(campaign.runs[0].usage.known_usage_cost_usd())
    );
    if let Some(expected) = expected {
        assert_eq!(
            result["usage"]["reported_cost_microusd"],
            usd_to_microusd(expected).expect("valid fixture cost")
        );
        assert_eq!(result["usage"]["confidence"], "estimated");
    } else {
        assert!(result["usage"]["reported_cost_microusd"].is_null());
        assert_eq!(result["usage"]["confidence"], "unknown");
    }
    Ok(())
}

#[tokio::test]
async fn model_eval_cost_pipeline_all_attempts_priced_is_known() -> Result<()> {
    let (_temp, campaign, records) = cost_pipeline(CostPipelineMode::Success).await?;
    let run = &campaign.runs[0];
    assert_eq!(run.usage_coverage.physical_attempts, 2);
    assert_eq!(run.usage_coverage.attempts_with_usage, 2);
    assert!(run.usage_coverage.is_complete());
    assert_eq!(run.total_cost_usd(), Some(0.2));
    assert_cost_report(&campaign, Some(0.2))?;

    // Equal counts cannot rescue a usage assigned to the other physical attempt.
    let mut wrong = records.clone();
    let projection = sigil_kernel::ProviderPhysicalAttemptProjection::from_records(&records)?;
    let starts = projection.attempts();
    let usage_id = &starts[0]
        .terminal
        .as_ref()
        .expect("first successful attempt terminal")
        .durable_output_event_ids[0];
    let record = wrong
        .iter_mut()
        .find(|record| &record.stored_event().event_id == usage_id)
        .expect("referenced usage record");
    let sigil_kernel::SessionStreamRecord::Stored(event) = record;
    event.correlation_id = Some(starts[1].started_event_id.clone());
    event.record_checksum = event.compute_record_checksum()?;
    assert!(
        super::super::trajectory::observe_usage_coverage(
            &wrong,
            starts[0].session_id(),
            0,
            &run.usage,
        )
        .is_err()
    );
    assert!(
        super::super::trajectory::observe_usage_coverage(
            &records,
            "foreign-session",
            0,
            &run.usage,
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn model_eval_cost_pipeline_failed_usage_preserves_subtotal_but_not_total() -> Result<()> {
    let (_temp, campaign, _) = cost_pipeline(CostPipelineMode::Failed).await?;
    let run = &campaign.runs[0];
    assert_eq!(run.usage_coverage.physical_attempts, 2);
    assert_eq!(run.usage_coverage.attempts_with_usage, 1);
    assert_eq!(run.usage_coverage.missing_usage_attempts, 1);
    assert_eq!(run.usage.known_usage_cost_usd(), Some(0.1));
    assert_eq!(run.total_cost_usd(), None);
    assert_cost_report(&campaign, None)
}

#[tokio::test]
async fn model_eval_cost_pipeline_cancel_and_missing_terminal_remain_unknown() -> Result<()> {
    let (_temp, campaign, records) = cost_pipeline(CostPipelineMode::Cancel).await?;
    assert_eq!(campaign.runs[0].usage.known_usage_cost_usd(), Some(0.1));
    assert_eq!(campaign.runs[0].total_cost_usd(), None);
    assert_cost_report(&campaign, None)?;
    // A reader can observe the contiguous prefix before the active attempt settles.
    // Deleting interior terminal records would instead create a corrupt sequence gap.
    let terminal_index = records
        .iter()
        .rposition(|record| {
            record.stored_event().event_kind()
                == Some(sigil_kernel::DurableEventType::ProviderPhysicalAttemptTerminal)
        })
        .expect("second attempt terminal");
    let without_terminal = records[..terminal_index].to_vec();
    let coverage = super::super::trajectory::observe_usage_coverage(
        &without_terminal,
        &without_terminal[0].stored_event().session_id,
        0,
        &campaign.runs[0].usage,
    )?;
    assert_eq!(coverage.physical_attempts, 2);
    assert_eq!(coverage.unterminated_attempts, 1);
    assert!(!coverage.is_complete());
    Ok(())
}

#[tokio::test]
async fn model_eval_cost_pipeline_missing_price_is_unknown_despite_complete_usage() -> Result<()> {
    let (_temp, campaign, _) = cost_pipeline(CostPipelineMode::Unpriced).await?;
    let run = &campaign.runs[0];
    assert!(run.usage_coverage.is_complete());
    assert_eq!(run.usage.usage_events, 2);
    assert_eq!(run.usage.priced_usage_events, 1);
    assert_eq!(run.usage.known_usage_cost_usd(), Some(0.1));
    assert_eq!(run.total_cost_usd(), None);
    assert_cost_report(&campaign, None)
}

#[tokio::test]
async fn model_eval_cost_pipeline_typed_no_consumption_is_separate_from_missing_usage() -> Result<()>
{
    let (_temp, campaign, _) = cost_pipeline(CostPipelineMode::NoConsumption).await?;
    let run = &campaign.runs[0];
    assert_eq!(run.usage_coverage.physical_attempts, 2);
    assert_eq!(
        run.usage_coverage.confirmed_no_model_consumption_attempts,
        1
    );
    assert_eq!(run.usage_coverage.missing_usage_attempts, 0);
    assert_eq!(run.usage_coverage.attempts_with_usage, 1);
    assert_eq!(run.total_cost_usd(), Some(0.1));
    assert_cost_report(&campaign, Some(0.1))
}
