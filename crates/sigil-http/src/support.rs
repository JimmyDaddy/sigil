use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use sigil_kernel::{ConnectionId, ModelRef, RootConfig, resolve_workspace_root};
use sigil_runtime::{
    current_unix_time_ms,
    doctor::build_doctor_report,
    provider_connections::{
        ConfigMode, ConfigPublishOutcome, ConfiguredProviderCredentialStore,
        ConnectionCredentialUpdate, ConnectionInventory, ConnectionReadiness, ConnectionSaveDraft,
        ConnectionSaveError, CredentialRefConfig, CredentialSourceView,
        LegacyConnectionMigrationPublishStatus, LegacyConnectionMigrationTransactionError,
        LegacyMigrationRecoveryState, ModelAvailability, ModelCatalogRequest, ModelCatalogResult,
        ModelRecommendation, PreparedCredential, ProcessCredentialEnvironment,
        ProviderConnectionConfig, ProviderCredentialErrorCode, ProviderFamily,
        ProviderModelCatalogService, ProviderProtocol, RootConfigPublisher,
        connection_inventory_native, connection_semantic_fingerprint, default_setup_root_config,
        legacy_connection_migration_preview, legacy_migration_recovery_state,
        load_provider_connections, materialize_v2_root_config, migrate_legacy_provider_config,
        provider_connection_template, recheck_legacy_migration_recovery_native,
        save_connection_config,
    },
    resolve_sigil_paths, secret_redactor_for_root_config,
    support::{
        DoctorSupportProjectionContext, DoctorSupportReportV1, SupportBuildInfo, SupportBundleV1,
        SupportEnvironmentV1, SupportPathKind, SupportPathRedaction,
        project_doctor_support_report_v1,
    },
};

use crate::dto::{
    HttpProviderConfigMode, HttpProviderConnectionEntry, HttpProviderConnectionInventory,
    HttpProviderConnectionIssue, HttpProviderConnectionReadiness, HttpProviderCredentialSource,
    HttpProviderLegacyMigrationOutcome, HttpProviderLegacyMigrationPreview,
    HttpProviderLegacyMigrationResult, HttpProviderLegacyMigrationWarning, HttpProviderModelRef,
    HttpProviderSetupCatalog, HttpProviderSetupCatalogRequest, HttpProviderSetupCredentialSource,
    HttpProviderSetupModel, HttpProviderSetupProtocol, HttpProviderSetupSaveRequest,
    HttpProviderSetupSaveResult, HttpProviderSetupTemplate, HttpSupportBundleExport,
    HttpSupportDoctorReport,
};

/// Process-private inputs used to project path-free desktop diagnostics.
#[derive(Clone)]
pub struct HttpSupportContext {
    config_path: PathBuf,
    launch_cwd: PathBuf,
    build: SupportBuildInfo,
    catalog_service: Arc<OnceLock<ProviderModelCatalogService>>,
    migration_revision_key: Arc<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpProviderMigrationFailure {
    InvalidRequest,
    Stale,
    NotRequired,
    Blocked,
    ConfigUnavailable,
    RecoveryStateUnavailable,
    CredentialStoreUnavailable,
    CredentialStoreRejected,
    CredentialReadbackMismatch,
    PublishFailed,
    RollbackIncomplete,
    ReconcileRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HttpProviderSetupFailure {
    #[error("provider setup is invalid")]
    Invalid,
    #[error("provider migration recovery must be resolved before provider setup")]
    RecoveryRequired(LegacyMigrationRecoveryState),
    #[error("provider migration recovery state is unavailable")]
    RecoveryStateUnavailable,
}

impl fmt::Debug for HttpSupportContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpSupportContext")
            .field("config_path", &self.config_path)
            .field("launch_cwd", &self.launch_cwd)
            .field("build", &self.build)
            .field(
                "catalog_service_initialized",
                &self.catalog_service.get().is_some(),
            )
            .field("migration_revision_key", &"[redacted]")
            .finish()
    }
}

impl HttpSupportContext {
    #[must_use]
    pub fn new(
        config_path: impl Into<PathBuf>,
        launch_cwd: impl Into<PathBuf>,
        build: SupportBuildInfo,
    ) -> Self {
        let mut migration_revision_key = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut migration_revision_key)
            .expect("operating system randomness is required for migration revisions");
        Self {
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            build,
            catalog_service: Arc::new(OnceLock::new()),
            migration_revision_key: Arc::new(migration_revision_key),
        }
    }

    /// Builds one redacted, bounded support projection for the authenticated desktop client.
    ///
    /// # Errors
    ///
    /// Returns an error when the frozen runtime support projection cannot be produced.
    pub fn doctor_report(&self) -> Result<HttpSupportDoctorReport> {
        self.project_doctor().map(Into::into)
    }

    /// Builds a private support bundle in memory. The renderer never receives its source paths.
    ///
    /// # Errors
    ///
    /// Returns an error when projection or bounded JSON serialization fails.
    pub fn support_bundle(&self) -> Result<HttpSupportBundleExport> {
        let doctor = self.project_doctor()?;
        let generated_at_unix_ms = doctor.generated_at_unix_ms;
        let content = SupportBundleV1::new(doctor, None)
            .to_pretty_json()
            .context("serialize bounded support bundle")?;
        Ok(HttpSupportBundleExport {
            suggested_file_name: format!("sigil-support-{generated_at_unix_ms}.json"),
            generated_at_unix_ms,
            content,
        })
    }

    /// Builds a credential-store-aware, secret-free provider connection inventory.
    ///
    /// # Errors
    ///
    /// Returns an error when the root configuration cannot be loaded safely.
    pub fn provider_connections(&self) -> Result<HttpProviderConnectionInventory> {
        let recovery_state = match legacy_migration_recovery_state(&self.config_path) {
            Ok(state) => state,
            Err(_) => return Ok(provider_migration_recovery_unavailable_inventory()),
        };
        if !self.config_path.exists() {
            let mut inventory = empty_provider_inventory();
            if let Some(state) = recovery_state {
                append_migration_recovery_issue(&mut inventory, state);
            }
            return Ok(inventory);
        }
        let source = match fs::read(&self.config_path) {
            Ok(source) => source,
            Err(_) if recovery_state.is_some() => {
                let mut inventory = empty_provider_inventory();
                append_migration_recovery_issue(
                    &mut inventory,
                    recovery_state.expect("recovery state was checked"),
                );
                return Ok(inventory);
            }
            Err(error) => {
                return Err(error).context("read persisted provider connection config");
            }
        };
        let root_config = match std::str::from_utf8(&source)
            .context("provider config must be UTF-8")
            .and_then(|raw| {
                RootConfig::parse_persisted(raw).context("load provider connection config")
            }) {
            Ok(root_config) => root_config,
            Err(_) if recovery_state.is_some() => {
                let mut inventory = empty_provider_inventory();
                append_migration_recovery_issue(
                    &mut inventory,
                    recovery_state.expect("recovery state was checked"),
                );
                return Ok(inventory);
            }
            Err(error) => return Err(error),
        };
        let mut inventory = project_provider_connections(connection_inventory_native(&root_config));
        if let Some(state) = recovery_state {
            append_migration_recovery_issue(&mut inventory, state);
        }
        if let Ok(preview) = legacy_connection_migration_preview(&root_config) {
            inventory.legacy_migration = Some(HttpProviderLegacyMigrationPreview {
                revision: self.migration_revision(&source),
                connection_count: u64::try_from(preview.connection_count)
                    .context("legacy connection count exceeds wire limit")?,
                inline_credential_count: u64::try_from(preview.inline_credential_count)
                    .context("legacy credential count exceeds wire limit")?,
                environment_reference_count: u64::try_from(preview.environment_reference_count)
                    .context("legacy environment count exceeds wire limit")?,
            });
        }
        Ok(inventory)
    }

    /// Explicitly rechecks a durable provider migration recovery block.
    ///
    /// Reconciliation requires a complete credential-aware V2 inventory. Rollback recovery may
    /// also clean tracked credentials against the exact unchanged valid legacy source and return
    /// it to migration-ready state. An unhealthy inventory retains the durable issue.
    ///
    /// # Errors
    ///
    /// Returns an error when the current config or recovery marker cannot be read safely.
    pub fn recheck_legacy_provider_migration(&self) -> Result<HttpProviderConnectionInventory> {
        let source =
            fs::read(&self.config_path).context("read provider config for migration recheck")?;
        let raw = std::str::from_utf8(&source).context("provider config must be UTF-8")?;
        let root_config = RootConfig::parse_persisted(raw)
            .context("load provider config for migration recheck")?;
        let (cleared, native_inventory) =
            recheck_legacy_migration_recovery_native(&self.config_path, &source, &root_config)
                .context("recheck provider migration recovery state")?;
        let mut inventory = project_provider_connections(native_inventory);
        if !cleared
            && let Some(state) = legacy_migration_recovery_state(&self.config_path)
                .context("read provider migration recovery state")?
        {
            append_migration_recovery_issue(&mut inventory, state);
        }
        if let Ok(preview) = legacy_connection_migration_preview(&root_config) {
            inventory.legacy_migration = Some(HttpProviderLegacyMigrationPreview {
                revision: self.migration_revision(&source),
                connection_count: u64::try_from(preview.connection_count)
                    .context("legacy connection count exceeds wire limit")?,
                inline_credential_count: u64::try_from(preview.inline_credential_count)
                    .context("legacy credential count exceeds wire limit")?,
                environment_reference_count: u64::try_from(preview.environment_reference_count)
                    .context("legacy environment count exceeds wire limit")?,
            });
        }
        Ok(inventory)
    }

    /// Loads one exact connection-scoped model catalog for the native desktop setup wizard.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is invalid or the provider catalog service is unavailable.
    pub(crate) fn provider_setup_catalog(
        &self,
        request: HttpProviderSetupCatalogRequest,
    ) -> std::result::Result<HttpProviderSetupCatalog, HttpProviderSetupFailure> {
        self.ensure_provider_setup_is_unblocked()?;
        let draft = self
            .prepare_provider_setup(
                request.template,
                request.protocol,
                request.endpoint,
                request.credential_source,
                request.api_key,
                None,
                None,
            )
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        let result = self
            .load_setup_catalog(&draft)
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        Ok(project_setup_catalog(&draft.connection, &result))
    }

    /// Atomically publishes one explicitly selected provider connection and saved default.
    ///
    /// # Errors
    ///
    /// Returns an error when catalog admission, credential storage, or config publish fails.
    pub(crate) fn save_provider_setup(
        &self,
        request: HttpProviderSetupSaveRequest,
    ) -> std::result::Result<HttpProviderSetupSaveResult, HttpProviderSetupFailure> {
        self.ensure_provider_setup_is_unblocked()?;
        let draft = self
            .prepare_provider_setup(
                request.template,
                request.protocol,
                request.endpoint,
                request.credential_source,
                request.api_key,
                Some(request.model_id),
                request.label,
            )
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        let catalog = self
            .load_setup_catalog(&draft)
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        if !catalog.state.manual_entry_allowed() {
            return Err(HttpProviderSetupFailure::Invalid);
        }
        let catalog_admits = catalog.entries.iter().any(|entry| {
            entry.model_ref == draft.default_model
                && entry.availability != ModelAvailability::ConfiguredUnavailable
        });
        if !catalog_admits && !catalog.manual_entry_allowed {
            return Err(HttpProviderSetupFailure::Invalid);
        }

        let save_current = if self.config_path.exists() {
            draft.current.clone()
        } else {
            draft.prepared_root.clone()
        };
        let credential_store =
            ConfiguredProviderCredentialStore::from_root_config(&draft.prepared_root);
        let credential_updates = draft
            .prepared_credential
            .into_iter()
            .map(|prepared| ConnectionCredentialUpdate {
                connection_id: draft.default_model.connection_id.clone(),
                prepared,
            })
            .collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        let outcome = runtime
            .block_on(save_connection_config(
                &save_current,
                &self.config_path,
                ConnectionSaveDraft {
                    connections: draft.connections,
                    default_model: draft.default_model.clone(),
                    credential_updates,
                    confirmed_legacy_environment: BTreeSet::new(),
                },
                &credential_store,
                &RootConfigPublisher,
            ))
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        let save_warning = outcome.old_credential_cleanup_warning
            || outcome.publish_outcome != ConfigPublishOutcome::Published;
        let inventory =
            project_provider_connections(connection_inventory_native(&outcome.root_config));
        Ok(HttpProviderSetupSaveResult {
            default_model: project_model_ref(draft.default_model),
            inventory,
            save_warning,
        })
    }

    fn ensure_provider_setup_is_unblocked(
        &self,
    ) -> std::result::Result<(), HttpProviderSetupFailure> {
        match legacy_migration_recovery_state(&self.config_path) {
            Ok(None) => Ok(()),
            Ok(Some(state)) => Err(HttpProviderSetupFailure::RecoveryRequired(state)),
            Err(_) => Err(HttpProviderSetupFailure::RecoveryStateUnavailable),
        }
    }

    /// Atomically migrates every valid legacy provider connection without a network round trip.
    ///
    /// Inline keys travel only from the server-loaded root config into the configured credential
    /// store. The request and response remain secret-free.
    ///
    /// # Errors
    ///
    /// Returns an error when the config is not valid legacy V1, credential storage fails, another
    /// process changes the config, or the atomic publish cannot complete.
    pub(crate) fn migrate_legacy_provider_connections(
        &self,
        expected_revision: &str,
    ) -> std::result::Result<HttpProviderLegacyMigrationResult, HttpProviderMigrationFailure> {
        if expected_revision.is_empty() || expected_revision.len() > 128 {
            return Err(HttpProviderMigrationFailure::InvalidRequest);
        }
        let source = fs::read(&self.config_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                HttpProviderMigrationFailure::Stale
            } else {
                HttpProviderMigrationFailure::ConfigUnavailable
            }
        })?;
        if !self.verify_migration_revision(&source, expected_revision) {
            return Err(HttpProviderMigrationFailure::Stale);
        }
        let raw =
            std::str::from_utf8(&source).map_err(|_| HttpProviderMigrationFailure::Blocked)?;
        let current =
            RootConfig::parse_persisted(raw).map_err(|_| HttpProviderMigrationFailure::Blocked)?;
        let credential_store = ConfiguredProviderCredentialStore::from_root_config(&current);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| HttpProviderMigrationFailure::PublishFailed)?;
        let outcome = runtime
            .block_on(migrate_legacy_provider_config(
                &self.config_path,
                &source,
                &credential_store,
                &RootConfigPublisher,
            ))
            .map_err(project_migration_failure)?;
        let (migration_outcome, warnings) = match outcome.status {
            LegacyConnectionMigrationPublishStatus::Published => {
                (HttpProviderLegacyMigrationOutcome::Published, Vec::new())
            }
            LegacyConnectionMigrationPublishStatus::PublishedDurabilityUncertain => (
                HttpProviderLegacyMigrationOutcome::PublishedWithWarning,
                vec![HttpProviderLegacyMigrationWarning::FilesystemDurabilityUncertain],
            ),
            LegacyConnectionMigrationPublishStatus::PublishedVisibilityReconciled => (
                HttpProviderLegacyMigrationOutcome::PublishedWithWarning,
                vec![HttpProviderLegacyMigrationWarning::PublicationVisibilityReconciled],
            ),
        };
        let default_model = load_provider_connections(&outcome.root_config)
            .default_model
            .ok_or(HttpProviderMigrationFailure::ReconcileRequired)?;
        Ok(HttpProviderLegacyMigrationResult {
            default_model: project_model_ref(default_model),
            inventory: project_provider_connections(connection_inventory_native(
                &outcome.root_config,
            )),
            migrated_connection_count: u64::try_from(outcome.connection_count)
                .map_err(|_| HttpProviderMigrationFailure::Blocked)?,
            moved_inline_credential_count: u64::try_from(outcome.inline_credential_count)
                .map_err(|_| HttpProviderMigrationFailure::Blocked)?,
            preserved_environment_reference_count: u64::try_from(
                outcome.environment_reference_count,
            )
            .map_err(|_| HttpProviderMigrationFailure::Blocked)?,
            outcome: migration_outcome,
            warnings,
        })
    }

    fn migration_revision(&self, source: &[u8]) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.migration_revision_key.as_ref());
        URL_SAFE_NO_PAD.encode(hmac::sign(&key, source).as_ref())
    }

    fn verify_migration_revision(&self, source: &[u8], revision: &str) -> bool {
        let Ok(tag) = URL_SAFE_NO_PAD.decode(revision) else {
            return false;
        };
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.migration_revision_key.as_ref());
        hmac::verify(&key, source, &tag).is_ok()
    }

    fn load_root_or_setup(&self) -> Result<RootConfig> {
        if self.config_path.exists() {
            RootConfig::load(&self.config_path).context("load provider connection config")
        } else {
            Ok(default_setup_root_config())
        }
    }

    fn prepare_provider_setup(
        &self,
        template: HttpProviderSetupTemplate,
        protocol: Option<HttpProviderSetupProtocol>,
        endpoint: Option<String>,
        credential_source: HttpProviderSetupCredentialSource,
        api_key: Option<String>,
        model_id: Option<String>,
        label: Option<String>,
    ) -> Result<PreparedProviderSetup> {
        let current = self.load_root_or_setup()?;
        let loaded = load_provider_connections(&current);
        if self.config_path.exists() {
            anyhow::ensure!(
                !matches!(
                    loaded.mode,
                    ConfigMode::Mixed | ConfigMode::UnsupportedFuture
                ) && loaded.issues.is_empty(),
                "current provider connection config must be repaired before setup"
            );
        }
        let (family, wire_protocol, id_base, provider_label) =
            setup_template_identity(template, protocol)?;
        if template != HttpProviderSetupTemplate::OpenAiCompatible && endpoint.is_some() {
            bail!("custom endpoints are only available for OpenAI-compatible connections");
        }
        let connection_id = next_connection_id(&loaded.connections, id_base)?;
        let default_label = format!(
            "{provider_label} {}",
            loaded
                .connections
                .values()
                .filter(|connection| connection.config.provider == family)
                .count()
                .saturating_add(1)
        );
        let (mut connection, provider_default_model) = provider_connection_template(
            family,
            wire_protocol,
            connection_id.clone(),
            label.unwrap_or(default_label),
        )?;
        if let Some(endpoint) = endpoint {
            let endpoint = endpoint.trim();
            anyhow::ensure!(!endpoint.is_empty(), "custom endpoint cannot be empty");
            connection.base_url = endpoint.to_owned();
        }
        let prepared_credential = match credential_source {
            HttpProviderSetupCredentialSource::Environment => {
                anyhow::ensure!(
                    api_key.is_none(),
                    "environment setup must not include an API key"
                );
                None
            }
            HttpProviderSetupCredentialSource::SecureStore => {
                let value = api_key.context("secure-store setup requires an API key")?;
                let value = value.trim();
                anyhow::ensure!(!value.is_empty(), "API key cannot be empty");
                anyhow::ensure!(value.len() <= 16 * 1024, "API key exceeds the setup limit");
                Some(PreparedCredential::api_key(family, value.to_owned()))
            }
            HttpProviderSetupCredentialSource::None => {
                anyhow::ensure!(
                    template == HttpProviderSetupTemplate::OpenAiCompatible,
                    "no-auth setup is only available for a custom endpoint"
                );
                anyhow::ensure!(
                    api_key.is_none(),
                    "no-auth setup must not include an API key"
                );
                connection.credential = CredentialRefConfig::None;
                None
            }
        };
        connection.validate()?;

        let model_id = model_id.unwrap_or(provider_default_model);
        let default_model = ModelRef::new(connection_id.clone(), model_id)?;
        let mut connections = loaded
            .connections
            .into_iter()
            .map(|(id, loaded)| (id, loaded.config))
            .collect::<BTreeMap<_, _>>();
        connections.insert(connection_id, connection.clone());
        let prepared_root = materialize_v2_root_config(&current, &connections, &default_model)?;
        Ok(PreparedProviderSetup {
            current,
            prepared_root,
            connections,
            default_model,
            connection,
            prepared_credential,
        })
    }

    fn load_setup_catalog(&self, draft: &PreparedProviderSetup) -> Result<ModelCatalogResult> {
        if self.catalog_service.get().is_none() {
            let workspace_root = resolve_workspace_root(
                &self.config_path,
                &self.launch_cwd,
                &draft.prepared_root.workspace.root,
            );
            let paths = resolve_sigil_paths(
                &draft.prepared_root.storage,
                &draft.prepared_root.session,
                &workspace_root,
            );
            let service = ProviderModelCatalogService::new(
                paths.cache_root,
                Arc::new(ConfiguredProviderCredentialStore::from_root_config(
                    &draft.prepared_root,
                )),
                Arc::new(ProcessCredentialEnvironment),
            )?;
            let _ = self.catalog_service.set(service);
        }
        let service = self
            .catalog_service
            .get()
            .context("provider catalog service is unavailable")?;
        let request = ModelCatalogRequest {
            request_id: 1,
            connection_id: draft.default_model.connection_id.clone(),
            draft_revision: 0,
            connection_fingerprint: connection_semantic_fingerprint(&draft.connection),
            explicit_refresh: false,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("initialize provider setup catalog runtime")?;
        Ok(runtime.block_on(service.models_with_prepared_credential(
            &draft.prepared_root,
            request,
            draft.prepared_credential.as_ref(),
        )))
    }

    fn project_doctor(&self) -> Result<DoctorSupportReportV1> {
        let report = build_doctor_report(&self.config_path, &self.launch_cwd);
        let root_config = RootConfig::load(&self.config_path).ok();
        let redactor = root_config
            .as_ref()
            .map(secret_redactor_for_root_config)
            .unwrap_or_default();
        let mut path_redactions = vec![
            SupportPathRedaction::new(&self.config_path, SupportPathKind::Config),
            SupportPathRedaction::new(&self.launch_cwd, SupportPathKind::Workspace),
        ];
        if let Some(root_config) = root_config.as_ref() {
            let workspace_root = resolve_workspace_root(
                &self.config_path,
                &self.launch_cwd,
                &root_config.workspace.root,
            );
            let paths =
                resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
            path_redactions.extend([
                SupportPathRedaction::new(workspace_root, SupportPathKind::Workspace),
                SupportPathRedaction::new(paths.cache_root, SupportPathKind::Cache),
                SupportPathRedaction::new(paths.state_root, SupportPathKind::State),
            ]);
        }
        if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
            path_redactions.push(SupportPathRedaction::new(home, SupportPathKind::Home));
        }
        let environment = SupportEnvironmentV1::current();
        project_doctor_support_report_v1(
            &report,
            DoctorSupportProjectionContext {
                generated_at_unix_ms: current_unix_time_ms(),
                build: &self.build,
                environment: &environment,
                redactor: &redactor,
                path_redactions: &path_redactions,
            },
        )
        .context("project redacted desktop support report")
    }
}

struct PreparedProviderSetup {
    current: RootConfig,
    prepared_root: RootConfig,
    connections: BTreeMap<ConnectionId, ProviderConnectionConfig>,
    default_model: ModelRef,
    connection: ProviderConnectionConfig,
    prepared_credential: Option<PreparedCredential>,
}

fn setup_template_identity(
    template: HttpProviderSetupTemplate,
    protocol: Option<HttpProviderSetupProtocol>,
) -> Result<(ProviderFamily, ProviderProtocol, &'static str, &'static str)> {
    match template {
        HttpProviderSetupTemplate::DeepSeek => Ok((
            ProviderFamily::DeepSeek,
            ProviderProtocol::DeepSeek,
            "deepseek",
            "DeepSeek",
        )),
        HttpProviderSetupTemplate::OpenAi => Ok((
            ProviderFamily::OpenAi,
            ProviderProtocol::OpenAiResponses,
            "openai",
            "OpenAI",
        )),
        HttpProviderSetupTemplate::Anthropic => Ok((
            ProviderFamily::Anthropic,
            ProviderProtocol::AnthropicMessages,
            "anthropic",
            "Anthropic",
        )),
        HttpProviderSetupTemplate::Gemini => Ok((
            ProviderFamily::Gemini,
            ProviderProtocol::GeminiGenerateContent,
            "gemini",
            "Google Gemini",
        )),
        HttpProviderSetupTemplate::OpenAiCompatible => Ok((
            ProviderFamily::Custom,
            match protocol.unwrap_or(HttpProviderSetupProtocol::ChatCompletions) {
                HttpProviderSetupProtocol::Responses => ProviderProtocol::OpenAiResponses,
                HttpProviderSetupProtocol::ChatCompletions => {
                    ProviderProtocol::OpenAiChatCompletions
                }
            },
            "openai-compatible",
            "OpenAI-compatible",
        )),
    }
}

fn next_connection_id(
    connections: &BTreeMap<ConnectionId, sigil_runtime::provider_connections::LoadedConnection>,
    base: &str,
) -> Result<ConnectionId> {
    for suffix in 1_u32..=10_000 {
        let candidate = ConnectionId::new(format!("{base}-{suffix}"))?;
        if !connections.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    bail!("provider connection limit reached")
}

fn project_setup_catalog(
    connection: &ProviderConnectionConfig,
    result: &ModelCatalogResult,
) -> HttpProviderSetupCatalog {
    let models = result
        .entries
        .iter()
        .filter(|entry| entry.model_ref.connection_id == connection.id)
        .map(|entry| HttpProviderSetupModel {
            model_id: entry.model_ref.model_id.clone(),
            display_name: entry.display_name.clone(),
            availability: match entry.availability {
                ModelAvailability::Available => "available",
                ModelAvailability::Unverified => "unverified",
                ModelAvailability::ConfiguredUnavailable => "configured_unavailable",
            }
            .to_owned(),
            recommended: entry.recommendation == ModelRecommendation::Recommended,
            provenance: match entry.provenance {
                sigil_runtime::provider_connections::ModelCatalogProvenance::Remote => "remote",
                sigil_runtime::provider_connections::ModelCatalogProvenance::Cache => "cache",
                sigil_runtime::provider_connections::ModelCatalogProvenance::Bundled => "bundled",
                sigil_runtime::provider_connections::ModelCatalogProvenance::Configured => {
                    "configured"
                }
                sigil_runtime::provider_connections::ModelCatalogProvenance::Manual => "manual",
            }
            .to_owned(),
        })
        .collect::<Vec<_>>();
    let suggested_model = models
        .iter()
        .find(|model| model.recommended && model.availability != "configured_unavailable")
        .or_else(|| {
            models
                .iter()
                .find(|model| model.availability == "available")
        })
        .map(|model| model.model_id.clone());
    HttpProviderSetupCatalog {
        connection_id: connection.id.to_string(),
        provider_label: connection.provider.label().to_owned(),
        state: result.state.code().to_owned(),
        models,
        suggested_model,
        manual_entry_allowed: result.manual_entry_allowed,
    }
}

fn empty_provider_inventory() -> HttpProviderConnectionInventory {
    HttpProviderConnectionInventory {
        config_mode: HttpProviderConfigMode::V2,
        default_model: None,
        connections: Vec::new(),
        issues: Vec::new(),
        legacy_migration: None,
    }
}

fn provider_migration_recovery_unavailable_inventory() -> HttpProviderConnectionInventory {
    let mut inventory = empty_provider_inventory();
    inventory.issues.push(HttpProviderConnectionIssue {
        code: "provider_migration_recovery_unavailable".to_owned(),
        message: "provider migration recovery state is unavailable; open diagnostics".to_owned(),
    });
    inventory
}

fn append_migration_recovery_issue(
    inventory: &mut HttpProviderConnectionInventory,
    state: LegacyMigrationRecoveryState,
) {
    inventory.issues.push(HttpProviderConnectionIssue {
        code: state.code().to_owned(),
        message: "provider migration recovery requires an explicit healthy V2 recheck".to_owned(),
    });
}

fn project_provider_connections(inventory: ConnectionInventory) -> HttpProviderConnectionInventory {
    HttpProviderConnectionInventory {
        config_mode: match inventory.mode {
            ConfigMode::LegacyV1 => HttpProviderConfigMode::LegacyV1,
            ConfigMode::V2 => HttpProviderConfigMode::V2,
            ConfigMode::Mixed => HttpProviderConfigMode::Mixed,
            ConfigMode::UnsupportedFuture => HttpProviderConfigMode::UnsupportedFuture,
        },
        default_model: inventory.default_model.map(project_model_ref),
        connections: inventory
            .entries
            .into_iter()
            .map(|entry| HttpProviderConnectionEntry {
                id: entry.id.to_string(),
                label: entry.label,
                provider_label: entry.provider_label,
                protocol_label: entry.protocol_label,
                endpoint_display: entry.endpoint_display,
                credential_source: match entry.credential_source {
                    CredentialSourceView::Environment => HttpProviderCredentialSource::Environment,
                    CredentialSourceView::SystemKeyring => {
                        HttpProviderCredentialSource::SystemKeyring
                    }
                    CredentialSourceView::Stored => HttpProviderCredentialSource::Stored,
                    CredentialSourceView::None => HttpProviderCredentialSource::None,
                    CredentialSourceView::LegacyPlaintext => {
                        HttpProviderCredentialSource::LegacyPlaintext
                    }
                },
                readiness: match entry.readiness {
                    ConnectionReadiness::Ready => HttpProviderConnectionReadiness::Ready,
                    ConnectionReadiness::NeedsCredential => {
                        HttpProviderConnectionReadiness::NeedsCredential
                    }
                    ConnectionReadiness::CredentialUnavailable => {
                        HttpProviderConnectionReadiness::CredentialUnavailable
                    }
                    ConnectionReadiness::NeedsModel => HttpProviderConnectionReadiness::NeedsModel,
                    ConnectionReadiness::Unverified => HttpProviderConnectionReadiness::Unverified,
                    ConnectionReadiness::Invalid => HttpProviderConnectionReadiness::Invalid,
                },
                default_model: entry.default_model.map(project_model_ref),
                issue: entry.issue.map(|issue| HttpProviderConnectionIssue {
                    code: issue.code,
                    message: issue.message,
                }),
            })
            .collect(),
        issues: inventory
            .issues
            .into_iter()
            .map(|issue| HttpProviderConnectionIssue {
                code: issue.code.to_owned(),
                message: issue.message,
            })
            .collect(),
        legacy_migration: None,
    }
}

fn project_migration_failure(
    error: LegacyConnectionMigrationTransactionError,
) -> HttpProviderMigrationFailure {
    match error {
        LegacyConnectionMigrationTransactionError::Stale => HttpProviderMigrationFailure::Stale,
        LegacyConnectionMigrationTransactionError::NotRequired => {
            HttpProviderMigrationFailure::NotRequired
        }
        LegacyConnectionMigrationTransactionError::Blocked
        | LegacyConnectionMigrationTransactionError::ConfigRead => {
            HttpProviderMigrationFailure::Blocked
        }
        LegacyConnectionMigrationTransactionError::ConfigUnavailable => {
            HttpProviderMigrationFailure::ConfigUnavailable
        }
        LegacyConnectionMigrationTransactionError::ReconcileRequired => {
            HttpProviderMigrationFailure::ReconcileRequired
        }
        LegacyConnectionMigrationTransactionError::RecoveryRequired { state } => match state {
            LegacyMigrationRecoveryState::RollbackIncomplete => {
                HttpProviderMigrationFailure::RollbackIncomplete
            }
            LegacyMigrationRecoveryState::ReconcileRequired => {
                HttpProviderMigrationFailure::ReconcileRequired
            }
        },
        LegacyConnectionMigrationTransactionError::RecoveryStateUnavailable => {
            HttpProviderMigrationFailure::RecoveryStateUnavailable
        }
        LegacyConnectionMigrationTransactionError::NotPublished {
            rollback_incomplete,
        } => {
            if rollback_incomplete {
                HttpProviderMigrationFailure::RollbackIncomplete
            } else {
                HttpProviderMigrationFailure::PublishFailed
            }
        }
        LegacyConnectionMigrationTransactionError::TransactionLock => {
            HttpProviderMigrationFailure::PublishFailed
        }
        LegacyConnectionMigrationTransactionError::Save { source } => match source {
            ConnectionSaveError::CredentialStoreWrite {
                source,
                orphaned_credential,
            } => {
                if orphaned_credential {
                    HttpProviderMigrationFailure::RollbackIncomplete
                } else if source.code
                    == ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
                {
                    HttpProviderMigrationFailure::CredentialStoreUnavailable
                } else {
                    HttpProviderMigrationFailure::CredentialStoreRejected
                }
            }
            ConnectionSaveError::CredentialReadBackMismatch {
                orphaned_credential,
            } => {
                if orphaned_credential {
                    HttpProviderMigrationFailure::RollbackIncomplete
                } else {
                    HttpProviderMigrationFailure::CredentialReadbackMismatch
                }
            }
            ConnectionSaveError::ConfigNotPublished {
                orphaned_credential,
                ..
            }
            | ConnectionSaveError::Materialize {
                orphaned_credential,
                ..
            } => {
                if orphaned_credential {
                    HttpProviderMigrationFailure::RollbackIncomplete
                } else {
                    HttpProviderMigrationFailure::PublishFailed
                }
            }
            ConnectionSaveError::ConcurrentModification => HttpProviderMigrationFailure::Stale,
            ConnectionSaveError::CurrentConfigInvalid
            | ConnectionSaveError::LegacySecretMigrationRequired { .. }
            | ConnectionSaveError::DuplicateCredentialUpdate { .. }
            | ConnectionSaveError::ConnectionNotFound
            | ConnectionSaveError::CredentialProviderMismatch => {
                HttpProviderMigrationFailure::Blocked
            }
            ConnectionSaveError::TransactionLock { .. } => {
                HttpProviderMigrationFailure::PublishFailed
            }
        },
    }
}

fn project_model_ref(model_ref: sigil_kernel::ModelRef) -> HttpProviderModelRef {
    HttpProviderModelRef {
        connection_id: model_ref.connection_id.to_string(),
        model_id: model_ref.model_id,
    }
}
