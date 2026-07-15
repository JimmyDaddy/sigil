use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sigil_kernel::{
    ApprovalMode, AutoApproveHandler, NetworkPolicy, PermissionMode, PublicRunEvent,
    PublicRunEventKind, RootConfig, StorageRoot, UsageStats,
};

use crate::application_run::{
    ApplicationRunConstraints, ApplicationRunEventHandler, ApplicationRunOutput,
    ApplicationRunRequest, ApplicationRunServices, prepare_application_run,
};

use super::{
    LoadedModelEvalFixture, MaterializedModelEvalFixture, load_model_eval_fixture,
    materialize_model_eval_fixture, sha256_digest, sync_directory,
};

pub const MODEL_EVAL_MAX_CASES: usize = 16;
pub const MODEL_EVAL_MAX_REPETITIONS: u32 = 10;
pub const MODEL_EVAL_MAX_CAMPAIGN_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub const MODEL_EVAL_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Explicit bounds and inputs for one opt-in model-eval campaign.
#[derive(Debug, Clone)]
pub struct ModelEvalCampaignRequest {
    pub config_path: PathBuf,
    pub fixture_roots: Vec<PathBuf>,
    pub repetitions: u32,
    pub max_cost_microusd: u64,
    pub campaign_timeout: Duration,
    pub output_dir: PathBuf,
}

/// Secret-free generated config and paths used by one model-eval repetition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEvalIsolatedConfig {
    pub config_path: PathBuf,
    pub config_digest: String,
    pub provider: String,
    pub model: String,
    pub session_path: PathBuf,
}

/// Cost observation quality for one provider-backed repetition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelEvalCostConfidence {
    Reported,
    Unknown,
}

/// Execution state before acceptance/report aggregation is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelEvalRunExecutionStatus {
    Completed,
    PreparationFailed,
    ExecutionFailed,
    TimedOut,
    BudgetSkipped,
    DeadlineSkipped,
}

/// Provider-neutral usage totals observed through production public run events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelEvalUsageTotals {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub input_cost_usd: f64,
    pub output_cost_usd: f64,
    pub usage_events: u32,
}

impl ModelEvalUsageTotals {
    fn record(&mut self, usage: &UsageStats) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(usage.completion_tokens);
        self.cache_hit_tokens = self.cache_hit_tokens.saturating_add(usage.cache_hit_tokens);
        self.cache_miss_tokens = self
            .cache_miss_tokens
            .saturating_add(usage.cache_miss_tokens);
        self.input_cost_usd += usage.input_cost;
        self.output_cost_usd += usage.output_cost;
        self.usage_events = self.usage_events.saturating_add(1);
    }

    #[must_use]
    pub fn total_cost_usd(&self) -> Option<f64> {
        let total = self.input_cost_usd + self.output_cost_usd;
        (self.usage_events > 0 && total.is_finite() && total >= 0.0).then_some(total)
    }
}

/// Raw production-path result for one fixture repetition.
#[derive(Debug, Clone)]
pub struct ModelEvalRunExecution {
    pub fixture_id: String,
    pub repetition: u32,
    pub run_id: String,
    pub workspace_root: PathBuf,
    pub config_path: PathBuf,
    pub config_digest: String,
    pub session_path: PathBuf,
    pub manifest_digest: String,
    pub tree_digest: String,
    pub provider: String,
    pub model: String,
    pub status: ModelEvalRunExecutionStatus,
    pub output: Option<ApplicationRunOutput>,
    pub usage: ModelEvalUsageTotals,
    pub cost_confidence: ModelEvalCostConfidence,
    pub charged_microusd: u64,
    pub wall_time: Duration,
    pub public_event_count: u64,
    pub safe_error: Option<String>,
    pub materialized_fixture: MaterializedModelEvalFixture,
}

/// Aggregate raw execution output before verification/report acceptance.
#[derive(Debug, Clone)]
pub struct ModelEvalCampaignExecution {
    pub campaign_id: String,
    pub output_dir: PathBuf,
    pub planned_runs: usize,
    pub reservation_microusd_per_run: u64,
    pub charged_microusd: u64,
    pub runs: Vec<ModelEvalRunExecution>,
}

/// Writes a secret-free, isolated runtime config for one materialized repetition.
///
/// # Errors
///
/// Returns an error when the source config is invalid, embeds an unsafe provider URL, uses a
/// read-only permission mode, or the isolated config cannot be persisted and reloaded.
pub fn write_isolated_model_eval_config(
    source_config_path: &Path,
    fixture: &MaterializedModelEvalFixture,
    run_root: &Path,
) -> Result<ModelEvalIsolatedConfig> {
    let mut config = RootConfig::load(source_config_path)?;
    if config.permission.mode == PermissionMode::ReadOnly
        || (config.permission.mode == PermissionMode::Manual
            && fixture.tool_scope.names.iter().any(|name| {
                matches!(name.as_str(), "edit_file" | "write_file")
                    && config.permission.tools.get(name) != Some(&ApprovalMode::Allow)
            }))
    {
        bail!("model eval requires a config that permits controlled workspace edits");
    }

    let active_provider = config.agent.provider.clone();
    config.providers.retain(|name, _| name == &active_provider);
    for value in config.providers.values_mut() {
        scrub_provider_secret_fields(value)?;
    }
    config.workspace.root = fixture.workspace_root.display().to_string();
    config.storage.state_root = StorageRoot::Path(run_root.join("state").display().to_string());
    config.storage.cache_root = StorageRoot::Path(run_root.join("cache").display().to_string());
    let session_dir = run_root.join("sessions");
    config.session.log_dir = Some(session_dir.display().to_string());
    config.agent.max_turns = Some(
        usize::try_from(fixture.max_turns).context("model eval max_turns does not fit usize")?,
    );
    config.memory.enabled = false;
    config.skills.enabled = false;
    config.skills.user_skills = false;
    config.skills.user_agents = false;
    config.compaction.enabled = false;
    config.code_intelligence.enabled = false;
    config.task.enabled = false;
    config.web.enabled = false;
    config.web.network_mode = NetworkPolicy::Deny;
    config.web.search_mcp = None;
    config.mcp_servers.clear();

    fs::create_dir_all(&session_dir)
        .with_context(|| format!("failed to create {}", session_dir.display()))?;
    let config_path = run_root.join("config.toml");
    config.save(&config_path)?;
    let config_bytes = fs::read(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let rendered = std::str::from_utf8(&config_bytes).context("isolated config is not UTF-8")?;
    if rendered.to_ascii_lowercase().contains("api_key") {
        bail!("isolated model eval config still contains an API-key field");
    }
    let reloaded = RootConfig::load(&config_path)?;
    if reloaded.workspace.root != config.workspace.root
        || reloaded.agent.provider != config.agent.provider
        || reloaded.agent.model != config.agent.model
        || !reloaded.mcp_servers.is_empty()
        || reloaded.web.enabled
    {
        bail!("isolated model eval config did not round-trip its safety boundary");
    }
    sync_directory(run_root)?;

    Ok(ModelEvalIsolatedConfig {
        config_path,
        config_digest: sha256_digest(&config_bytes),
        provider: config.agent.provider,
        model: config.agent.model,
        session_path: session_dir.join("run.jsonl"),
    })
}

/// Executes an explicit model-eval campaign through the production application run service.
///
/// # Errors
///
/// Returns an error for invalid campaign bounds, fixture/config preflight failure, or unsafe
/// output paths. Individual provider/run failures are retained as structured run executions.
pub async fn run_model_eval_campaign(
    request: ModelEvalCampaignRequest,
    services: &ApplicationRunServices,
) -> Result<ModelEvalCampaignExecution> {
    let fixtures = preflight_campaign(&request)?;
    let planned_runs = fixtures
        .len()
        .checked_mul(request.repetitions as usize)
        .context("model eval planned run count overflowed")?;
    let reservation_microusd_per_run = request
        .max_cost_microusd
        .checked_add(planned_runs as u64 - 1)
        .context("model eval budget reservation overflowed")?
        / planned_runs as u64;
    let output_dir = create_campaign_output_dir(&request.output_dir)?;
    let campaign_id = format!("model-eval-{}", uuid::Uuid::new_v4());
    let deadline = Instant::now()
        .checked_add(request.campaign_timeout)
        .context("model eval campaign deadline overflowed")?;
    let mut charged_microusd = 0_u64;
    let mut runs = Vec::with_capacity(planned_runs);

    for fixture in fixtures {
        for repetition in 1..=request.repetitions {
            let run_root = output_dir.join(format!("{}-{repetition}", fixture.manifest.id));
            fs::create_dir(&run_root)
                .with_context(|| format!("failed to create {}", run_root.display()))?;
            let materialized =
                materialize_model_eval_fixture(&fixture, run_root.join("workspace"))?;
            let isolated =
                write_isolated_model_eval_config(&request.config_path, &materialized, &run_root)?;
            let run_id = format!("{}-{}-{repetition}", campaign_id, fixture.manifest.id);

            if Instant::now() >= deadline
                || request.max_cost_microusd.saturating_sub(charged_microusd)
                    < reservation_microusd_per_run
            {
                let deadline_reached = Instant::now() >= deadline;
                runs.push(skipped_execution(
                    &materialized,
                    repetition,
                    run_id,
                    isolated,
                    if deadline_reached {
                        ModelEvalRunExecutionStatus::DeadlineSkipped
                    } else {
                        ModelEvalRunExecutionStatus::BudgetSkipped
                    },
                    if deadline_reached {
                        "campaign deadline reached before provider admission"
                    } else {
                        "campaign cost admission budget exhausted"
                    },
                ));
                continue;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut execution = execute_model_eval_run(
                &materialized,
                repetition,
                run_id,
                isolated,
                remaining,
                services,
            )
            .await;
            if execution.status != ModelEvalRunExecutionStatus::PreparationFailed {
                let actual = execution
                    .usage
                    .total_cost_usd()
                    .and_then(usd_to_microusd)
                    .unwrap_or(reservation_microusd_per_run);
                execution.charged_microusd = actual.max(reservation_microusd_per_run);
                charged_microusd = charged_microusd.saturating_add(execution.charged_microusd);
            }
            runs.push(execution);
        }
    }
    sync_directory(&output_dir)?;

    Ok(ModelEvalCampaignExecution {
        campaign_id,
        output_dir,
        planned_runs,
        reservation_microusd_per_run,
        charged_microusd,
        runs,
    })
}

fn preflight_campaign(request: &ModelEvalCampaignRequest) -> Result<Vec<LoadedModelEvalFixture>> {
    if request.fixture_roots.is_empty() || request.fixture_roots.len() > MODEL_EVAL_MAX_CASES {
        bail!(
            "model eval campaign must contain between 1 and {} cases",
            MODEL_EVAL_MAX_CASES
        );
    }
    if request.repetitions == 0 || request.repetitions > MODEL_EVAL_MAX_REPETITIONS {
        bail!(
            "model eval repetitions must be between 1 and {}",
            MODEL_EVAL_MAX_REPETITIONS
        );
    }
    if request.max_cost_microusd == 0 {
        bail!("model eval max cost admission budget must be greater than zero");
    }
    if request.campaign_timeout.is_zero()
        || request.campaign_timeout > MODEL_EVAL_MAX_CAMPAIGN_TIMEOUT
    {
        bail!("model eval campaign timeout is outside the V1 bound");
    }
    RootConfig::load(&request.config_path).context("model eval source config preflight failed")?;
    let mut fixtures = request
        .fixture_roots
        .iter()
        .map(load_model_eval_fixture)
        .collect::<Result<Vec<_>>>()?;
    fixtures.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
    for pair in fixtures.windows(2) {
        if pair[0].manifest.id == pair[1].manifest.id {
            bail!("model eval campaign contains duplicate fixture ids");
        }
    }
    Ok(fixtures)
}

async fn execute_model_eval_run(
    fixture: &MaterializedModelEvalFixture,
    repetition: u32,
    run_id: String,
    isolated: ModelEvalIsolatedConfig,
    timeout: Duration,
    services: &ApplicationRunServices,
) -> ModelEvalRunExecution {
    let started = Instant::now();
    let mut request = ApplicationRunRequest::non_interactive(
        &isolated.config_path,
        &fixture.workspace_root,
        fixture.prompt.clone(),
        run_id.clone(),
    );
    request.session_path = Some(isolated.session_path.clone());
    let request = request.with_constraints(ApplicationRunConstraints {
        max_turns: fixture.max_turns as usize,
        max_output_tokens: fixture.max_output_tokens,
        tool_scope: fixture.tool_scope.clone(),
    });
    let prepared = match prepare_application_run(request, services).await {
        Ok(prepared) => prepared,
        Err(_) => {
            return base_execution(
                fixture,
                repetition,
                run_id,
                isolated,
                ModelEvalRunExecutionStatus::PreparationFailed,
                started.elapsed(),
                Some("application run preparation failed before provider dispatch".to_owned()),
            );
        }
    };
    let (execution, control) = prepared.into_parts();
    let mut events = ModelEvalEventRecorder::default();
    let mut approvals = AutoApproveHandler;
    let mut future = Box::pin(execution.execute(&mut events, &mut approvals));
    let mut status = ModelEvalRunExecutionStatus::Completed;
    let mut output = None;
    let mut safe_error = None;
    let mut cancellation_ticket = None;
    let mut execution_joined = true;

    match tokio::time::timeout(timeout, future.as_mut()).await {
        Ok(Ok(run_output)) => output = Some(run_output),
        Ok(Err(_)) => {
            status = ModelEvalRunExecutionStatus::ExecutionFailed;
            safe_error = Some("application run execution failed".to_owned());
        }
        Err(_) => {
            status = ModelEvalRunExecutionStatus::TimedOut;
            safe_error = Some("application run exceeded the campaign deadline".to_owned());
            match control.request_cancellation(
                "model eval campaign deadline reached",
                Some(MODEL_EVAL_CANCELLATION_TIMEOUT),
                || {},
            ) {
                Ok(ticket) => cancellation_ticket = Some(ticket),
                Err(error) => cancellation_ticket = error.into_ticket(),
            }
            let join_timeout = cancellation_ticket
                .as_ref()
                .map_or(MODEL_EVAL_CANCELLATION_TIMEOUT, |ticket| {
                    ticket.remaining_timeout()
                });
            match tokio::time::timeout(join_timeout, future.as_mut()).await {
                Ok(Ok(run_output)) => output = Some(run_output),
                Ok(Err(_)) => {}
                Err(_) => execution_joined = false,
            }
        }
    }
    drop(future);
    if let Some(ticket) = cancellation_ticket
        && control
            .finalize_cancellation(ticket, execution_joined, &mut events)
            .await
            .is_err()
    {
        status = ModelEvalRunExecutionStatus::ExecutionFailed;
        safe_error = Some("application run cancellation could not be audited".to_owned());
    }

    let cost_confidence = if events.usage.total_cost_usd().is_some() {
        ModelEvalCostConfidence::Reported
    } else {
        ModelEvalCostConfidence::Unknown
    };
    ModelEvalRunExecution {
        fixture_id: fixture.fixture_id.clone(),
        repetition,
        run_id,
        workspace_root: fixture.workspace_root.clone(),
        config_path: isolated.config_path,
        config_digest: isolated.config_digest,
        session_path: isolated.session_path,
        manifest_digest: fixture.manifest_digest.clone(),
        tree_digest: fixture.tree_digest.clone(),
        provider: isolated.provider,
        model: isolated.model,
        status,
        output,
        usage: events.usage,
        cost_confidence,
        charged_microusd: 0,
        wall_time: started.elapsed(),
        public_event_count: events.event_count,
        safe_error,
        materialized_fixture: fixture.clone(),
    }
}

fn base_execution(
    fixture: &MaterializedModelEvalFixture,
    repetition: u32,
    run_id: String,
    isolated: ModelEvalIsolatedConfig,
    status: ModelEvalRunExecutionStatus,
    wall_time: Duration,
    safe_error: Option<String>,
) -> ModelEvalRunExecution {
    ModelEvalRunExecution {
        fixture_id: fixture.fixture_id.clone(),
        repetition,
        run_id,
        workspace_root: fixture.workspace_root.clone(),
        config_path: isolated.config_path,
        config_digest: isolated.config_digest,
        session_path: isolated.session_path,
        manifest_digest: fixture.manifest_digest.clone(),
        tree_digest: fixture.tree_digest.clone(),
        provider: isolated.provider,
        model: isolated.model,
        status,
        output: None,
        usage: ModelEvalUsageTotals::default(),
        cost_confidence: ModelEvalCostConfidence::Unknown,
        charged_microusd: 0,
        wall_time,
        public_event_count: 0,
        safe_error,
        materialized_fixture: fixture.clone(),
    }
}

fn skipped_execution(
    fixture: &MaterializedModelEvalFixture,
    repetition: u32,
    run_id: String,
    isolated: ModelEvalIsolatedConfig,
    status: ModelEvalRunExecutionStatus,
    reason: &str,
) -> ModelEvalRunExecution {
    base_execution(
        fixture,
        repetition,
        run_id,
        isolated,
        status,
        Duration::ZERO,
        Some(reason.to_owned()),
    )
}

#[derive(Debug, Default)]
struct ModelEvalEventRecorder {
    usage: ModelEvalUsageTotals,
    event_count: u64,
}

impl ApplicationRunEventHandler for ModelEvalEventRecorder {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        self.event_count = self.event_count.saturating_add(1);
        if let PublicRunEventKind::Usage { usage } = event.event {
            self.usage.record(&usage);
        }
        Ok(())
    }
}

fn create_campaign_output_dir(requested: &Path) -> Result<PathBuf> {
    if requested.exists() {
        bail!(
            "model eval output directory already exists: {}",
            requested.display()
        );
    }
    let parent = requested
        .parent()
        .context("model eval output directory has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", parent.display()))?;
    let leaf = requested
        .file_name()
        .context("model eval output directory has no final component")?;
    let output_dir = canonical_parent.join(leaf);
    fs::create_dir(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    Ok(output_dir)
}

fn scrub_provider_secret_fields(value: &mut Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            let keys = object.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let normalized = key.to_ascii_lowercase();
                if normalized.contains("api_key")
                    || normalized.contains("token")
                    || normalized.contains("secret")
                    || normalized.contains("password")
                    || normalized.contains("authorization")
                    || normalized.contains("header")
                {
                    object.remove(&key);
                    continue;
                }
                if normalized.contains("base_url")
                    && let Some(raw) = object.get(&key).and_then(Value::as_str)
                {
                    validate_provider_base_url(raw)?;
                }
                if let Some(child) = object.get_mut(&key) {
                    scrub_provider_secret_fields(child)?;
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                scrub_provider_secret_fields(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_provider_base_url(raw: &str) -> Result<()> {
    let parsed = url::Url::parse(raw).context("model eval provider base URL is invalid")?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("model eval provider base URL contains a secret-capable carrier");
    }
    Ok(())
}

fn usd_to_microusd(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value > u64::MAX as f64 / 1_000_000.0 {
        return None;
    }
    Some((value * 1_000_000.0).ceil() as u64)
}
