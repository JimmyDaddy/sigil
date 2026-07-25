use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sigil_kernel::{
    AgentRole, ORCHESTRATION_EVAL_MIN_NEGATIVE_CASES, ORCHESTRATION_EVAL_MIN_POSITIVE_CASES,
    OrchestrationEvalCaseClass, RootConfig, ToolRegistry, ToolRegistryScope, ToolSpec,
    WorkspaceTrust, changeset_only_child_contract_prompt, request_task_planning_tool_spec,
    runtime_context_v1_system_prompt_contract_material, task_plan_update_tool_spec,
    task_planner_prompt_contract_material, task_routing_system_prompt_contract_material,
};
use sigil_provider_deepseek::{
    DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT, DEFAULT_DEEPSEEK_V4_FLASH_MODEL,
};

use super::{
    LoadedModelEvalFixture, MODEL_EVAL_ORCHESTRATION_ROUTE_CONTRACT_SCHEMA_VERSION,
    ModelEvalOrchestrationRouteContractV1, load_model_eval_fixture, materialize_model_eval_fixture,
    sha256_digest, sync_directory, write_isolated_model_eval_config,
};
use crate::{
    AgentProfileRegistry, MAX_TASK_DISCOVERY_PROBES, ORCHESTRATION_RUNTIME_BUILD_ID,
    agent_supervisor::{planner_tools_with_discovery, task_discovery_system_prompt},
    application_run::constrain_application_tool_registry,
    build_role_tool_registry, build_tool_surface_without_eager_mcp_with_workspace_trust,
    provider_capabilities_for_name, provider_config_key, resolve_deepseek_config,
    unsupported_mcp_elicitation_handler, unsupported_mcp_runtime_event_handler,
};

const DEEPSEEK_ENDPOINT_FAMILY: &str = "openai_chat_completions";
const DEEPSEEK_PRIMARY_BASE_URL: &str = "https://api.deepseek.com";
const DEEPSEEK_BETA_BASE_URL: &str = "https://api.deepseek.com/beta";
const DEEPSEEK_CANONICAL_MODEL_NAME: &str = "DeepSeek-V4-Flash";

/// Frozen inputs used to derive one orchestration route contract without provider dispatch.
#[derive(Debug, Clone)]
pub struct ModelEvalRouteContractBuildRequest {
    pub config_path: PathBuf,
    pub fixture_roots: Vec<PathBuf>,
}

/// Derives a truthful route contract from the exact candidate binary, isolated evaluation
/// configuration, production tool schemas, and production task prompts.
///
/// # Errors
///
/// Returns an error when the corpus is incomplete, the route is not the pinned hosted DeepSeek V4
/// Flash route, build metadata is unavailable, or the production tool/profile surface cannot be
/// assembled without provider dispatch.
pub fn build_model_eval_orchestration_route_contract(
    request: &ModelEvalRouteContractBuildRequest,
) -> Result<ModelEvalOrchestrationRouteContractV1> {
    let source_config = RootConfig::load(&request.config_path)
        .context("route contract source config preflight failed")?;
    let provider_kind = provider_config_key(&source_config.agent.provider);
    if provider_kind != "deepseek" {
        bail!(
            "automatic route-contract derivation currently supports only the pinned deepseek route"
        );
    }
    if source_config.agent.model != DEFAULT_DEEPSEEK_V4_FLASH_MODEL {
        bail!("route-contract derivation requires model {DEFAULT_DEEPSEEK_V4_FLASH_MODEL}");
    }
    let deepseek = resolve_deepseek_config(&source_config)?;
    if deepseek.base_url.trim_end_matches('/') != DEEPSEEK_PRIMARY_BASE_URL
        || deepseek.beta_base_url.trim_end_matches('/') != DEEPSEEK_BETA_BASE_URL
    {
        bail!(
            "route-contract derivation requires the official DeepSeek primary and beta endpoints"
        );
    }

    let fixtures = load_complete_orchestration_corpus(&request.fixture_roots)?;
    let temporary = tempfile::tempdir().context("failed to create route-contract workspace")?;
    let materialized =
        materialize_model_eval_fixture(&fixtures[0], temporary.path().join("workspace"))?;
    let run_root = temporary.path().join("run");
    fs::create_dir(&run_root)?;
    let isolated =
        write_isolated_model_eval_config(&request.config_path, &materialized, &run_root)?;
    let isolated_config = RootConfig::load(&isolated.config_path)?;
    let capabilities = provider_capabilities_for_name(&isolated_config.agent.provider)
        .context("route-contract provider has no registered capabilities")?;
    let surface = build_tool_surface_without_eager_mcp_with_workspace_trust(
        &isolated_config,
        &capabilities,
        materialized.workspace_root.clone(),
        unsupported_mcp_elicitation_handler(),
        unsupported_mcp_runtime_event_handler(),
        WorkspaceTrust::Unknown,
    )?;
    let profile_registry = AgentProfileRegistry::from_root_config_with_workspace(
        &isolated_config,
        &materialized.workspace_root,
    )?;

    let routing_prompt_digest = digest_value(
        b"sigil-orchestration-routing-prompt-v1\0",
        &json!({
            "system_prompt": task_routing_system_prompt_contract_material(),
            "tool": request_task_planning_tool_spec(),
        }),
    )?;
    let planner_prompt_digest = digest_bytes(
        b"sigil-orchestration-planner-prompt-v1\0",
        task_planner_prompt_contract_material().as_bytes(),
    );
    let system_prompt_digest = digest_value(
        b"sigil-orchestration-system-prompt-v1\0",
        &json!({
            "runtime_context": runtime_context_v1_system_prompt_contract_material(),
            "changeset_only_child": changeset_only_child_contract_prompt(),
            "planner_discovery": task_discovery_system_prompt(),
        }),
    )?;
    let tool_profile_contract_digest = tool_profile_contract_digest(
        &isolated_config,
        &surface.registry,
        &profile_registry,
        &fixtures,
    )?;
    let sigil_commit = candidate_build_commit()?;

    Ok(ModelEvalOrchestrationRouteContractV1 {
        schema_version: MODEL_EVAL_ORCHESTRATION_ROUTE_CONTRACT_SCHEMA_VERSION,
        provider_kind: provider_kind.to_owned(),
        endpoint_family: DEEPSEEK_ENDPOINT_FAMILY.to_owned(),
        canonical_model_version: format!(
            "{DEEPSEEK_CANONICAL_MODEL_NAME}@{DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT}"
        ),
        routing_prompt_digest,
        planner_prompt_digest,
        system_prompt_digest,
        tool_profile_contract_digest,
        sigil_commit,
        sigil_build: ORCHESTRATION_RUNTIME_BUILD_ID.to_owned(),
    })
}

/// Publishes a newly derived contract without replacing an existing candidate artifact.
///
/// # Errors
///
/// Returns an error when the destination already exists, is not below a regular directory, or
/// cannot be written and synchronized.
pub fn write_model_eval_orchestration_route_contract(
    contract: &ModelEvalOrchestrationRouteContractV1,
    output_path: &Path,
) -> Result<()> {
    let parent = output_path
        .parent()
        .context("route contract output has no parent")?;
    let parent = super::canonical_regular_directory(parent)?;
    let leaf = output_path
        .file_name()
        .context("route contract output has no file name")?;
    let destination = parent.join(leaf);
    let rendered = toml::to_string_pretty(contract)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    output.write_all(rendered.as_bytes())?;
    output.sync_all()?;
    sync_directory(&parent)?;
    Ok(())
}

fn load_complete_orchestration_corpus(roots: &[PathBuf]) -> Result<Vec<LoadedModelEvalFixture>> {
    let mut fixtures = roots
        .iter()
        .map(load_model_eval_fixture)
        .collect::<Result<Vec<_>>>()?;
    fixtures.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
    if fixtures
        .windows(2)
        .any(|pair| pair[0].manifest.id == pair[1].manifest.id)
    {
        bail!("route-contract corpus contains duplicate fixture ids");
    }
    let negative = fixtures
        .iter()
        .filter(|fixture| {
            fixture
                .manifest
                .orchestration
                .as_ref()
                .is_some_and(|case| case.case_class == OrchestrationEvalCaseClass::Negative)
        })
        .count();
    let positive = fixtures
        .iter()
        .filter(|fixture| {
            fixture
                .manifest
                .orchestration
                .as_ref()
                .is_some_and(|case| case.case_class == OrchestrationEvalCaseClass::Positive)
        })
        .count();
    if negative != ORCHESTRATION_EVAL_MIN_NEGATIVE_CASES
        || positive != ORCHESTRATION_EVAL_MIN_POSITIVE_CASES
        || fixtures
            .iter()
            .any(|fixture| fixture.manifest.orchestration.is_none())
    {
        bail!("route-contract derivation requires the complete frozen orchestration corpus");
    }
    let corpus_versions = fixtures
        .iter()
        .filter_map(|fixture| fixture.manifest.orchestration.as_ref())
        .map(|case| case.corpus_version.as_str())
        .collect::<BTreeSet<_>>();
    if corpus_versions.len() != 1 {
        bail!("route-contract corpus must use one exact version");
    }
    Ok(fixtures)
}

fn tool_profile_contract_digest(
    config: &RootConfig,
    base_registry: &ToolRegistry,
    profile_registry: &AgentProfileRegistry,
    fixtures: &[LoadedModelEvalFixture],
) -> Result<String> {
    let mut scopes = BTreeMap::<String, ToolRegistryScope>::new();
    for fixture in fixtures {
        let scope = ToolRegistryScope::from_names_and_prefixes(
            fixture.manifest.allowed_tools.iter().cloned(),
            std::iter::empty::<String>(),
        );
        let key = serde_json::to_string(&scope)?;
        scopes.entry(key).or_insert(scope);
    }
    let variants = scopes
        .into_values()
        .map(|scope| {
            let scoped = constrain_application_tool_registry(base_registry.clone(), &scope)?;
            let planner =
                build_role_tool_registry(&scoped, config, AgentRole::Planner).into_registry();
            let planner = planner_tools_with_discovery(
                &planner,
                config
                    .task
                    .max_planning_research_agents
                    .min(MAX_TASK_DISCOVERY_PROBES),
            );
            Ok(json!({
                "scope": scope,
                "conversation": sorted_specs_with(
                    scoped.specs(),
                    [request_task_planning_tool_spec()]
                ),
                "planner": sorted_specs_with(
                    planner.specs(),
                    [task_plan_update_tool_spec()]
                ),
                "executor": sorted_specs(
                    build_role_tool_registry(&scoped, config, AgentRole::Executor)
                        .specs()
                ),
                "subagent_read": sorted_specs(
                    build_role_tool_registry(&scoped, config, AgentRole::SubagentRead)
                        .specs()
                ),
                "subagent_write": sorted_specs(
                    build_role_tool_registry(&scoped, config, AgentRole::SubagentWrite)
                        .specs()
                ),
                "synthesis": Vec::<ToolSpec>::new(),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let profiles = profile_registry
        .profiles()
        .iter()
        .map(|resolved| {
            json!({
                "profile": &resolved.profile,
                "source_hash": &resolved.source_hash,
                "source": &resolved.source,
                "execution_role": resolved.execution_role,
                "enabled": resolved.effective_enabled(),
                "user_invocable": resolved.effective_user_invocation_allowed(),
                "model_invocable": resolved.effective_model_invocation_allowed(),
                "trust_state": resolved.trust_state,
            })
        })
        .collect::<Vec<_>>();
    digest_value(
        b"sigil-orchestration-tool-profile-contract-v1\0",
        &json!({
            "variants": variants,
            "profiles": profiles,
        }),
    )
}

fn sorted_specs(mut specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
    specs.sort_by(|left, right| left.name.cmp(&right.name));
    specs
}

fn sorted_specs_with(
    mut specs: Vec<ToolSpec>,
    additions: impl IntoIterator<Item = ToolSpec>,
) -> Vec<ToolSpec> {
    specs.extend(additions);
    sorted_specs(specs)
}

fn digest_value(domain: &[u8], value: &Value) -> Result<String> {
    Ok(digest_bytes(domain, &serde_json::to_vec(value)?))
}

fn digest_bytes(domain: &[u8], bytes: &[u8]) -> String {
    let mut material = Vec::with_capacity(domain.len() + bytes.len());
    material.extend_from_slice(domain);
    material.extend_from_slice(bytes);
    sha256_digest(&material)
}

fn candidate_build_commit() -> Result<String> {
    let (_, commit) = ORCHESTRATION_RUNTIME_BUILD_ID
        .rsplit_once('+')
        .context("candidate runtime build id has no commit")?;
    if commit.trim().is_empty() || commit == "unknown" {
        bail!("candidate runtime build metadata has no usable commit");
    }
    Ok(commit.to_owned())
}
