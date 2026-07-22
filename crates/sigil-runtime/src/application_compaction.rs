//! Shared application-facing preview and apply contract for portable context compaction.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    CompactionInitiation, ContinuationModelOutputV1, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    ExtensionProcessNetworkAdmission, FrozenProviderRequestMaterial, InputTokenEvidence,
    InteractionMode, JsonlSessionStore, MutationEventRecorder, PortableSemanticCompactionPreflight,
    PortableSemanticCompactionRequest, PortableTargetRequestMaterial, RootConfig,
    RuntimeContextCandidates, Session, SessionLogEntry, ToolOutputProjectionPolicy,
    build_workspace_snapshot, resolve_workspace_root, stable_event_uuid, stable_workspace_id,
    workspace_trust_from_entries,
};

/// Exact economics rendered before the user confirms a portable compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionEconomics {
    pub before_input_tokens: u64,
    pub target_input_tokens: u64,
    pub context_window_tokens: u64,
    pub output_tokens: u64,
    pub safety_buffer_tokens: u64,
    pub savings_tokens: u64,
    pub savings_ratio_ppm: u32,
    pub minimum_savings_tokens: u64,
    pub minimum_savings_ratio_ppm: u32,
}

/// Admission result of one read-only application compaction preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ApplicationCompactionAdmission {
    Ready {
        economics: ApplicationCompactionEconomics,
    },
    NoFoldableHistory {
        durable_message_count: usize,
        configured_tail_message_count: usize,
    },
    Unavailable {
        reason: String,
    },
}

/// Safe, bounded preview shown before a user confirms portable compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionReview {
    pub preview_id: Option<String>,
    pub folded_event_count: usize,
    pub retained_event_count: usize,
    pub admission: ApplicationCompactionAdmission,
}

/// Durable receipt returned after a successfully applied portable compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionReceipt {
    pub compaction_id: String,
    pub attempt_id: String,
    pub task_memory_id: String,
    pub folded_event_count: usize,
    pub tool_output_projection_recorded: bool,
}

/// Exact process-local material retained between preview and explicit apply.
///
/// The frozen provider request is deliberately neither serializable nor cloneable. A process
/// restart invalidates an unapplied preview; an already completed apply remains replayable through
/// the adapter's durable command receipt.
pub struct PendingApplicationCompaction {
    preview_id: String,
    session_scope_id: String,
    preflight: PortableSemanticCompactionPreflight,
    target_material: PortableTargetRequestMaterial,
    folded_event_count: usize,
}

impl std::fmt::Debug for PendingApplicationCompaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingApplicationCompaction")
            .field("preview_id", &self.preview_id)
            .field("session_scope_id", &"[bound]")
            .field("folded_event_count", &self.folded_event_count)
            .finish_non_exhaustive()
    }
}

impl PendingApplicationCompaction {
    #[must_use]
    pub fn preview_id(&self) -> &str {
        &self.preview_id
    }

    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    /// Applies this one exact preview under the kernel's writer-lock stale-frontier CAS.
    ///
    /// # Errors
    ///
    /// Returns an error when the preview or durable scope differs, the stream changed, or the
    /// frozen target proof is no longer admissible.
    pub fn apply(
        self,
        session_path: &Path,
        expected_session_scope_id: &str,
        expected_preview_id: &str,
    ) -> Result<ApplicationCompactionReceipt> {
        if self.session_scope_id != expected_session_scope_id {
            bail!("reviewed application compaction belongs to a different session scope");
        }
        if self.preview_id != expected_preview_id {
            bail!("application compaction preview binding is stale");
        }
        let outcome = JsonlSessionStore::new(session_path)?
            .execute_portable_semantic_compaction(self.preflight, self.target_material)?;
        Ok(ApplicationCompactionReceipt {
            compaction_id: outcome.compaction_id,
            attempt_id: outcome.attempt_id,
            task_memory_id: outcome.task_memory_id,
            folded_event_count: self.folded_event_count,
            tool_output_projection_recorded: outcome.tool_output_projection_recorded,
        })
    }
}

/// Builds a read-only portable compaction review and retains its exact process-local target.
///
/// No compaction lifecycle entry is appended here. Capability/tokenizer failures are returned as
/// a renderable `Unavailable` admission, while malformed configuration or durable truth remains a
/// hard error.
///
/// # Errors
///
/// Returns an error when configuration, durable session identity, workspace snapshot, provider,
/// or tool-surface assembly cannot be validated.
pub async fn prepare_application_compaction(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<(
    ApplicationCompactionReview,
    Option<PendingApplicationCompaction>,
)> {
    let mut root_config = RootConfig::load(config_path)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let store = JsonlSessionStore::new(session_path)?;
    let mutation_recorder = MutationEventRecorder::new(store.clone());
    let session = Session::load_from_store(
        root_config.agent.provider.clone(),
        root_config.agent.model.clone(),
        store,
    )?;
    if session.session_scope_id() != expected_session_scope_id {
        bail!("application compaction session scope mismatch");
    }
    root_config.agent.provider = session.provider_name().to_owned();
    root_config.agent.model = session.model_name().to_owned();
    let effective_config = crate::effective_compaction_config(
        session.provider_name(),
        session.model_name(),
        &root_config.compaction,
    );
    if !effective_config.enabled {
        return Ok((unavailable_review("context compaction is disabled"), None));
    }
    let Some(preview) = session.v2_compaction_preview(effective_config.tail_messages)? else {
        return Ok((
            ApplicationCompactionReview {
                preview_id: None,
                folded_event_count: 0,
                retained_event_count: durable_message_count(session.entries()),
                admission: ApplicationCompactionAdmission::NoFoldableHistory {
                    durable_message_count: durable_message_count(session.entries()),
                    configured_tail_message_count: effective_config.tail_messages,
                },
            },
            None,
        ));
    };

    let folded_event_count = preview.plan.folded_event_ids.len();
    let retained_event_count = preview.plan.retained_event_ids.len();
    let preview_id = format!(
        "compact-{}",
        stable_event_uuid(
            "sigil-application-compaction-preview",
            &format!(
                "{}:{}:{}",
                expected_session_scope_id,
                preview.plan.base_stream_cursor.last_applied_event_id,
                uuid::Uuid::new_v4()
            ),
        )
    );

    let provider = crate::build_provider(&root_config)?;
    let workspace_trust = workspace_trust_from_entries(session.entries(), &workspace_root)?;
    let options = crate::build_run_options(
        &root_config,
        workspace_root.clone(),
        InteractionMode::Interactive,
    );
    let surface =
        crate::build_tool_surface_with_mutation_recorder_and_workspace_trust_and_network_admission(
            &root_config,
            &provider.capabilities(),
            workspace_root.clone(),
            mutation_recorder,
            workspace_trust,
            ExtensionProcessNetworkAdmission::new(options.permission_context.network_policy, false),
        )
        .await?;
    let runtime_context =
        resolve_session_request_context(&session, &surface.context_resolver).await;

    let prepared = prepare_exact_application_compaction(
        &preview_id,
        &root_config,
        &workspace_root,
        session_path,
        &session,
        &options.memory_config,
        options.reasoning_effort,
        options.traffic_partition_key,
        surface.registry.specs(),
        runtime_context,
        preview,
    );
    let (preflight, target_material) = match prepared {
        Ok(material) => material,
        Err(error) => {
            return Ok((
                ApplicationCompactionReview {
                    preview_id: None,
                    folded_event_count,
                    retained_event_count,
                    admission: ApplicationCompactionAdmission::Unavailable {
                        reason: format!(
                            "exact portable compaction proof is unavailable: {error:#}"
                        ),
                    },
                },
                None,
            ));
        }
    };
    let economics = target_material
        .portable_economics()
        .context("portable target material has no before/after economics proof")?;
    let proof = target_material.proof();
    let target_input_tokens = match &proof.input {
        InputTokenEvidence::Exact { tokens, .. } => *tokens,
        InputTokenEvidence::ConservativeUpperBound { .. } => {
            return Ok((
                ApplicationCompactionReview {
                    preview_id: None,
                    folded_event_count,
                    retained_event_count,
                    admission: ApplicationCompactionAdmission::Unavailable {
                        reason: "local exact target proof is unavailable".to_owned(),
                    },
                },
                None,
            ));
        }
    };
    let review = ApplicationCompactionReview {
        preview_id: Some(preview_id.clone()),
        folded_event_count,
        retained_event_count,
        admission: ApplicationCompactionAdmission::Ready {
            economics: ApplicationCompactionEconomics {
                before_input_tokens: economics.before_input.admission_tokens(),
                target_input_tokens,
                context_window_tokens: proof.budget.context_window_tokens,
                output_tokens: proof.budget.requested_output_tokens,
                safety_buffer_tokens: proof.budget.safety_buffer_tokens,
                savings_tokens: economics.savings_tokens,
                savings_ratio_ppm: economics.savings_ratio_ppm,
                minimum_savings_tokens: economics.minimum_savings_tokens,
                minimum_savings_ratio_ppm: economics.minimum_savings_ratio_ppm,
            },
        },
    };
    Ok((
        review,
        Some(PendingApplicationCompaction {
            preview_id,
            session_scope_id: expected_session_scope_id.to_owned(),
            preflight,
            target_material,
            folded_event_count,
        }),
    ))
}

#[allow(clippy::too_many_arguments)]
fn prepare_exact_application_compaction(
    preview_id: &str,
    root_config: &RootConfig,
    workspace_root: &Path,
    session_path: &Path,
    session: &Session,
    memory_config: &sigil_kernel::MemoryConfig,
    reasoning_effort: Option<sigil_kernel::ReasoningEffort>,
    traffic_partition_key: Option<String>,
    tools: Vec<sigil_kernel::ToolSpec>,
    runtime_context: RuntimeContextCandidates,
    preview: sigil_kernel::V2CompactionPreview,
) -> Result<(
    PortableSemanticCompactionPreflight,
    PortableTargetRequestMaterial,
)> {
    if crate::is_deepseek_v4_flash_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    ) {
        crate::require_default_deepseek_v4_flash_portable_transport(root_config)?;
    }
    let workspace_id = stable_workspace_id(workspace_root)?;
    let scope = root_config
        .verification
        .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let snapshot = build_workspace_snapshot(workspace_root, workspace_id, &scope, 0)?;
    let valid_for_snapshot = snapshot
        .workspace_snapshot_id
        .context("portable compaction requires a complete workspace snapshot")?;
    let now = crate::current_unix_time_ms();
    let source_key = format!(
        "{}:{}:application-manual:{preview_id}",
        session.session_scope_id(),
        preview.plan.base_stream_cursor.last_applied_event_id,
    );
    let attempt_id = format!(
        "portable-{}",
        stable_event_uuid("sigil-portable-compaction-attempt", &source_key)
    );
    let compaction_id = format!(
        "portable-{}",
        stable_event_uuid("sigil-portable-compaction-activation", &source_key)
    );
    let request = PortableSemanticCompactionRequest {
        attempt_id,
        compaction_id,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "portable-v2-admission-r1".to_owned(),
        branch_id: None,
        valid_for_snapshot,
        objective: None,
        language: "en".to_owned(),
        plan: preview.plan,
        model_output: ContinuationModelOutputV1 {
            in_progress: Vec::new(),
            pending_actions: Vec::new(),
            provider_continuity: Vec::new(),
            model_notes: Vec::new(),
        },
        tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
        started_at_unix_ms: now,
        completed_at_unix_ms: now,
    };
    let store = JsonlSessionStore::new(session_path)?;
    let preflight = store.prepare_portable_semantic_compaction(request)?;
    let target_max_tokens = crate::portable_compaction_target_output_tokens(
        session.provider_name(),
        session.model_name(),
    );
    let previous_response_handle = session.latest_response_handle(session.provider_name());
    let before_request = session.build_pre_turn_candidate_request(
        workspace_root,
        memory_config,
        tools.clone(),
        target_max_tokens,
        reasoning_effort.clone(),
        previous_response_handle.clone(),
        traffic_partition_key.clone(),
        &[],
        runtime_context.clone(),
        &[],
    )?;
    let frozen_before_request =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), before_request)?;
    let target_request = session.build_portable_compaction_candidate_request(
        workspace_root,
        memory_config,
        preflight.checkpoint(),
        preflight.task_memory(),
        preflight.candidate_messages().to_vec(),
        tools,
        target_max_tokens,
        reasoning_effort,
        previous_response_handle,
        traffic_partition_key,
        &[],
        runtime_context,
        &[],
    )?;
    let frozen_target_request =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), target_request)?;
    let paths =
        crate::resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    let target_material = crate::deepseek_v4_flash_portable_target_material_with_economics(
        &paths.cache_root,
        &frozen_before_request,
        frozen_target_request,
    )?;
    Ok((preflight, target_material))
}

async fn resolve_session_request_context(
    session: &Session,
    context_resolver: &crate::RequestContextResolver,
) -> RuntimeContextCandidates {
    let query = session.messages().into_iter().rev().find_map(|message| {
        matches!(message.role, sigil_kernel::MessageRole::User)
            .then_some(message.content)
            .flatten()
            .filter(|content| !content.trim().is_empty())
    });
    match query {
        Some(query) => context_resolver.resolve(&query).await.unwrap_or_default(),
        None => RuntimeContextCandidates::default(),
    }
}

fn durable_message_count(entries: &[SessionLogEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::User(_)
                    | SessionLogEntry::Assistant(_)
                    | SessionLogEntry::ToolResult(_)
            )
        })
        .count()
}

fn unavailable_review(reason: impl Into<String>) -> ApplicationCompactionReview {
    ApplicationCompactionReview {
        preview_id: None,
        folded_event_count: 0,
        retained_event_count: 0,
        admission: ApplicationCompactionAdmission::Unavailable {
            reason: reason.into(),
        },
    }
}

#[cfg(test)]
#[path = "tests/application_compaction_tests.rs"]
mod tests;
