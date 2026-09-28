use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    ApprovalMode, AutoApproveHandler, NetworkPolicy, PermissionMode, PublicRunEvent,
    PublicRunEventKind, RootConfig, StorageRoot, UsageStats,
};

use crate::application_run::{
    ApplicationRunConstraints, ApplicationRunEventHandler, ApplicationRunOutput,
    ApplicationRunRequest, ApplicationRunServices, prepare_application_run,
};
use crate::provider_api_key_env_names;

use super::{
    LoadedModelEvalFixture, MODEL_EVAL_ORCHESTRATION_ROUTE_CONTRACT_SCHEMA_VERSION,
    MaterializedModelEvalFixture, ModelEvalOrchestrationRouteContractV1, load_model_eval_fixture,
    materialize_model_eval_fixture, sha256_digest, sync_directory,
    verification::ModelEvalVerificationExecution,
};

/// Bounded above the RFC-0053 minimum 20-negative/10-positive orchestration corpus.
pub const MODEL_EVAL_MAX_CASES: usize = 64;
const _: () = assert!(MODEL_EVAL_MAX_CASES >= 30);
pub const MODEL_EVAL_MAX_REPETITIONS: u32 = 10;
pub const MODEL_EVAL_MAX_CAMPAIGN_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);
pub const MODEL_EVAL_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Explicit bounds and inputs for one opt-in model-eval campaign.
#[derive(Debug, Clone)]
pub struct ModelEvalCampaignRequest {
    pub config_path: PathBuf,
    pub fixture_roots: Vec<PathBuf>,
    pub orchestration_route_contract: Option<ModelEvalOrchestrationRouteContractV1>,
    pub repetitions: u32,
    pub max_cost_microusd: u64,
    pub campaign_timeout: Duration,
    pub output_dir: PathBuf,
    pub release_output_owner: Option<Arc<dyn super::ReleaseOutputOwnerV1>>,
}

/// Secret-free generated config and paths used by one model-eval repetition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEvalIsolatedConfig {
    pub config_path: PathBuf,
    pub config_digest: String,
    pub isolated_config_digest: String,
    pub provider: String,
    pub model: String,
    pub session_path: PathBuf,
}

/// Cost observation quality for one provider-backed repetition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelEvalCostConfidence {
    Reported,
    Estimated,
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
    pub priced_usage_events: u32,
    pub pricing_snapshot_ids: BTreeSet<String>,
    pub provider_system_fingerprints: BTreeSet<String>,
    known_priced_cost_usd: f64,
}

impl ModelEvalUsageTotals {
    pub(super) fn record(&mut self, usage: &UsageStats) {
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
        let valid_snapshot = usage
            .pricing_snapshot
            .as_ref()
            .filter(|snapshot| snapshot.validate().is_ok());
        if let Some(snapshot) = valid_snapshot {
            self.pricing_snapshot_ids
                .insert(snapshot.snapshot_id.clone());
        }
        let complete_prices = valid_snapshot.is_some_and(|snapshot| {
            usage.cache_usage.as_ref().is_some_and(|cache| {
                cache
                    .validate_for_prompt_tokens(usage.prompt_tokens)
                    .is_ok()
                    && cache.read.is_some()
                    && cache.uncached.is_some()
                    && (cache.write.is_none() || snapshot.cache_write_per_unit.is_some())
            })
        });
        let valid_costs = usage.input_cost.is_finite()
            && usage.output_cost.is_finite()
            && usage.input_cost >= 0.0
            && usage.output_cost >= 0.0;
        if valid_costs && (complete_prices || usage.input_cost > 0.0 || usage.output_cost > 0.0) {
            self.priced_usage_events = self.priced_usage_events.saturating_add(1);
            self.known_priced_cost_usd += usage.input_cost + usage.output_cost;
        }
        if let Some(fingerprint) = usage
            .system_fingerprint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            self.provider_system_fingerprints
                .insert(fingerprint.to_owned());
        }
    }

    fn merge(&mut self, other: &Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.cache_hit_tokens = self.cache_hit_tokens.saturating_add(other.cache_hit_tokens);
        self.cache_miss_tokens = self
            .cache_miss_tokens
            .saturating_add(other.cache_miss_tokens);
        self.input_cost_usd += other.input_cost_usd;
        self.output_cost_usd += other.output_cost_usd;
        self.known_priced_cost_usd += other.known_priced_cost_usd;
        self.usage_events = self.usage_events.saturating_add(other.usage_events);
        self.priced_usage_events = self
            .priced_usage_events
            .saturating_add(other.priced_usage_events);
        self.pricing_snapshot_ids
            .extend(other.pricing_snapshot_ids.iter().cloned());
        self.provider_system_fingerprints
            .extend(other.provider_system_fingerprints.iter().cloned());
    }

    #[must_use]
    pub(super) fn priced_usage_total_usd(&self) -> Option<f64> {
        let total = self.input_cost_usd + self.output_cost_usd;
        (self.usage_events > 0
            && self.priced_usage_events == self.usage_events
            && total.is_finite()
            && total >= 0.0)
            .then_some(total)
    }

    /// The known priced subset only; never a claim that every request was measured.
    #[must_use]
    pub fn known_usage_cost_usd(&self) -> Option<f64> {
        (self.priced_usage_events > 0 && self.known_priced_cost_usd.is_finite())
            .then_some(self.known_priced_cost_usd)
    }
}

/// Exact durable-attempt coverage of the observed usage, not billing authority.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ModelEvalUsageCoverage {
    pub observed: bool,
    pub physical_attempts: usize,
    pub attempts_with_usage: usize,
    pub confirmed_no_model_consumption_attempts: usize,
    pub missing_usage_attempts: usize,
    pub unterminated_attempts: usize,
    pub incomplete_attempts: usize,
    pub unassociated_usage_events: usize,
    pub observation_error: Option<String>,
}

impl ModelEvalUsageCoverage {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.observed
            && self.physical_attempts > 0
            && self.missing_usage_attempts == 0
            && self.unterminated_attempts == 0
            && self.incomplete_attempts == 0
            && self.unassociated_usage_events == 0
            && self.observation_error.is_none()
    }

    fn merge(&mut self, other: &Self) {
        self.observed &= other.observed;
        self.physical_attempts += other.physical_attempts;
        self.attempts_with_usage += other.attempts_with_usage;
        self.confirmed_no_model_consumption_attempts +=
            other.confirmed_no_model_consumption_attempts;
        self.missing_usage_attempts += other.missing_usage_attempts;
        self.unterminated_attempts += other.unterminated_attempts;
        self.incomplete_attempts += other.incomplete_attempts;
        self.unassociated_usage_events += other.unassociated_usage_events;
        if self.observation_error.is_none() {
            self.observation_error.clone_from(&other.observation_error);
        }
    }

    pub(super) fn cost_usd(&self, usage: &ModelEvalUsageTotals) -> Option<f64> {
        if !self.is_complete() {
            return None;
        }
        if self.physical_attempts == self.confirmed_no_model_consumption_attempts
            && usage.usage_events == 0
        {
            return Some(0.0);
        }
        usage.priced_usage_total_usd()
    }

    fn confidence(&self, usage: &ModelEvalUsageTotals) -> ModelEvalCostConfidence {
        if self.cost_usd(usage).is_none() {
            ModelEvalCostConfidence::Unknown
        } else if usage.pricing_snapshot_ids.is_empty() {
            ModelEvalCostConfidence::Reported
        } else {
            ModelEvalCostConfidence::Estimated
        }
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
    pub isolated_config_digest: String,
    pub session_path: PathBuf,
    pub manifest_digest: String,
    pub tree_digest: String,
    pub provider: String,
    pub model: String,
    pub status: ModelEvalRunExecutionStatus,
    pub output: Option<ApplicationRunOutput>,
    pub usage: ModelEvalUsageTotals,
    pub cost_confidence: ModelEvalCostConfidence,
    pub usage_coverage: ModelEvalUsageCoverage,
    durable_sequence: Option<u64>,
    pub charged_microusd: u64,
    pub wall_time: Duration,
    pub public_event_count: u64,
    pub safe_error: Option<String>,
    pub verification: Option<ModelEvalVerificationExecution>,
    pub materialized_fixture: MaterializedModelEvalFixture,
    pub turns: Vec<super::ModelEvalTurnTrace>,
}

impl ModelEvalRunExecution {
    /// Complete observed request cost only; an incomplete request remains unknown.
    #[must_use]
    pub fn total_cost_usd(&self) -> Option<f64> {
        self.usage_coverage.cost_usd(&self.usage)
    }
}

/// Aggregate raw execution output before verification/report acceptance.
#[derive(Debug, Clone)]
pub struct ModelEvalCampaignExecution {
    pub campaign_id: String,
    pub started_at_unix_ms: u64,
    pub ended_at_unix_ms: u64,
    pub output_dir: PathBuf,
    pub planned_runs: usize,
    pub reservation_microusd_per_run: u64,
    pub charged_microusd: u64,
    pub orchestration_route_contract: Option<ModelEvalOrchestrationRouteContractV1>,
    pub orchestration_corpus_digest: Option<String>,
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
    // This is a new fixture configuration, never a publication back to the user's document.
    // Keep the selected composition while discarding inactive raw module payloads before the
    // fixture deliberately replaces module settings below.
    config.composition = config.composition.selection_only();
    if config.permission.mode == PermissionMode::ReadOnly
        || (config.permission.mode == PermissionMode::Manual
            && fixture.tool_scope.names.iter().any(|name| {
                matches!(name.as_str(), "edit_file" | "write_file")
                    && config.permission.tools.get(name) != Some(&ApprovalMode::Allow)
            }))
    {
        bail!("model eval requires a config that permits controlled workspace edits");
    }

    let loaded = crate::provider_connections::load_provider_connections(&config);
    if !loaded.issues.is_empty() {
        let issue_codes = loaded
            .issues
            .iter()
            .map(|issue| issue.code)
            .collect::<Vec<_>>()
            .join(",");
        bail!("model eval provider connection configuration is invalid: {issue_codes}");
    }
    let default_model = loaded
        .default_model
        .clone()
        .context("model eval requires an exact default model route")?;
    let selected_connection = loaded
        .connections
        .get(&default_model.connection_id)
        .context("model eval default connection is missing")?;
    let active_provider =
        crate::provider_connections::runtime_provider_name(&selected_connection.config).to_owned();
    let mut isolated_connection = selected_connection.config.clone();
    if matches!(
        isolated_connection.credential,
        crate::provider_connections::CredentialRefConfig::Stored { .. }
    ) {
        let names = crate::provider_api_key_env_names(&active_provider)
            .context("model eval connection has no process-environment credential binding")?;
        let environment_name = names
            .iter()
            .copied()
            .find(|name| {
                env::var_os(name)
                    .and_then(|value| value.into_string().ok())
                    .is_some_and(|value| !value.trim().is_empty())
            })
            .or_else(|| names.first().copied())
            .expect("provider API key environment names are non-empty");
        isolated_connection.credential =
            crate::provider_connections::CredentialRefConfig::Environment {
                name: environment_name.to_owned(),
            };
    }
    let isolated_connections =
        BTreeMap::from([(default_model.connection_id.clone(), isolated_connection)]);
    config.agent.max_turns = Some(
        usize::try_from(fixture.max_turns).context("model eval max_turns does not fit usize")?,
    );
    config.memory.enabled = false;
    config.skills.enabled = false;
    config.skills.user_skills = false;
    config.skills.user_agents = false;
    config.compaction.enabled = false;
    config.code_intelligence.enabled = false;
    config.task.enabled = fixture.orchestration.is_some() || fixture.agent_delegation;
    if fixture.orchestration.is_some() {
        config.task =
            crate::orchestration_rollout::orchestration_rollout_task_candidate(&config.task);
    } else {
        // Deterministic eval runs must never auto-route: an unqualified fresh config now defaults
        // to `Auto`, so pin the isolated policy explicitly.
        config.task.routing_policy = sigil_kernel::TaskRoutingPolicy::Manual;
        if fixture.agent_delegation {
            config.task.multi_agent_mode = sigil_kernel::MultiAgentMode::Proactive;
        }
    }
    config.web.enabled = false;
    config.web.network_mode = NetworkPolicy::Deny;
    config.web.search_mcp = None;
    config.mcp_servers.clear();
    config = crate::provider_connections::materialize_root_config(
        &config,
        &isolated_connections,
        &default_model,
    )?;

    config.workspace.root = "<model-eval-workspace>".to_owned();
    config.storage.state_root = StorageRoot::Path("<model-eval-state>".to_owned());
    config.storage.cache_root = StorageRoot::Path("<model-eval-cache>".to_owned());
    config.session.log_dir = Some("<model-eval-sessions>".to_owned());
    let comparison_config = toml::to_string(&config)
        .context("failed to serialize normalized model eval config identity")?;
    let config_digest = sha256_digest(comparison_config.as_bytes());

    config.workspace.root = fixture.workspace_root.display().to_string();
    config.storage.state_root = StorageRoot::Path(run_root.join("state").display().to_string());
    config.storage.cache_root = StorageRoot::Path(run_root.join("cache").display().to_string());
    let session_dir = run_root.join("sessions");
    config.session.log_dir = Some(session_dir.display().to_string());

    fs::create_dir_all(&session_dir)
        .with_context(|| format!("failed to create {}", session_dir.display()))?;
    let config_path = run_root.join("config.toml");
    config.save(&config_path)?;
    let config_bytes = fs::read(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    std::str::from_utf8(&config_bytes).context("isolated config is not UTF-8")?;
    let reloaded = RootConfig::load(&config_path)?.with_effective_composition()?;
    let reloaded_connections = crate::provider_connections::load_provider_connections(&reloaded);
    if reloaded.workspace.root != config.workspace.root
        || reloaded_connections.default_model.as_ref() != Some(&default_model)
        || reloaded_connections.connections.len() != 1
        || !reloaded_connections.issues.is_empty()
        || !reloaded.mcp_servers.is_empty()
        || reloaded.web.enabled
        || reloaded.task.enabled != (fixture.orchestration.is_some() || fixture.agent_delegation)
    {
        bail!("isolated model eval config did not round-trip its safety boundary");
    }
    if fixture.orchestration.is_some()
        && (reloaded.task.routing_policy != sigil_kernel::TaskRoutingPolicy::Auto
            || reloaded.task.multi_agent_mode != sigil_kernel::MultiAgentMode::Proactive)
    {
        bail!("isolated orchestration eval config did not retain the rollout task policy");
    }
    if fixture.agent_delegation
        && (reloaded.task.routing_policy != sigil_kernel::TaskRoutingPolicy::Manual
            || reloaded.task.multi_agent_mode != sigil_kernel::MultiAgentMode::Proactive)
    {
        bail!(
            "isolated agent delegation eval config did not retain the explicit delegation policy"
        );
    }
    sync_directory(run_root)?;

    Ok(ModelEvalIsolatedConfig {
        config_path,
        config_digest,
        isolated_config_digest: sha256_digest(&config_bytes),
        provider: active_provider,
        model: default_model.model_id,
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
    let started_at_unix_ms = unix_time_ms()?;
    let (request, fixtures, orchestration_corpus_digest, output_dir) =
        tokio::task::spawn_blocking(move || {
            let (fixtures, digest) = preflight_campaign(&request)?;
            let output_dir = if let Some(owner) = request.release_output_owner.as_deref() {
                owner.prepare_tree_root(&request.output_dir)?;
                request.output_dir.clone()
            } else {
                create_campaign_output_dir(&request.output_dir)?
            };
            Ok::<_, anyhow::Error>((request, fixtures, digest, output_dir))
        })
        .await
        .context("model eval preflight worker failed")??;
    let planned_runs = fixtures
        .len()
        .checked_mul(request.repetitions as usize)
        .context("model eval planned run count overflowed")?;
    let reservation_microusd_per_run =
        model_eval_reservation_microusd(request.max_cost_microusd, planned_runs)?;
    let campaign_id = format!("model-eval-{}", uuid::Uuid::new_v4());
    let deadline = Instant::now()
        .checked_add(request.campaign_timeout)
        .context("model eval campaign deadline overflowed")?;
    let mut charged_microusd = 0_u64;
    let mut runs = Vec::with_capacity(planned_runs);

    for fixture in fixtures {
        for repetition in 1..=request.repetitions {
            let run_root = output_dir.join(format!("{}-{repetition}", fixture.manifest.id));
            let fixture = fixture.clone();
            let source_config = request.config_path.clone();
            let (fixture, materialized, isolated) = tokio::task::spawn_blocking(move || {
                fs::create_dir(&run_root)
                    .with_context(|| format!("failed to create {}", run_root.display()))?;
                let materialized =
                    materialize_model_eval_fixture(&fixture, run_root.join("workspace"))?;
                let isolated =
                    write_isolated_model_eval_config(&source_config, &materialized, &run_root)?;
                Ok::<_, anyhow::Error>((fixture, materialized, isolated))
            })
            .await
            .context("model eval fixture materialization worker failed")??;
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
            let run_services = model_eval_run_services(&materialized, services);
            let mut execution = execute_model_eval_run(
                &materialized,
                repetition,
                run_id,
                isolated,
                remaining,
                &run_services,
            )
            .await;
            if execution.status != ModelEvalRunExecutionStatus::PreparationFailed {
                let actual = execution
                    .usage
                    .known_usage_cost_usd()
                    .and_then(usd_to_microusd)
                    .unwrap_or(reservation_microusd_per_run);
                execution.charged_microusd = actual.max(reservation_microusd_per_run);
                charged_microusd = charged_microusd.saturating_add(execution.charged_microusd);
            }
            runs.push(execution);
        }
    }
    let ended_at_unix_ms = unix_time_ms()?;
    let campaign = ModelEvalCampaignExecution {
        campaign_id,
        started_at_unix_ms,
        ended_at_unix_ms,
        output_dir,
        planned_runs,
        reservation_microusd_per_run,
        charged_microusd,
        orchestration_route_contract: request.orchestration_route_contract,
        orchestration_corpus_digest,
        runs,
    };
    tokio::task::spawn_blocking(move || {
        let owner = request.release_output_owner.as_deref();
        super::write_model_eval_campaign_report_with_owner(&campaign, owner)?;
        super::trajectory::write_campaign_trajectory(&campaign, owner)?;
        sync_directory(&campaign.output_dir)?;
        Ok(campaign)
    })
    .await
    .context("model eval report worker failed")?
}

fn model_eval_run_services(
    fixture: &MaterializedModelEvalFixture,
    services: &ApplicationRunServices,
) -> ApplicationRunServices {
    if fixture.orchestration.is_none() && !fixture.agent_delegation {
        return services.clone();
    }
    services.clone().with_task_role_provider_builder(Arc::new(
        crate::agent_supervisor::task_role_runtime::RuntimeTaskRoleProviderBuilder,
    ))
}

pub(crate) fn model_eval_reservation_microusd(
    max_cost_microusd: u64,
    planned_runs: usize,
) -> Result<u64> {
    let planned_runs = u64::try_from(planned_runs)
        .context("model eval planned run count does not fit the budget counter")?;
    if planned_runs == 0 {
        bail!("model eval campaign must plan at least one run");
    }
    let reservation = max_cost_microusd / planned_runs;
    if reservation == 0 {
        bail!("model eval budget must reserve at least one microUSD per planned run");
    }
    Ok(reservation)
}

fn preflight_campaign(
    request: &ModelEvalCampaignRequest,
) -> Result<(Vec<LoadedModelEvalFixture>, Option<String>)> {
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
    let config = RootConfig::load(&request.config_path)
        .context("model eval source config preflight failed")?;
    let loaded = crate::provider_connections::load_provider_connections(&config);
    anyhow::ensure!(
        loaded.issues.is_empty(),
        "model eval source config contains invalid provider connections"
    );
    let default_model = loaded
        .default_model
        .as_ref()
        .context("model eval source config has no default model route")?;
    let selected_connection = loaded
        .connections
        .get(&default_model.connection_id)
        .context("model eval default connection is missing")?;
    let active_provider =
        crate::provider_connections::runtime_provider_name(&selected_connection.config);
    match &selected_connection.config.credential {
        crate::provider_connections::CredentialRefConfig::Environment { name } => {
            let environment = crate::provider_connections::ProcessCredentialEnvironment;
            let credential_is_present =
                crate::provider_connections::read_configured_environment_credential(
                    &selected_connection.config,
                    name,
                    &environment,
                )
                .is_some();
            if !credential_is_present {
                bail!(
                    "model eval requires {name}; isolated eval configs intentionally exclude provider credentials from retained artifacts"
                );
            }
        }
        crate::provider_connections::CredentialRefConfig::Stored { .. } => {
            let names = provider_api_key_env_names(active_provider).with_context(|| {
                format!(
                    "model eval provider {active_provider} has no runtime credential environment mapping"
                )
            })?;
            let credential_env = names.iter().copied().find(|name| {
                env::var_os(name)
                    .and_then(|value| value.into_string().ok())
                    .is_some_and(|value| !value.trim().is_empty())
            });
            if credential_env.is_none() {
                bail!(
                    "model eval requires one of {}; isolated eval configs intentionally exclude provider credentials from retained artifacts",
                    names.join(" or ")
                );
            }
        }
        crate::provider_connections::CredentialRefConfig::None => {}
    }
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
    let orchestration_cases = fixtures
        .iter()
        .filter(|fixture| fixture.manifest.orchestration.is_some())
        .count();
    let orchestration_corpus_digest = match (
        orchestration_cases,
        request.orchestration_route_contract.as_ref(),
    ) {
        (0, None) => None,
        (0, Some(_)) => {
            bail!("orchestration route contract requires orchestration fixtures");
        }
        (_, None) => {
            bail!(
                "orchestration fixtures require an exact route contract before provider dispatch"
            );
        }
        (count, Some(_)) if count != fixtures.len() => {
            bail!("model eval campaign cannot mix orchestration and ordinary fixtures");
        }
        (_, Some(contract)) => {
            validate_orchestration_route_contract(contract)?;
            let corpus_versions = fixtures
                .iter()
                .filter_map(|fixture| fixture.manifest.orchestration.as_ref())
                .map(|orchestration| orchestration.corpus_version.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            if corpus_versions.len() != 1 {
                bail!("orchestration campaign must use one exact corpus version");
            }
            if contract.provider_kind == "deepseek" {
                let (_, provider_system_fingerprint) = contract
                    .canonical_model_version
                    .rsplit_once('@')
                    .context("deepseek route contract must bind a provider system fingerprint")?;
                let expected = super::build_model_eval_orchestration_route_contract(
                    &super::ModelEvalRouteContractBuildRequest {
                        config_path: request.config_path.clone(),
                        fixture_roots: request.fixture_roots.clone(),
                        provider_system_fingerprint: provider_system_fingerprint.to_owned(),
                    },
                )?;
                if &expected != contract {
                    bail!(
                        "orchestration route contract does not match the exact candidate binary, config, corpus, prompts, and tool profiles"
                    );
                }
            }
            Some(orchestration_corpus_digest(&fixtures))
        }
    };
    Ok((fixtures, orchestration_corpus_digest))
}

fn validate_orchestration_route_contract(
    contract: &ModelEvalOrchestrationRouteContractV1,
) -> Result<()> {
    if contract.schema_version != MODEL_EVAL_ORCHESTRATION_ROUTE_CONTRACT_SCHEMA_VERSION {
        bail!(
            "unsupported orchestration route contract schema version: {}",
            contract.schema_version
        );
    }
    for (field, value) in [
        ("provider_kind", contract.provider_kind.as_str()),
        ("endpoint_family", contract.endpoint_family.as_str()),
        (
            "canonical_model_version",
            contract.canonical_model_version.as_str(),
        ),
        ("sigil_commit", contract.sigil_commit.as_str()),
        ("sigil_build", contract.sigil_build.as_str()),
    ] {
        if value.trim().is_empty()
            || value.chars().count() > 512
            || value.chars().any(char::is_control)
        {
            bail!("orchestration route contract has an invalid {field}");
        }
    }
    for (field, digest) in [
        (
            "routing_prompt_digest",
            contract.routing_prompt_digest.as_str(),
        ),
        (
            "direct_task_prompt_digest",
            contract.direct_task_prompt_digest.as_str(),
        ),
        (
            "system_prompt_digest",
            contract.system_prompt_digest.as_str(),
        ),
        (
            "tool_profile_contract_digest",
            contract.tool_profile_contract_digest.as_str(),
        ),
    ] {
        if digest.len() != 71
            || !digest.starts_with("sha256:")
            || !digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("orchestration route contract has an invalid {field}");
        }
    }
    Ok(())
}

fn orchestration_corpus_digest(fixtures: &[LoadedModelEvalFixture]) -> String {
    let mut identity = Vec::new();
    identity.extend_from_slice(b"sigil-orchestration-corpus-v1\0");
    for fixture in fixtures {
        let orchestration = fixture
            .manifest
            .orchestration
            .as_ref()
            .expect("preflight only hashes orchestration fixtures");
        identity.extend_from_slice(fixture.manifest.id.as_bytes());
        identity.push(0);
        identity.extend_from_slice(fixture.manifest_digest.as_bytes());
        identity.push(0);
        identity.extend_from_slice(orchestration.corpus_version.as_bytes());
        identity.push(0);
        identity.extend_from_slice(match orchestration.case_class {
            sigil_kernel::OrchestrationEvalCaseClass::Chat => b"chat",
            sigil_kernel::OrchestrationEvalCaseClass::PlanReview => b"plan_review",
            sigil_kernel::OrchestrationEvalCaseClass::DirectTask => b"direct_task",
        });
        identity.push(0);
    }
    sha256_digest(&identity)
}

async fn execute_model_eval_run(
    fixture: &MaterializedModelEvalFixture,
    repetition: u32,
    run_id: String,
    mut isolated: ModelEvalIsolatedConfig,
    timeout: Duration,
    services: &ApplicationRunServices,
) -> ModelEvalRunExecution {
    let started = Instant::now();
    let mut usage = ModelEvalUsageTotals::default();
    let mut coverage: Option<ModelEvalUsageCoverage> = None;
    let mut durable_sequence = Some(0);
    let mut event_count = 0_u64;
    let mut turns = Vec::new();
    let prompts = std::iter::once(&fixture.prompt).chain(&fixture.followup_prompts);
    let mut result = None;
    for (index, prompt) in prompts.enumerate() {
        let mut turn_fixture = fixture.clone();
        turn_fixture.prompt.clone_from(prompt);
        turn_fixture.followup_prompts.clear();
        let turn_id = if index == 0 {
            run_id.clone()
        } else {
            format!("{run_id}-turn-{}", index + 1)
        };
        let timings_before = sigil_kernel::run_diagnostics::run_timing_snapshot();
        let mut current = execute_model_eval_turn(
            &turn_fixture,
            repetition,
            turn_id,
            isolated.clone(),
            timeout.saturating_sub(started.elapsed()),
            services,
            index == fixture.followup_prompts.len(),
            durable_sequence,
        )
        .await;
        let timings_after = sigil_kernel::run_diagnostics::run_timing_snapshot();
        usage.merge(&current.usage);
        match &mut coverage {
            Some(coverage) => coverage.merge(&current.usage_coverage),
            None => coverage = Some(current.usage_coverage.clone()),
        }
        durable_sequence = current.durable_sequence;
        event_count = event_count.saturating_add(current.public_event_count);
        turns.push(super::ModelEvalTurnTrace::from_execution(
            index + 1,
            &current,
            &timings_before,
            &timings_after,
        ));
        isolated.session_path = current.session_path.clone();
        let may_continue = current.status == ModelEvalRunExecutionStatus::Completed
            && current.output.as_ref().is_some_and(|output| {
                output.terminal_status
                    == crate::application_run::ApplicationRunTerminalStatus::Succeeded
            });
        current.materialized_fixture = fixture.clone();
        result = Some(current);
        if !may_continue || started.elapsed() >= timeout {
            break;
        }
    }
    let mut result = result.expect("every fixture has an initial prompt");
    if turns.len() <= fixture.followup_prompts.len()
        && result.status == ModelEvalRunExecutionStatus::Completed
        && started.elapsed() >= timeout
    {
        result.status = ModelEvalRunExecutionStatus::TimedOut;
        result.safe_error =
            Some("campaign deadline reached before all fixed user turns".to_owned());
    }
    result.run_id = run_id;
    result.usage = usage;
    result.usage_coverage = coverage.unwrap_or_default();
    result.cost_confidence = result.usage_coverage.confidence(&result.usage);
    result.public_event_count = event_count;
    result.wall_time = started.elapsed();
    result.turns = turns;
    result
}

#[allow(clippy::too_many_arguments)]
fn execute_model_eval_turn<'a>(
    fixture: &'a MaterializedModelEvalFixture,
    repetition: u32,
    run_id: String,
    isolated: ModelEvalIsolatedConfig,
    timeout: Duration,
    services: &'a ApplicationRunServices,
    verify_final: bool,
    after_sequence: Option<u64>,
) -> std::pin::Pin<Box<impl std::future::Future<Output = ModelEvalRunExecution> + 'a>> {
    // Construct this large run future outside the campaign/case poll frames so the
    // shared preparation state is not repeatedly materialized on ordinary worker stacks.
    Box::pin(async move {
        let started = Instant::now();
        // Model-eval is a production application-run driver, so it must perform the same authority
        // boot and exact workspace registration as the shipping CLI/HTTP/TUI surfaces before a
        // managed file tool can plan or execute. The isolated run root gives this repetition its
        // own durable authority state and state namespace.
        let boot_services = services.clone();
        let config_path = isolated.config_path.clone();
        let workspace_root = fixture.workspace_root.clone();
        let services = match tokio::task::spawn_blocking(move || {
            crate::r71_authority_composition::attach_boot_authority_to_services(
                boot_services,
                &config_path,
                &workspace_root,
            )
        })
        .await
        {
            Ok(Ok(services)) => services,
            Ok(Err(_)) | Err(_) => {
                return base_execution(
                    fixture,
                    repetition,
                    run_id,
                    isolated,
                    ModelEvalRunExecutionStatus::PreparationFailed,
                    started.elapsed(),
                    Some("model eval authority boot failed before provider dispatch".to_owned()),
                );
            }
        };
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
        let prepared = match prepare_application_run(request, &services).await {
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
        // Current-schema boot may redirect the requested session path to an authority-managed leaf;
        // carry the admitted physical session path into verification and reporting instead of
        // reopening the pre-admission request path.
        let session_path = prepared.session_log_path().to_path_buf();
        let session_scope = prepared.session_id().to_owned();
        let reader = prepared.session_projection_owner().read_handle();
        let (execution, control) = prepared.into_parts();
        let mut events = ModelEvalEventRecorder::default();
        let mut approvals = AutoApproveHandler;
        let mut future = Box::pin(execution.execute(&mut events, &mut approvals));
        let mut status = ModelEvalRunExecutionStatus::Completed;
        let mut output = None;
        let mut safe_error = None;
        let mut cancellation_ticket = None;
        let mut execution_joined = true;

        match tokio::time::timeout(timeout.saturating_sub(started.elapsed()), future.as_mut()).await
        {
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

        let verification = if verify_final && status == ModelEvalRunExecutionStatus::Completed {
            match super::verification::verify_model_eval_run_with_deadline(
                fixture,
                &isolated.config_path,
                &session_path,
                &isolated.provider,
                &isolated.model,
                &run_id,
                started.checked_add(timeout),
            )
            .await
            {
                Ok(verification) => Some(verification),
                Err(_) => {
                    safe_error =
                        Some("fixture verification could not produce durable evidence".to_owned());
                    None
                }
            }
        } else {
            None
        };

        if status == ModelEvalRunExecutionStatus::Completed && started.elapsed() >= timeout {
            status = ModelEvalRunExecutionStatus::TimedOut;
            safe_error = Some("fixture verification reached the campaign deadline".to_owned());
        }

        // Observation cannot change execution, cancellation, or acceptance. Keep ownership of
        // the bounded read worker until it returns, and mark unavailable evidence as unknown.
        let observed_usage = events.usage.clone();
        let observation = tokio::task::spawn_blocking(move || {
            let records = reader.read_event_records()?;
            let end = records
                .last()
                .map(|record| record.stored_event().stream_sequence);
            let coverage = after_sequence
                .context("previous usage boundary unavailable")
                .and_then(|after| {
                    super::trajectory::observe_usage_coverage(
                        &records,
                        &session_scope,
                        after,
                        &observed_usage,
                    )
                });
            Ok::<_, anyhow::Error>((coverage, end))
        })
        .await;
        let (usage_coverage, durable_sequence) = match observation {
            Ok(Ok((Ok(coverage), end))) => (coverage, end),
            Ok(Ok((Err(_), end))) => (
                ModelEvalUsageCoverage {
                    observation_error: Some("durable usage association unavailable".into()),
                    ..ModelEvalUsageCoverage::default()
                },
                end,
            ),
            Ok(Err(_)) | Err(_) => (
                ModelEvalUsageCoverage {
                    observation_error: Some("durable usage observation unavailable".into()),
                    ..ModelEvalUsageCoverage::default()
                },
                None,
            ),
        };
        let cost_confidence = usage_coverage.confidence(&events.usage);
        ModelEvalRunExecution {
            fixture_id: fixture.fixture_id.clone(),
            repetition,
            run_id,
            workspace_root: fixture.workspace_root.clone(),
            config_path: isolated.config_path,
            config_digest: isolated.config_digest,
            isolated_config_digest: isolated.isolated_config_digest,
            session_path,
            manifest_digest: fixture.manifest_digest.clone(),
            tree_digest: fixture.tree_digest.clone(),
            provider: isolated.provider,
            model: isolated.model,
            status,
            output,
            usage: events.usage,
            cost_confidence,
            usage_coverage,
            durable_sequence,
            charged_microusd: 0,
            wall_time: started.elapsed(),
            public_event_count: events.event_count,
            safe_error,
            verification,
            materialized_fixture: fixture.clone(),
            turns: Vec::new(),
        }
    })
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
        isolated_config_digest: isolated.isolated_config_digest,
        session_path: isolated.session_path,
        manifest_digest: fixture.manifest_digest.clone(),
        tree_digest: fixture.tree_digest.clone(),
        provider: isolated.provider,
        model: isolated.model,
        status,
        output: None,
        usage: ModelEvalUsageTotals::default(),
        cost_confidence: ModelEvalCostConfidence::Unknown,
        usage_coverage: ModelEvalUsageCoverage::default(),
        durable_sequence: None,
        charged_microusd: 0,
        wall_time,
        public_event_count: 0,
        safe_error,
        verification: None,
        materialized_fixture: fixture.clone(),
        turns: Vec::new(),
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

fn usd_to_microusd(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value > u64::MAX as f64 / 1_000_000.0 {
        return None;
    }
    Some((value * 1_000_000.0).ceil() as u64)
}

fn unix_time_ms() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?;
    Ok(elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
#[path = "../tests/model_eval_cost_tests.rs"]
mod cost_tests;

#[cfg(test)]
#[path = "../tests/model_eval_task_usage_support.rs"]
mod task_usage_support;
#[cfg(test)]
pub use task_usage_support::{observe_task_eval_patch, observe_task_eval_usage};
