//! Shared application-facing preview and apply contract for portable context compaction.

use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    CompactionAdmissionReasonV2, CompactionForecastConfidenceV1, CompactionForecastSourceV1,
    CompactionInitiation, CompactionPressureStateV1, CompactionRolloutModeV1, CompactionStrategy,
    ControlEntry, DEFAULT_TAIL_MIN_COMPLETE_TURNS, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    ExpectedRemainingTurnsV1, ExtensionProcessNetworkAdmission, FrozenProviderRequestMaterial,
    InputTokenEvidence, InteractionMode, JsonlSessionStore, MutationEventRecorder,
    PortableSemanticCompactionPreflight, PortableSemanticCompactionRequest,
    PortableTargetRequestMaterial, RootConfig, RuntimeContextCandidates, SecretRedactor, Session,
    SessionLogEntry, ToolOutputProjectionPolicy, build_workspace_snapshot, resolve_workspace_root,
    stable_event_uuid, stable_workspace_id, workspace_trust_from_entries,
};

/// Provider-native compaction remains fail-closed until the resulting carrier is consumed by the
/// next request on the exact same route and source cursor.
///
/// Keeping this false prevents an opt-in configuration value from creating an additional billed
/// request whose opaque result would not yet reduce any later request.
pub const NATIVE_COMPACTION_RESUME_ENABLED: bool = false;

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
    pub summary_cache_read_tokens: u64,
    pub summary_uncached_input_tokens: u64,
    pub summary_output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_cost_nano_usd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub economics_v2: Option<sigil_kernel::CompactionEconomicsV2>,
}

/// Admission result of one non-activating application compaction review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ApplicationCompactionAdmission {
    Prepared {
        standalone_tool_output_shrink_available: bool,
    },
    Ready {
        economics: Box<ApplicationCompactionEconomics>,
    },
    NoFoldableHistory {
        durable_message_count: usize,
        minimum_tail_turn_count: usize,
    },
    Unavailable {
        reason: String,
    },
}

/// User-facing policy evidence shared by TUI, serve, and Desktop compaction previews.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionPolicyView {
    pub strategy: CompactionStrategy,
    pub phase: CompactionPressureStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forecast_confidence: Option<CompactionForecastConfidenceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_reason: Option<CompactionAdmissionReasonV2>,
    pub native_carrier_available: bool,
}

/// One exact active constraint shown before compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionConstraintView {
    pub text: String,
    pub source_event_id: String,
    pub source_field_path: String,
}

/// One bounded, recoverable historical tool-output candidate shown before compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionToolArtifactView {
    pub source_event_id: String,
    pub content_sha256: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub status: String,
    pub original_content_bytes: u64,
    pub original_content_token_upper_bound: u64,
    pub head_excerpt: String,
    pub tail_excerpt: String,
    pub reason: sigil_kernel::ToolOutputShrinkReasonV1,
    pub recovery_instruction: String,
}

/// Bounded continuity, tail, cache and protection facts shared by every graphical surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionDetailsView {
    pub active_objective: String,
    pub objective_source_event_id: String,
    pub active_constraints: Vec<ApplicationCompactionConstraintView>,
    pub folded_complete_turn_count: usize,
    pub folded_token_upper_bound: u64,
    pub retained_complete_turn_count: usize,
    pub retained_token_upper_bound: u64,
    pub tool_artifact_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_artifacts: Vec<ApplicationCompactionToolArtifactView>,
    pub pending_work_count: usize,
    pub unresolved_question_count: usize,
    pub recoverable_attachment_count: usize,
    pub protected_control_event_count: usize,
    pub protected_active_tool_or_approval_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_even_turns: Option<u32>,
}

/// Safe, bounded preview shown before a user confirms portable compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationCompactionReview {
    pub preview_id: Option<String>,
    pub folded_event_count: usize,
    pub retained_event_count: usize,
    pub policy: ApplicationCompactionPolicyView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<ApplicationCompactionDetailsView>>,
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
    pub native_carrier_materialized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_carrier_status: Option<String>,
}

/// Receipt for a local-only large tool-output projection epoch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationToolOutputShrinkReceipt {
    pub context_epoch_id: String,
    pub projected_output_count: usize,
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
    native_carrier: Option<PendingApplicationNativeCarrier>,
    session_attachment:
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
}

/// Exact local-only compaction plan retained before the user authorizes a billed summary call.
#[derive(Debug)]
pub struct PendingApplicationCompactionPreview {
    preview_id: String,
    session_scope_id: String,
    preview: sigil_kernel::V2CompactionPreview,
    session_attachment:
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
}

impl PendingApplicationCompactionPreview {
    #[must_use]
    pub fn preview_id(&self) -> &str {
        &self.preview_id
    }

    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    /// Applies only the deterministic recoverable tool-output projection for this exact plan.
    pub fn apply_standalone_tool_output_shrink(
        self,
        session_path: &Path,
        expected_session_scope_id: &str,
        expected_preview_id: &str,
    ) -> Result<ApplicationToolOutputShrinkReceipt> {
        if self.session_scope_id != expected_session_scope_id
            || self.preview_id != expected_preview_id
        {
            bail!("local application compaction preview binding is stale");
        }
        let store = JsonlSessionStore::new(session_path)?;
        anyhow::ensure!(
            self.session_attachment.session_path() == store.path(),
            "local application compaction attachment belongs to another durable session"
        );
        let active = store.active_projection_snapshot()?;
        let pressure = active.tool_output_pressure();
        let batch = sigil_kernel::ToolOutputAgingBatchV1::select(
            &pressure,
            sigil_kernel::ToolOutputAgingReasonV1::Manual,
        )?
        .context("no new large historical tool outputs are eligible")?;
        let activation = sigil_kernel::ToolOutputAgingActivatedV1::prepare(&pressure, &batch)?;
        let projected_output_count = activation.replacements.len();
        let context_epoch_id = activation.target_epoch_id.clone();
        store
            .append_tool_output_aging_activation(active.frontier(), activation)?
            .context("no standalone tool-output projection was appended")?;
        Ok(ApplicationToolOutputShrinkReceipt {
            context_epoch_id,
            projected_output_count,
        })
    }
}

struct PendingApplicationNativeCarrier {
    provider: Box<dyn sigil_kernel::Provider>,
    session: Session,
    frozen_request: FrozenProviderRequestMaterial,
    covers_through: sigil_kernel::CompactionCursor,
    portable_compaction_id: sigil_kernel::CompactionId,
}

struct ApplicationNativeCarrierSource {
    frozen_request: FrozenProviderRequestMaterial,
    covers_through: sigil_kernel::CompactionCursor,
    portable_compaction_id: sigil_kernel::CompactionId,
    summary_cache_read_tokens: u64,
    summary_uncached_input_tokens: u64,
    summary_output_tokens: u64,
    summary_cost_nano_usd: Option<u64>,
}

impl std::fmt::Debug for PendingApplicationCompaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingApplicationCompaction")
            .field("preview_id", &self.preview_id)
            .field("session_scope_id", &"[bound]")
            .field("folded_event_count", &self.folded_event_count)
            .field("has_native_carrier", &self.native_carrier.is_some())
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
        self.apply_portable(session_path, expected_session_scope_id, expected_preview_id)
            .map(|(receipt, _)| receipt)
    }

    /// Applies portable truth and then, only when explicitly enabled during preview, attempts the
    /// provider-native acceleration dual-write.
    pub async fn apply_with_optional_native(
        self,
        session_path: &Path,
        expected_session_scope_id: &str,
        expected_preview_id: &str,
    ) -> Result<ApplicationCompactionReceipt> {
        let _route_execution_owner = self
            .session_attachment
            .route_mutation_authority(&self.session_scope_id)?
            .acquire_execution_owner()
            .map_err(anyhow::Error::new)?;
        let (mut receipt, native_carrier) =
            self.apply_portable(session_path, expected_session_scope_id, expected_preview_id)?;
        if !NATIVE_COMPACTION_RESUME_ENABLED {
            return Ok(receipt);
        }
        let Some(native_carrier) = native_carrier else {
            return Ok(receipt);
        };
        let logical_run_id = format!("native-carrier:{}", receipt.compaction_id);
        match crate::materialize_native_compaction_carrier(
            native_carrier.provider.as_ref(),
            &native_carrier.session,
            logical_run_id,
            native_carrier.frozen_request,
            native_carrier.covers_through,
            native_carrier.portable_compaction_id,
        )
        .await
        {
            Ok(Some(_materialized)) => {
                receipt.native_carrier_materialized = true;
                receipt.native_carrier_status = Some("native_carrier_materialized".to_owned());
            }
            Ok(None) => {
                receipt.native_carrier_status = Some("native_carrier_not_produced".to_owned());
            }
            Err(_error) => {
                receipt.native_carrier_status = Some("native_carrier_unavailable".to_owned());
            }
        }
        Ok(receipt)
    }

    fn apply_portable(
        self,
        session_path: &Path,
        expected_session_scope_id: &str,
        expected_preview_id: &str,
    ) -> Result<(
        ApplicationCompactionReceipt,
        Option<PendingApplicationNativeCarrier>,
    )> {
        if self.session_scope_id != expected_session_scope_id {
            bail!("reviewed application compaction belongs to a different session scope");
        }
        if self.preview_id != expected_preview_id {
            bail!("application compaction preview binding is stale");
        }
        let store = JsonlSessionStore::new(session_path)?;
        anyhow::ensure!(
            self.session_attachment.session_path() == store.path(),
            "reviewed application compaction attachment belongs to another durable session"
        );
        let outcome =
            store.execute_portable_semantic_compaction(self.preflight, self.target_material)?;
        Ok((
            ApplicationCompactionReceipt {
                compaction_id: outcome.compaction_id,
                attempt_id: outcome.attempt_id,
                task_memory_id: outcome.task_memory_id,
                folded_event_count: self.folded_event_count,
                tool_output_projection_recorded: outcome.tool_output_projection_recorded,
                native_carrier_materialized: false,
                native_carrier_status: None,
            },
            self.native_carrier,
        ))
    }
}

/// Builds the local-only compaction review required before any semantic-summary provider call.
///
/// This performs local filesystem/projection work only. It creates a deterministic authority and
/// continuity baseline with empty model-owned sections so every surface can render the exact
/// fold, tail and recoverable tool-output candidates before the user chooses whether to spend.
pub fn preview_application_compaction(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<(
    ApplicationCompactionReview,
    Option<PendingApplicationCompactionPreview>,
)> {
    let attachment = Arc::new(
        crate::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            session_path,
        )?,
    );
    preview_application_compaction_with_attachment(
        config_path,
        launch_cwd,
        session_path,
        expected_session_scope_id,
        attachment,
    )
}

/// Builds a local compaction review under a controller-owned session attachment.
///
/// # Errors
///
/// Returns an error when the supplied attachment does not belong to the session or when the
/// durable route, workspace snapshot, or compaction projection cannot be validated.
pub fn preview_application_compaction_with_attachment(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    attachment: Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
) -> Result<(
    ApplicationCompactionReview,
    Option<PendingApplicationCompactionPreview>,
)> {
    let root_config = RootConfig::load(config_path)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let store = JsonlSessionStore::new(session_path)?;
    anyhow::ensure!(
        attachment.session_path() == store.path(),
        "supplied session attachment belongs to another durable session"
    );
    let (session, exact_model_ref) =
        load_application_compaction_session(&root_config, store.clone(), Some(&attachment))?;
    if session.session_scope_id() != expected_session_scope_id {
        bail!("application compaction session scope mismatch");
    }
    let effective_config = crate::effective_compaction_config_for_model_ref(
        &root_config,
        &exact_model_ref,
        session.provider_name(),
    );
    if !effective_config.enabled {
        return Ok((
            unavailable_review(&effective_config, false, "context compaction is disabled"),
            None,
        ));
    }
    let Some(preview) =
        crate::context_window::compaction_preview_for_strategy(&session, &effective_config)?
    else {
        return Ok((
            ApplicationCompactionReview {
                preview_id: None,
                folded_event_count: 0,
                retained_event_count: durable_message_count(session.entries()),
                policy: compaction_policy_view(&effective_config, None, false),
                details: None,
                admission: ApplicationCompactionAdmission::NoFoldableHistory {
                    durable_message_count: durable_message_count(session.entries()),
                    minimum_tail_turn_count: DEFAULT_TAIL_MIN_COMPLETE_TURNS,
                },
            },
            None,
        ));
    };
    let preview_id = format!(
        "compact-{}",
        stable_event_uuid(
            "sigil-application-local-compaction-preview",
            &format!(
                "{}:{}:{}",
                expected_session_scope_id,
                preview.plan.base_stream_cursor.last_applied_event_id,
                uuid::Uuid::new_v4()
            ),
        )
    );
    let workspace_id = stable_workspace_id(&workspace_root)?;
    let scope = root_config
        .verification
        .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let snapshot = build_workspace_snapshot(&workspace_root, workspace_id, &scope, 0)?;
    let valid_for_snapshot = snapshot
        .workspace_snapshot_id
        .context("portable compaction requires a complete workspace snapshot")?;
    let now = crate::current_unix_time_ms();
    let local_preflight =
        store.prepare_portable_semantic_compaction(PortableSemanticCompactionRequest {
            attempt_id: format!("local-preview-attempt:{preview_id}"),
            compaction_id: format!("local-preview:{preview_id}"),
            initiation: CompactionInitiation::Manual,
            base_projection_revision: "portable-v3-local-preview-r1".to_owned(),
            branch_id: None,
            valid_for_snapshot,
            objective: None,
            language: "en".to_owned(),
            plan: preview.plan.clone(),
            model_output: sigil_kernel::ContinuationModelOutputV1 {
                in_progress: Vec::new(),
                pending_actions: Vec::new(),
                provider_continuity: Vec::new(),
                model_notes: Vec::new(),
            },
            tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
            started_at_unix_ms: now,
            completed_at_unix_ms: now,
        })?;
    let pressure = store.active_projection_snapshot()?.tool_output_pressure();
    let tool_artifacts = compaction_tool_artifact_views(
        &pressure,
        &crate::secret_redactor_for_root_config(&root_config),
    )?;
    let tool_artifact_count = tool_artifacts.len();
    let review = ApplicationCompactionReview {
        preview_id: Some(preview_id.clone()),
        folded_event_count: preview.plan.folded_event_ids.len(),
        retained_event_count: preview.plan.retained_event_ids.len(),
        policy: compaction_policy_view(&effective_config, None, false),
        details: Some(Box::new(compaction_details_view(
            &preview.plan,
            local_preflight.checkpoint(),
            tool_artifacts,
            None,
            latest_cache_read_tokens(&session),
        ))),
        admission: ApplicationCompactionAdmission::Prepared {
            standalone_tool_output_shrink_available: tool_artifact_count > 0,
        },
    };
    Ok((
        review,
        Some(PendingApplicationCompactionPreview {
            preview_id,
            session_scope_id: expected_session_scope_id.to_owned(),
            preview,
            session_attachment: attachment,
        }),
    ))
}

/// Builds a portable compaction review and retains its exact process-local target.
///
/// Cache-aware V3 issues one audited semantic-summary provider attempt before the review. No
/// compaction activation lifecycle is appended here. Capability/tokenizer failures are returned
/// as a renderable `Unavailable` admission, while malformed configuration or durable truth
/// remains a hard error.
///
/// # Errors
///
/// Returns an error when configuration, durable session identity, workspace snapshot, provider,
/// or tool-surface assembly cannot be validated.
#[cfg(test)]
async fn prepare_application_compaction(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<(
    ApplicationCompactionReview,
    Option<PendingApplicationCompaction>,
)> {
    prepare_application_compaction_for_preview(
        config_path,
        launch_cwd,
        session_path,
        expected_session_scope_id,
        None,
        Arc::new(
            crate::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                session_path,
            )?,
        ),
    )
    .await
}

/// Generates the billed semantic summary for one exact local preview.
pub async fn prepare_application_compaction_from_preview(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    preview: PendingApplicationCompactionPreview,
) -> Result<(
    ApplicationCompactionReview,
    Option<PendingApplicationCompaction>,
)> {
    let attachment = Arc::clone(&preview.session_attachment);
    prepare_application_compaction_from_preview_with_attachment(
        config_path,
        launch_cwd,
        session_path,
        expected_session_scope_id,
        preview,
        attachment,
    )
    .await
}

/// Generates one billed semantic summary under a controller-owned session attachment.
///
/// The attachment supplies the shared route authority. A live execution owner is retained for
/// the entire provider operation so a concurrent route mutation cannot cross the request.
///
/// # Errors
///
/// Returns an error when the preview binding, attachment, route, provider, or durable session
/// state cannot be validated.
pub async fn prepare_application_compaction_from_preview_with_attachment(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    preview: PendingApplicationCompactionPreview,
    attachment: Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
) -> Result<(
    ApplicationCompactionReview,
    Option<PendingApplicationCompaction>,
)> {
    if preview.session_scope_id != expected_session_scope_id {
        bail!("local application compaction preview belongs to a different session scope");
    }
    anyhow::ensure!(
        Arc::ptr_eq(&preview.session_attachment, &attachment),
        "local application compaction preview belongs to another attachment generation"
    );
    prepare_application_compaction_for_preview(
        config_path,
        launch_cwd,
        session_path,
        expected_session_scope_id,
        Some(preview),
        attachment,
    )
    .await
}

async fn prepare_application_compaction_for_preview(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    forced_preview: Option<PendingApplicationCompactionPreview>,
    attachment: Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
) -> Result<(
    ApplicationCompactionReview,
    Option<PendingApplicationCompaction>,
)> {
    let root_config = RootConfig::load(config_path)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let store = JsonlSessionStore::new(session_path)?;
    let mutation_recorder = MutationEventRecorder::new(store.clone());
    anyhow::ensure!(
        attachment.session_path() == store.path(),
        "supplied session attachment belongs to another durable session"
    );
    let (mut session, exact_model_ref) =
        load_application_compaction_session(&root_config, store, Some(&attachment))?;
    if session.session_scope_id() != expected_session_scope_id {
        bail!("application compaction session scope mismatch");
    }
    let _route_execution_owner = attachment
        .route_mutation_authority(session.session_scope_id())?
        .acquire_execution_owner()
        .map_err(anyhow::Error::new)?;
    let effective_config = crate::effective_compaction_config_for_model_ref(
        &root_config,
        &exact_model_ref,
        session.provider_name(),
    );
    if !effective_config.enabled {
        return Ok((
            unavailable_review(&effective_config, false, "context compaction is disabled"),
            None,
        ));
    }
    let Some(preview) =
        crate::context_window::compaction_preview_for_strategy(&session, &effective_config)?
    else {
        return Ok((
            ApplicationCompactionReview {
                preview_id: None,
                folded_event_count: 0,
                retained_event_count: durable_message_count(session.entries()),
                policy: compaction_policy_view(&effective_config, None, false),
                details: None,
                admission: ApplicationCompactionAdmission::NoFoldableHistory {
                    durable_message_count: durable_message_count(session.entries()),
                    minimum_tail_turn_count: DEFAULT_TAIL_MIN_COMPLETE_TURNS,
                },
            },
            None,
        ));
    };

    let folded_event_count = preview.plan.folded_event_ids.len();
    let retained_event_count = preview.plan.retained_event_ids.len();
    let preview_plan = preview.plan.clone();
    let preview_id = if let Some(forced_preview) = forced_preview {
        if forced_preview.preview != preview {
            bail!("local application compaction preview is stale");
        }
        forced_preview.preview_id
    } else {
        format!(
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
        )
    };

    if crate::is_deepseek_v4_flash_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    ) {
        crate::require_deepseek_v4_flash_portable_transport_for_model_ref(
            &root_config,
            &exact_model_ref,
        )?;
    }
    let provider = crate::build_provider_for_model_ref_async(&root_config, &exact_model_ref)
        .await
        .context("failed to build the exact durable compaction provider")?;
    let context_capabilities = provider.context_capabilities(session.model_name());
    let native_carrier_available = NATIVE_COMPACTION_RESUME_ENABLED
        && effective_config.native_carrier_enabled
        && context_capabilities.validate().is_ok()
        && context_capabilities.native_compaction.is_some();
    let workspace_trust = workspace_trust_from_entries(session.entries(), &workspace_root)?;
    let mut options = crate::build_run_options(
        &root_config,
        workspace_root.clone(),
        InteractionMode::Interactive,
        None,
    );
    options.compaction_config = effective_config.clone();
    let mut reasoning_config = root_config.clone();
    reasoning_config.agent.runtime_provider = session.provider_name().to_owned();
    reasoning_config.agent.model = session.model_name().to_owned();
    options.reasoning_effort =
        crate::reasoning_effort::configured_default_reasoning_effort(&reasoning_config);
    let surface =
        crate::build_tool_surface_with_mutation_recorder_and_workspace_trust_and_network_admission(
            &root_config,
            &provider.capabilities(),
            workspace_root.clone(),
            mutation_recorder,
            workspace_trust,
            ExtensionProcessNetworkAdmission::new(options.permission_context.network_policy, false),
        )
        .await
        .context("failed to build the application compaction tool surface")?;
    let runtime_context =
        resolve_session_request_context(&session, &surface.context_resolver).await;

    let prepared = prepare_exact_application_compaction(
        &preview_id,
        &root_config,
        &workspace_root,
        session_path,
        provider.as_ref(),
        &mut session,
        &options.memory_config,
        options.reasoning_effort,
        options.traffic_partition_key,
        surface.registry.specs(),
        runtime_context,
        preview,
    )
    .await;
    let (preflight, target_material, native_source) = match prepared {
        Ok(material) => material,
        Err(error) => {
            return Ok((
                ApplicationCompactionReview {
                    preview_id: None,
                    folded_event_count,
                    retained_event_count,
                    policy: compaction_policy_view(
                        &effective_config,
                        None,
                        native_carrier_available,
                    ),
                    details: None,
                    admission: ApplicationCompactionAdmission::Unavailable {
                        reason: safe_compaction_unavailable_reason(&error).to_owned(),
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
                    policy: compaction_policy_view(
                        &effective_config,
                        None,
                        native_carrier_available,
                    ),
                    details: None,
                    admission: ApplicationCompactionAdmission::Unavailable {
                        reason: "local exact target proof is unavailable".to_owned(),
                    },
                },
                None,
            ));
        }
    };
    let pressure = session
        .active_projection_snapshot()?
        .context("active tool-output pressure projection is unavailable")?
        .tool_output_pressure();
    let tool_artifacts = compaction_tool_artifact_views(
        &pressure,
        &crate::secret_redactor_for_root_config(&root_config),
    )?;
    let review = ApplicationCompactionReview {
        preview_id: Some(preview_id.clone()),
        folded_event_count,
        retained_event_count,
        policy: compaction_policy_view(
            &effective_config,
            economics.v2_economics.as_ref(),
            native_carrier_available,
        ),
        details: Some(Box::new(compaction_details_view(
            &preview_plan,
            preflight.checkpoint(),
            tool_artifacts,
            economics.v2_economics.as_ref(),
            latest_cache_read_tokens(&session),
        ))),
        admission: ApplicationCompactionAdmission::Ready {
            economics: Box::new(ApplicationCompactionEconomics {
                before_input_tokens: economics.before_input.admission_tokens(),
                target_input_tokens,
                context_window_tokens: proof.budget.context_window_tokens,
                output_tokens: proof.budget.requested_output_tokens,
                safety_buffer_tokens: proof.budget.safety_buffer_tokens,
                savings_tokens: economics.savings_tokens,
                savings_ratio_ppm: economics.savings_ratio_ppm,
                minimum_savings_tokens: economics.minimum_savings_tokens,
                minimum_savings_ratio_ppm: economics.minimum_savings_ratio_ppm,
                summary_cache_read_tokens: native_source.summary_cache_read_tokens,
                summary_uncached_input_tokens: native_source.summary_uncached_input_tokens,
                summary_output_tokens: native_source.summary_output_tokens,
                summary_cost_nano_usd: native_source.summary_cost_nano_usd,
                economics_v2: economics.v2_economics.clone(),
            }),
        },
    };
    let native_carrier = native_carrier_available.then_some(PendingApplicationNativeCarrier {
        provider,
        session,
        frozen_request: native_source.frozen_request,
        covers_through: native_source.covers_through,
        portable_compaction_id: native_source.portable_compaction_id,
    });
    Ok((
        review,
        Some(PendingApplicationCompaction {
            preview_id,
            session_scope_id: expected_session_scope_id.to_owned(),
            preflight,
            target_material,
            folded_event_count,
            native_carrier,
            session_attachment: attachment,
        }),
    ))
}

fn load_application_compaction_session(
    root_config: &RootConfig,
    store: JsonlSessionStore,
    attachment: Option<&crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
) -> Result<(Session, sigil_kernel::ModelRef)> {
    let (_, fallback_route) = crate::provider_connections::resolve_default_model_route(root_config)
        .map_err(anyhow::Error::new)
        .context("failed to resolve the configured fallback model route")?;
    let session = crate::application_run::load_application_session_for_route_with_attachment(
        root_config,
        &fallback_route,
        store,
        attachment,
    )
    .context("failed to load the exact durable compaction route")?;
    let model_ref = session
        .resolved_model_route()
        .map(|route| route.model_ref.clone())
        .context("session_route_missing")?;
    Ok((session, model_ref))
}

#[allow(clippy::too_many_arguments)]
async fn prepare_exact_application_compaction(
    preview_id: &str,
    root_config: &RootConfig,
    workspace_root: &Path,
    session_path: &Path,
    provider: &dyn sigil_kernel::Provider,
    session: &mut Session,
    memory_config: &sigil_kernel::MemoryConfig,
    reasoning_effort: Option<sigil_kernel::ReasoningEffort>,
    traffic_partition_key: Option<String>,
    tools: Vec<sigil_kernel::ToolSpec>,
    runtime_context: RuntimeContextCandidates,
    preview: sigil_kernel::V2CompactionPreview,
) -> Result<(
    PortableSemanticCompactionPreflight,
    PortableTargetRequestMaterial,
    ApplicationNativeCarrierSource,
)> {
    if !crate::is_deepseek_v4_flash_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    ) {
        bail!("route has no admitted normal portable compaction target profile");
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
    let next_turn_p95_tokens = preview
        .plan
        .adaptive_tail
        .recent_complete_turn_p95_tokens
        .max(1);
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
    let store = JsonlSessionStore::new(session_path)?;
    let target_max_tokens = Some(
        crate::portable_compaction_target_output_tokens(
            session.provider_name(),
            session.model_name(),
        )
        .context(
            "local exact target proof is unavailable: route has no admitted portable target profile",
        )?,
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
    let summary_result = crate::generate_portable_compaction_summary(
        provider,
        session,
        &store,
        &attempt_id,
        &frozen_before_request,
        &preview.plan,
        crate::SemanticCompactionFallbackPolicy::Forbid,
    )
    .await;
    let summary = match summary_result {
        Ok(summary) => summary,
        Err(error) => {
            crate::record_semantic_compaction_failure(
                &store,
                &attempt_id,
                CompactionInitiation::Manual,
                now,
                &error,
            )
            .context("failed to record semantic compaction failure")?;
            return Err(error).context("semantic compaction summary request failed");
        }
    };
    let crate::PortableCompactionSummary {
        model_output,
        usage: summary_usage,
        rebased_plan,
        deterministic_emergency_fallback: _,
    } = summary;
    let native_portable_compaction_id = compaction_id.clone();
    let native_covers_through = rebased_plan
        .folded_through
        .clone()
        .context("portable compaction plan has no folded-through cursor")?;
    let request = PortableSemanticCompactionRequest {
        attempt_id,
        compaction_id,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "portable-v3-hybrid-summary-r1".to_owned(),
        branch_id: None,
        valid_for_snapshot,
        objective: None,
        language: "en".to_owned(),
        plan: rebased_plan,
        model_output,
        tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
        started_at_unix_ms: now,
        completed_at_unix_ms: crate::current_unix_time_ms(),
    };
    let preflight = store.prepare_portable_semantic_compaction(request)?;
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
    let target_material =
        crate::deepseek_v4_flash_portable_target_material_with_economics_v2_candidate(
            &paths.cache_root,
            &frozen_before_request,
            frozen_target_request,
        )?;
    let latest_usage = session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage)) = entry else {
            return None;
        };
        Some(usage)
    });
    let cache_usage = latest_usage.and_then(|usage| usage.cache_usage.as_ref());
    let summary_cache_usage = summary_usage
        .as_ref()
        .and_then(|usage| usage.cache_usage.as_ref());
    let summary_cache_read_tokens = summary_cache_usage
        .and_then(|usage| usage.read.as_ref())
        .map_or_else(
            || {
                summary_usage
                    .as_ref()
                    .map_or(0, |usage| usage.cache_hit_tokens)
            },
            |count| count.tokens,
        );
    let summary_uncached_input_tokens = summary_cache_usage
        .and_then(|usage| usage.uncached.as_ref())
        .map_or_else(
            || {
                summary_usage
                    .as_ref()
                    .map_or(0, |usage| usage.cache_miss_tokens)
            },
            |count| count.tokens,
        );
    let summary_output_tokens = summary_usage
        .as_ref()
        .map_or(0, |usage| usage.completion_tokens);
    let bulky_shrink_candidate_tokens = session
        .active_projection_snapshot()?
        .map(|snapshot| snapshot.tool_output_pressure().reclaimable_tool_tokens)
        .unwrap_or(0);
    let target_material = crate::attach_portable_compaction_economics_v2(
        target_material,
        crate::PortableCompactionEconomicsV2Input {
            next_turn_p95_tokens,
            tool_growth_p95_tokens: 4_096,
            provider_state_tokens: 0,
            bulky_shrink_candidate_tokens,
            overflow_observed: false,
            expected_remaining_turns: ExpectedRemainingTurnsV1 {
                turns: 3,
                source: CompactionForecastSourceV1::ConservativeFallback,
                confidence: CompactionForecastConfidenceV1::Low,
                source_event_ids: Vec::new(),
            },
            observed_current_cache_read_tokens: cache_usage
                .and_then(|usage| usage.read.as_ref())
                .map(|count| count.tokens),
            observed_current_uncached_tokens: cache_usage
                .and_then(|usage| usage.uncached.as_ref())
                .map(|count| count.tokens),
            pricing_snapshot: summary_usage
                .as_ref()
                .filter(|usage| usage.prompt_tokens > 0 && usage.completion_tokens > 0)
                .and_then(|usage| usage.pricing_snapshot.clone())
                .or_else(|| {
                    summary_usage
                        .as_ref()
                        .is_some_and(|usage| usage.prompt_tokens > 0 && usage.completion_tokens > 0)
                        .then(|| latest_usage.and_then(|usage| usage.pricing_snapshot.clone()))
                        .flatten()
                }),
            compactor_usage_observed: summary_usage
                .as_ref()
                .is_some_and(|usage| usage.prompt_tokens > 0 && usage.completion_tokens > 0),
            compactor_cache_read_tokens: summary_cache_read_tokens,
            compactor_uncached_input_tokens: summary_uncached_input_tokens,
            compactor_output_tokens: summary_output_tokens,
            rollout_mode: CompactionRolloutModeV1::Preview,
            user_confirmed: true,
        },
    )?;
    let admission = target_material
        .portable_economics()
        .and_then(|economics| economics.v2_economics.as_ref())
        .map(|economics| &economics.admission)
        .context("application compaction has no RFC-0057 admission")?;
    if admission.decision != sigil_kernel::CompactionAdmissionDecisionV2::Admit
        || !admission.user_confirmed
        || admission.automatic_allowed
    {
        bail!(
            "application compaction is not confirmed by RFC-0057 admission: {:?} ({:?})",
            admission.decision,
            admission.reason
        );
    }
    let summary_cost_nano_usd = target_material
        .portable_economics()
        .and_then(|economics| economics.v2_economics.as_ref())
        .and_then(|economics| economics.cost_projection.as_ref())
        .map(|projection| projection.rotate_compactor_cost_nano_usd);
    Ok((
        preflight,
        target_material,
        ApplicationNativeCarrierSource {
            frozen_request: frozen_before_request,
            covers_through: native_covers_through,
            portable_compaction_id: native_portable_compaction_id,
            summary_cache_read_tokens,
            summary_uncached_input_tokens,
            summary_output_tokens,
            summary_cost_nano_usd,
        },
    ))
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
                    | SessionLogEntry::ToolResultV3(_)
            )
        })
        .count()
}

fn compaction_policy_view(
    config: &sigil_kernel::CompactionConfig,
    economics: Option<&sigil_kernel::CompactionEconomicsV2>,
    native_carrier_available: bool,
) -> ApplicationCompactionPolicyView {
    ApplicationCompactionPolicyView {
        strategy: config.strategy,
        phase: economics.map_or(CompactionPressureStateV1::BelowObserve, |economics| {
            economics.forecast.pressure_state
        }),
        forecast_confidence: economics
            .map(|economics| economics.forecast.input.expected_remaining_turns.confidence),
        admission_reason: economics.map(|economics| economics.admission.reason),
        native_carrier_available,
    }
}

fn compaction_details_view(
    plan: &sigil_kernel::CompactionFoldPlan,
    checkpoint: &sigil_kernel::ContinuationCheckpointV1,
    tool_artifacts: Vec<ApplicationCompactionToolArtifactView>,
    economics: Option<&sigil_kernel::CompactionEconomicsV2>,
    current_cache_read_tokens: Option<u64>,
) -> ApplicationCompactionDetailsView {
    let anchor = checkpoint
        .session_anchor
        .as_ref()
        .expect("portable V3 checkpoint always carries a session anchor");
    let continuity = checkpoint
        .continuity_v2
        .as_ref()
        .expect("portable V3 checkpoint always carries grounded continuity");
    let adaptive_tail = checkpoint
        .adaptive_tail
        .as_ref()
        .expect("portable V3 checkpoint always carries an adaptive tail proof");
    let active_constraints = anchor
        .constraints
        .iter()
        .chain(&anchor.authorization_boundary)
        .filter(|constraint| constraint.status == sigil_kernel::ConstraintStatusV1::Active)
        .map(|constraint| ApplicationCompactionConstraintView {
            text: constraint.exact_text.clone(),
            source_event_id: constraint.source.event_id.clone(),
            source_field_path: constraint.source.field_path.clone(),
        })
        .collect();
    let protected_control_event_count = plan
        .protected_events
        .iter()
        .filter(|event| event.reason == sigil_kernel::CompactionFoldProtectionReason::ControlState)
        .count();
    let protected_active_tool_or_approval_count = plan
        .protected_events
        .iter()
        .filter(|event| {
            event.reason == sigil_kernel::CompactionFoldProtectionReason::ActiveToolOrApproval
        })
        .count();

    ApplicationCompactionDetailsView {
        active_objective: anchor.root_objective.exact_text.clone(),
        objective_source_event_id: anchor.root_objective.source.event_id.clone(),
        active_constraints,
        folded_complete_turn_count: adaptive_tail.folded_complete_turns,
        folded_token_upper_bound: adaptive_tail.folded_token_upper_bound,
        retained_complete_turn_count: adaptive_tail.retained_complete_turns,
        retained_token_upper_bound: adaptive_tail.retained_token_upper_bound,
        tool_artifact_count: tool_artifacts.len(),
        tool_artifacts,
        pending_work_count: continuity.pending_work.len(),
        unresolved_question_count: continuity.unresolved_questions.len(),
        recoverable_attachment_count: anchor.attachment_refs.len(),
        protected_control_event_count,
        protected_active_tool_or_approval_count,
        current_cache_read_tokens,
        break_even_turns: economics
            .and_then(|economics| economics.cost_projection.as_ref())
            .and_then(|projection| projection.break_even_turns),
    }
}

const MAX_APPLICATION_COMPACTION_TOOL_ARTIFACTS: usize = 16;
const MAX_APPLICATION_COMPACTION_EXCERPT_BYTES: usize = 512;

fn compaction_tool_artifact_views(
    pressure: &sigil_kernel::ToolOutputPressureSnapshotV1,
    redactor: &SecretRedactor,
) -> Result<Vec<ApplicationCompactionToolArtifactView>> {
    let Some(batch) = sigil_kernel::ToolOutputAgingBatchV1::select(
        pressure,
        sigil_kernel::ToolOutputAgingReasonV1::Manual,
    )?
    else {
        return Ok(Vec::new());
    };
    let selected = batch
        .source_event_ids
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    Ok(pressure
        .items
        .iter()
        .filter(|item| selected.contains(&item.source_event_id))
        .map(|item| ApplicationCompactionToolArtifactView {
            source_event_id: item.source_event_id.clone(),
            content_sha256: item.artifact_sha256.clone().unwrap_or_default(),
            tool_name: item.tool_name.clone(),
            tool_call_id: item.call_id.clone(),
            status: item.facts.status.clone(),
            original_content_bytes: item.observed_bytes,
            original_content_token_upper_bound: item.initial_model_tokens,
            head_excerpt: bounded_compaction_excerpt(&redactor.redact_text(&item.preview_excerpt)),
            tail_excerpt: String::new(),
            reason: sigil_kernel::ToolOutputShrinkReasonV1::LargeCompletedHistoricalResult,
            recovery_instruction: bounded_compaction_excerpt(
                &item.artifact_ref.as_ref().map_or_else(
                    || {
                        "raw artifact is unavailable; use the preserved facts and preview"
                            .to_owned()
                    },
                    |artifact_ref| {
                        format!(
                            "use read_tool_artifact with opaque ref {} for bounded retrieval",
                            artifact_ref.artifact_id
                        )
                    },
                ),
            ),
        })
        .take(MAX_APPLICATION_COMPACTION_TOOL_ARTIFACTS)
        .collect())
}

fn bounded_compaction_excerpt(value: &str) -> String {
    if value.len() <= MAX_APPLICATION_COMPACTION_EXCERPT_BYTES {
        return value.to_owned();
    }
    let mut boundary = MAX_APPLICATION_COMPACTION_EXCERPT_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…", &value[..boundary])
}

fn latest_cache_read_tokens(session: &Session) -> Option<u64> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage)) = entry else {
            return None;
        };
        usage
            .cache_usage
            .as_ref()
            .and_then(|cache| cache.read.as_ref())
            .map(|count| count.tokens)
    })
}

fn unavailable_review(
    config: &sigil_kernel::CompactionConfig,
    native_carrier_available: bool,
    reason: impl Into<String>,
) -> ApplicationCompactionReview {
    ApplicationCompactionReview {
        preview_id: None,
        folded_event_count: 0,
        retained_event_count: 0,
        policy: compaction_policy_view(config, None, native_carrier_available),
        details: None,
        admission: ApplicationCompactionAdmission::Unavailable {
            reason: reason.into(),
        },
    }
}

fn safe_compaction_unavailable_reason(error: &anyhow::Error) -> &'static str {
    if error.chain().any(|cause| {
        cause
            .to_string()
            .contains("semantic compaction summary request failed")
    }) {
        "semantic summary response was unavailable or invalid"
    } else if error.chain().any(|cause| {
        cause
            .to_string()
            .contains("route has no admitted portable target profile")
    }) {
        "the current model route has no admitted portable compaction profile"
    } else {
        "exact portable compaction proof is unavailable"
    }
}

#[cfg(test)]
#[path = "tests/application_compaction_tests.rs"]
mod tests;
