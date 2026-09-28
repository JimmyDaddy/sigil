//! Derived evaluation artifacts. They observe the existing session and never grant authority.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sigil_kernel::run_diagnostics::{RunTimingPhase, RunTimingSnapshot, run_timing_key};
use sigil_kernel::{ControlEntry, JsonlSessionStore, SessionLogEntry, ToolExecutionStatus};

use super::{ModelEvalCampaignExecution, ModelEvalRunExecution, ReleaseOutputOwnerV1};

/// Derives coverage from exact causal references. Counts are output, never association logic.
pub(super) fn observe_usage_coverage(
    records: &[sigil_kernel::SessionStreamRecord],
    expected_scope: &str,
    after_sequence: u64,
    public_usage: &super::ModelEvalUsageTotals,
) -> Result<super::ModelEvalUsageCoverage> {
    let attempts = sigil_kernel::ProviderPhysicalAttemptProjection::from_records(records)?;
    let mut usage_by_id = BTreeMap::new();
    let mut durable_usage = super::ModelEvalUsageTotals::default();
    let mut child_scopes = false;
    for record in records {
        let event = record.stored_event();
        if event.session_id != expected_scope {
            bail!("usage observation session scope differs");
        }
        if event.stream_sequence <= after_sequence {
            continue;
        }
        match record.session_log_entry()? {
            Some(SessionLogEntry::Control(
                ControlEntry::UsageSnapshot(usage)
                | ControlEntry::SemanticCompactionUsageSnapshot(usage),
            )) => {
                durable_usage.record(&usage);
                if usage_by_id.insert(event.event_id.as_str(), event).is_some() {
                    bail!("usage event identity is repeated");
                }
            }
            Some(SessionLogEntry::Control(ControlEntry::AgentThreadStarted(_))) => {
                child_scopes = true;
            }
            _ => {}
        }
    }
    let mut coverage = super::ModelEvalUsageCoverage {
        observed: true,
        ..Default::default()
    };
    let mut associated = BTreeSet::new();
    for attempt in attempts
        .attempts()
        .into_iter()
        .filter(|attempt| attempt.started_stream_sequence > after_sequence)
    {
        coverage.physical_attempts += 1;
        if attempt.session_id() != expected_scope {
            bail!("physical attempt session scope differs");
        }
        let Some(terminal) = &attempt.terminal else {
            coverage.unterminated_attempts += 1;
            continue;
        };
        let mut has_usage = false;
        for id in &terminal.durable_output_event_ids {
            let Some(event) = usage_by_id.get(id.as_str()) else {
                continue;
            };
            if event.correlation_id.as_deref() != Some(attempt.started_event_id.as_str())
                || event.session_id != attempt.session_id()
                || !associated.insert(id.as_str())
            {
                bail!("usage output does not belong to its physical attempt");
            }
            has_usage = true;
        }
        if has_usage {
            coverage.attempts_with_usage += 1;
        }
        match terminal.outcome {
            sigil_kernel::ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption => {
                coverage.confirmed_no_model_consumption_attempts += 1;
                if has_usage {
                    bail!("no-consumption attempt has usage");
                }
            }
            sigil_kernel::ProviderPhysicalAttemptOutcome::Completed => {
                if !has_usage {
                    coverage.missing_usage_attempts += 1;
                }
            }
            _ => {
                // A interrupted/failed stream may publish only partial usage. Retain the
                // known subtotal, but never infer a fully measured request from its presence.
                coverage.incomplete_attempts += 1;
                if !has_usage {
                    coverage.missing_usage_attempts += 1;
                }
            }
        }
    }
    coverage.unassociated_usage_events = usage_by_id
        .keys()
        .filter(|id| !associated.contains(**id))
        .count();
    if durable_usage != *public_usage {
        coverage.observation_error = Some("public and durable usage differ".into());
    } else if child_scopes {
        coverage.observation_error = Some("child-session provider usage is unobserved".into());
    }
    Ok(coverage)
}

/// One fixed user turn in an evaluation case, measured on the same process clock.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEvalTurnTrace {
    pub turn: usize,
    pub run_id: String,
    pub execution_status: String,
    pub terminal_status: Option<String>,
    pub wall_time_ms: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub usage_events: u32,
    pub priced_usage_events: u32,
    pub reported_or_priced_cost_usd: Option<f64>,
    pub known_usage_cost_usd: Option<f64>,
    pub usage_coverage: super::ModelEvalUsageCoverage,
    pub pricing_snapshot_ids: BTreeSet<String>,
    /// Best-effort observations from the existing process buffer, captured at this turn's end.
    stage_timings: ModelEvalStageTimings,
}

impl ModelEvalTurnTrace {
    pub(super) fn from_execution(
        turn: usize,
        execution: &ModelEvalRunExecution,
        timings_before: &RunTimingSnapshot,
        timings_after: &RunTimingSnapshot,
    ) -> Self {
        Self {
            turn,
            run_id: execution.run_id.clone(),
            execution_status: format!("{:?}", execution.status),
            terminal_status: execution
                .output
                .as_ref()
                .map(|output| format!("{:?}", output.terminal_status)),
            wall_time_ms: execution.wall_time.as_millis().min(u128::from(u64::MAX)) as u64,
            prompt_tokens: execution.usage.prompt_tokens,
            completion_tokens: execution.usage.completion_tokens,
            cache_hit_tokens: execution.usage.cache_hit_tokens,
            cache_miss_tokens: execution.usage.cache_miss_tokens,
            usage_events: execution.usage.usage_events,
            priced_usage_events: execution.usage.priced_usage_events,
            reported_or_priced_cost_usd: execution.total_cost_usd(),
            known_usage_cost_usd: execution.usage.known_usage_cost_usd(),
            usage_coverage: execution.usage_coverage.clone(),
            pricing_snapshot_ids: execution.usage.pricing_snapshot_ids.clone(),
            stage_timings: ModelEvalStageTimings::observe(
                &execution.run_id,
                timings_before,
                timings_after,
            ),
        }
    }
}

/// A derived side table, not another recorder or a claim of UI first-paint coverage.
#[derive(Debug, Clone, Serialize)]
struct ModelEvalStageTimings {
    snapshots_available: bool,
    /// Process-wide window information; drops cannot be attributed to this particular run.
    global_dropped_during_turn: Option<u64>,
    phases: Vec<ModelEvalPhaseDurations>,
}

#[derive(Debug, Clone, Serialize)]
struct ModelEvalPhaseDurations {
    phase: RunTimingPhase,
    /// In sequence order. Missing phases remain unknown, rather than zero or successful.
    elapsed_us: Option<Vec<u64>>,
}

impl ModelEvalStageTimings {
    fn observe(run_id: &str, before: &RunTimingSnapshot, after: &RunTimingSnapshot) -> Self {
        let snapshots_available = before.available
            && after.available
            && before.process_instance == after.process_instance;
        let global_dropped_during_turn =
            snapshots_available.then(|| after.dropped.saturating_sub(before.dropped));
        let frontier = before
            .observations
            .iter()
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or(0);
        let key = run_timing_key(run_id);
        // Reuse the kernel's closed vocabulary. Every phase is present so absent observations,
        // including UI-only phases in this non-UI harness, serialize explicitly as unknown.
        let phases = [
            RunTimingPhase::InputAccepted,
            RunTimingPhase::FirstFeedbackFrame,
            RunTimingPhase::Admission,
            RunTimingPhase::Preparation,
            RunTimingPhase::SessionPreparation,
            RunTimingPhase::ProviderConstruction,
            RunTimingPhase::ToolSurface,
            RunTimingPhase::RequestContext,
            RunTimingPhase::ProviderDispatch,
            RunTimingPhase::ProviderStreamReady,
            RunTimingPhase::ProviderFirstChunk,
            RunTimingPhase::ProviderFirstContent,
            RunTimingPhase::ToolExecution,
            RunTimingPhase::CancellationRequested,
            RunTimingPhase::CancellationSettled,
        ]
        .into_iter()
        .map(|phase| {
            let durations = after
                .observations
                .iter()
                .filter(|entry| {
                    snapshots_available
                        && entry.sequence > frontier
                        && entry.run_key == key
                        && entry.phase == phase
                })
                .map(|entry| entry.elapsed_us)
                .collect::<Vec<_>>();
            ModelEvalPhaseDurations {
                phase,
                elapsed_us: (!durations.is_empty()).then_some(durations),
            }
        })
        .collect();
        Self {
            snapshots_available,
            global_dropped_during_turn,
            phases,
        }
    }
}

/// Case-wide counters derived from durable facts; unavailable observations remain `None`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelEvalTrajectorySummary {
    pub physical_provider_attempts: Option<usize>,
    pub tool_status_counts: BTreeMap<String, usize>,
    pub verification_status_counts: BTreeMap<String, usize>,
    /// Existing audits do not prove path/range identity plus absence of intermediate writes.
    pub redundant_reads: Option<usize>,
    /// Approval counts do not establish actual human intervention in an automated campaign.
    pub human_interventions: Option<usize>,
    /// Failed checks alone do not establish which model edits were ineffective repair rounds.
    pub ineffective_repair_rounds: Option<usize>,
    /// Strict observable subsets; never substitutes for the broader unknown metrics above.
    pub activity: Option<super::ModelEvalActivityMetrics>,
    pub observation_error: Option<String>,
}

#[derive(Serialize)]
struct PatchFile {
    path: PathBuf,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
    added_lines: Option<usize>,
    removed_lines: Option<usize>,
    status: &'static str,
}

pub(super) fn write_campaign_trajectory(
    campaign: &ModelEvalCampaignExecution,
    owner: Option<&dyn ReleaseOutputOwnerV1>,
) -> Result<()> {
    let mut rows = Vec::new();
    for execution in &campaign.runs {
        let mut summary = ModelEvalTrajectorySummary::default();
        let mut paths = execution
            .materialized_fixture
            .fixture_files
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        match JsonlSessionStore::read_event_records(&execution.session_path) {
            Ok(records) => {
                match super::activity::observe_activity(&records) {
                    Ok(activity) => summary.activity = Some(activity),
                    Err(_) => {
                        summary.observation_error =
                            Some("activity observations unavailable".to_owned())
                    }
                }
                match sigil_kernel::ProviderPhysicalAttemptProjection::from_records(&records) {
                    Ok(projection) => {
                        summary.physical_provider_attempts = Some(projection.attempts().len())
                    }
                    Err(_) => {
                        summary.observation_error =
                            Some("provider attempt projection unavailable".to_owned())
                    }
                }
                let mut tools = BTreeMap::new();
                for record in &records {
                    match record.session_log_entry()? {
                        Some(SessionLogEntry::Control(ControlEntry::ToolExecution(entry))) => {
                            for path in &entry.changed_files {
                                let path = Path::new(path);
                                let relative = if path.is_absolute() {
                                    path.strip_prefix(&execution.workspace_root).ok()
                                } else {
                                    Some(path)
                                };
                                if let Some(path) = relative.filter(|path| {
                                    super::validate_relative_path("observed edit path", path)
                                        .is_ok()
                                }) {
                                    paths.insert(path.to_path_buf());
                                }
                            }
                            tools.insert(
                                entry.call_id.clone(),
                                (entry.tool_name.clone(), entry.status),
                            );
                        }
                        Some(SessionLogEntry::Control(ControlEntry::VerificationRecorded(
                            entry,
                        ))) => {
                            *summary
                                .verification_status_counts
                                .entry(format!("{:?}", entry.receipt.check_status))
                                .or_default() += 1;
                        }
                        _ => {}
                    }
                }
                for (_, (tool, status)) in tools {
                    let label = match status {
                        ToolExecutionStatus::Started => "unfinished",
                        ToolExecutionStatus::Completed => "completed",
                        ToolExecutionStatus::Failed => "failed",
                        ToolExecutionStatus::Cancelled => "cancelled",
                        ToolExecutionStatus::Interrupted => "interrupted",
                    };
                    *summary
                        .tool_status_counts
                        .entry(format!("{tool}:{label}"))
                        .or_default() += 1;
                }
            }
            Err(_) => summary.observation_error = Some("durable trajectory unavailable".to_owned()),
        }
        let (patch, files, complete) = render_patch(execution, &paths);
        let patch_name = format!("{}-{}.patch", execution.fixture_id, execution.repetition);
        publish(
            &campaign.output_dir.join(&patch_name),
            patch.as_bytes(),
            owner,
        )?;
        rows.push(serde_json::json!({
            "schema_version": 1, "fixture_id": execution.fixture_id, "repetition": execution.repetition,
            "run_id": execution.run_id, "fixture_manifest_digest": execution.manifest_digest,
            "fixture_tree_digest": execution.tree_digest, "config_digest": execution.config_digest,
            "turns": execution.turns, "trajectory": summary,
            "billing": { "reported_or_priced_cost_usd": execution.total_cost_usd(),
                "known_usage_cost_usd": execution.usage.known_usage_cost_usd(),
                "usage_coverage": execution.usage_coverage,
                "pricing_snapshot_ids": execution.usage.pricing_snapshot_ids,
                "priced_usage_events": execution.usage.priced_usage_events, "usage_events": execution.usage.usage_events,
                "budget_reservation_microusd": campaign.reservation_microusd_per_run,
                "budget_accounted_microusd": execution.charged_microusd,
                "budget_accounting_is_actual_bill": false },
            "patch": { "path": patch_name, "sha256": super::sha256_digest(patch.as_bytes()), "complete_for_declared_and_audited_files": complete, "files": files }
        }));
    }
    let mut output = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut output, &row)?;
        output.push(b'\n');
    }
    publish(
        &campaign.output_dir.join("trajectory.jsonl"),
        &output,
        owner,
    )
}

fn publish(path: &Path, bytes: &[u8], owner: Option<&dyn ReleaseOutputOwnerV1>) -> Result<()> {
    if let Some(owner) = owner {
        owner.publish_file(path, bytes)
    } else {
        fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
    }
}

fn render_patch(
    execution: &ModelEvalRunExecution,
    paths: &BTreeSet<PathBuf>,
) -> (String, Vec<PatchFile>, bool) {
    let mut patch = String::new();
    let mut files = Vec::new();
    let mut complete = paths.len() <= 64;
    for path in paths.iter().take(64) {
        let observed = (|| {
            let before = match execution
                .materialized_fixture
                .fixture_file_sources
                .get(path)
            {
                Some(source) => {
                    let source = super::resolve_source_path(
                        &execution.materialized_fixture.fixture_source_root,
                        source,
                    )?;
                    let bytes = super::read_bounded_regular_file(
                        &source,
                        super::MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES,
                        "patch source",
                    )?;
                    super::validate_digest(
                        "patch source digest",
                        &execution.materialized_fixture.fixture_file_digests[path],
                        &bytes,
                    )?;
                    Some(bytes)
                }
                None => None,
            };
            let after = match fs::symlink_metadata(execution.workspace_root.join(path)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                _ => {
                    let source = super::resolve_source_path(&execution.workspace_root, path)?;
                    Some(super::read_bounded_regular_file(
                        &source,
                        super::MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES,
                        "patch result",
                    )?)
                }
            };
            Ok::<_, anyhow::Error>((before, after))
        })();
        let Ok((before, after)) = observed else {
            complete = false;
            files.push(PatchFile {
                path: path.clone(),
                before_sha256: None,
                after_sha256: None,
                added_lines: None,
                removed_lines: None,
                status: "unavailable",
            });
            continue;
        };
        if before == after {
            continue;
        }
        let mut file = PatchFile {
            path: path.clone(),
            before_sha256: before.as_deref().map(super::sha256_digest),
            after_sha256: after.as_deref().map(super::sha256_digest),
            added_lines: None,
            removed_lines: None,
            status: "binary",
        };
        if let (Ok(before_text), Ok(after_text)) = (
            std::str::from_utf8(before.as_deref().unwrap_or_default()),
            std::str::from_utf8(after.as_deref().unwrap_or_default()),
        ) && !before_text.contains('\0')
            && !after_text.contains('\0')
        {
            let diff = similar::TextDiff::from_lines(before_text, after_text);
            file.added_lines = Some(
                diff.iter_all_changes()
                    .filter(|change| change.tag() == similar::ChangeTag::Insert)
                    .count(),
            );
            file.removed_lines = Some(
                diff.iter_all_changes()
                    .filter(|change| change.tag() == similar::ChangeTag::Delete)
                    .count(),
            );
            file.status = "text";
            patch.push_str(
                &diff
                    .unified_diff()
                    .context_radius(3)
                    .header(
                        &format!("a/{}", path.display()),
                        &format!("b/{}", path.display()),
                    )
                    .to_string(),
            );
        } else {
            complete = false;
        }
        files.push(file);
    }
    (patch, files, complete)
}

#[cfg(test)]
#[path = "../tests/model_eval_trajectory_tests.rs"]
mod tests;
