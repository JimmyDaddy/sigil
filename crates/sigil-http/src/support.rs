use std::{
    collections::BTreeMap,
    env, fmt,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{ConnectionId, ModelRef, RootConfig, resolve_workspace_root};
use sigil_runtime::{
    current_unix_time_ms,
    doctor::build_doctor_report,
    provider_connections::{
        ConfigMode, ConfigPublishOutcome, ConfiguredProviderCredentialStore,
        ConnectionCredentialUpdate, ConnectionInventory, ConnectionReadiness, ConnectionSaveDraft,
        CredentialRefConfig, CredentialSourceView, ModelAvailability, ModelCatalogRequest,
        ModelCatalogResult, ModelRecommendation, PreparedCredential, ProcessCredentialEnvironment,
        ProviderConnectionConfig, ProviderFamily, ProviderModelCatalogService, ProviderProtocol,
        RootConfigPublisher, connection_inventory_native, connection_semantic_fingerprint,
        default_setup_root_config, load_provider_connections, materialize_root_config,
        provider_connection_template, resolve_model_route, save_connection_config,
        save_connection_config_replacing_invalid,
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
    HttpProviderDefaultModelSaveRequest, HttpProviderDefaultModelSaveResult, HttpProviderModelRef,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HttpProviderSetupFailure {
    #[error("provider setup is invalid")]
    Invalid,
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
        Self {
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            build,
            catalog_service: Arc::new(OnceLock::new()),
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
        if !self.config_path.exists() {
            return Ok(empty_provider_inventory());
        }
        let root_config = match RootConfig::load(&self.config_path) {
            Ok(root_config) => root_config,
            Err(_) => return Ok(invalid_provider_inventory()),
        };
        Ok(project_provider_connections(connection_inventory_native(
            &root_config,
        )))
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
        let draft = self
            .prepare_provider_setup(
                request.template,
                request.protocol,
                request.endpoint,
                request.credential_source,
                request.api_key,
                None,
                None,
                request.replace_invalid_config,
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
    /// Returns an error when local validation, credential storage, or config publish fails.
    pub(crate) fn save_provider_setup(
        &self,
        request: HttpProviderSetupSaveRequest,
    ) -> std::result::Result<HttpProviderSetupSaveResult, HttpProviderSetupFailure> {
        let draft = self
            .prepare_provider_setup(
                request.template,
                request.protocol,
                request.endpoint,
                request.credential_source,
                request.api_key,
                Some(request.model_id),
                request.label,
                request.replace_invalid_config,
            )
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        let save_current = if self.config_path.exists() && !draft.replace_invalid_config {
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
        let save_draft = ConnectionSaveDraft {
            connections: draft.connections,
            default_model: draft.default_model.clone(),
            credential_updates,
        };
        let outcome = if draft.replace_invalid_config {
            runtime.block_on(save_connection_config_replacing_invalid(
                &save_current,
                &self.config_path,
                save_draft,
                &credential_store,
                &RootConfigPublisher,
            ))
        } else {
            runtime.block_on(save_connection_config(
                &save_current,
                &self.config_path,
                save_draft,
                &credential_store,
                &RootConfigPublisher,
            ))
        }
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

    /// Atomically changes the shared default route without rewriting a connection or credential.
    ///
    /// Existing sessions retain their durable exact route. The new default is used only when a
    /// product surface creates a fresh session without an explicit model reference.
    pub(crate) fn save_provider_default_model(
        &self,
        request: HttpProviderDefaultModelSaveRequest,
    ) -> std::result::Result<HttpProviderDefaultModelSaveResult, HttpProviderSetupFailure> {
        let current =
            RootConfig::load(&self.config_path).map_err(|_| HttpProviderSetupFailure::Invalid)?;
        let loaded = load_provider_connections(&current);
        if loaded.mode != ConfigMode::V2 || !loaded.issues.is_empty() {
            return Err(HttpProviderSetupFailure::Invalid);
        }
        let model_ref = ModelRef::new(
            ConnectionId::new(request.model_ref.connection_id)
                .map_err(|_| HttpProviderSetupFailure::Invalid)?,
            request.model_ref.model_id,
        )
        .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        resolve_model_route(&current, &model_ref).map_err(|_| HttpProviderSetupFailure::Invalid)?;
        let mut inventory = connection_inventory_native(&current);
        let connection_is_usable = inventory.entries.iter().any(|entry| {
            entry.id == model_ref.connection_id
                && matches!(
                    entry.readiness,
                    ConnectionReadiness::Ready | ConnectionReadiness::Unverified
                )
        });
        if !connection_is_usable {
            return Err(HttpProviderSetupFailure::Invalid);
        }
        let mut next = current.clone();
        next.agent.connection = Some(model_ref.connection_id.clone());
        next.agent.model = model_ref.model_id.clone();
        next.save_if_unchanged(&self.config_path, &current)
            .map_err(|_| HttpProviderSetupFailure::Invalid)?;
        inventory.default_model = Some(model_ref.clone());
        for entry in &mut inventory.entries {
            entry.default_model = (entry.id == model_ref.connection_id).then(|| model_ref.clone());
        }
        Ok(HttpProviderDefaultModelSaveResult {
            default_model: project_model_ref(model_ref),
            inventory: project_provider_connections(inventory),
            save_warning: false,
        })
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
        replace_invalid_config: bool,
    ) -> Result<PreparedProviderSetup> {
        let current = if self.config_path.exists() {
            match RootConfig::load(&self.config_path) {
                Ok(current) => {
                    anyhow::ensure!(
                        !replace_invalid_config,
                        "valid provider configuration must not be replaced as invalid"
                    );
                    current
                }
                Err(_) => {
                    anyhow::ensure!(
                        replace_invalid_config,
                        "invalid provider configuration requires explicit replacement"
                    );
                    default_setup_root_config()
                }
            }
        } else {
            anyhow::ensure!(
                !replace_invalid_config,
                "missing provider configuration is not an invalid replacement"
            );
            default_setup_root_config()
        };
        let loaded = load_provider_connections(&current);
        if self.config_path.exists() && !replace_invalid_config {
            anyhow::ensure!(
                matches!(loaded.mode, ConfigMode::V2) && loaded.issues.is_empty(),
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
        let prepared_root = materialize_root_config(&current, &connections, &default_model)?;
        Ok(PreparedProviderSetup {
            current,
            prepared_root,
            connections,
            default_model,
            connection,
            prepared_credential,
            replace_invalid_config,
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
    replace_invalid_config: bool,
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
    }
}

fn invalid_provider_inventory() -> HttpProviderConnectionInventory {
    HttpProviderConnectionInventory {
        config_mode: HttpProviderConfigMode::Invalid,
        default_model: None,
        connections: Vec::new(),
        issues: vec![HttpProviderConnectionIssue {
            code: "config_invalid_current_schema".to_owned(),
            message: "The current Sigil configuration is invalid and must be explicitly replaced."
                .to_owned(),
        }],
    }
}

fn project_provider_connections(inventory: ConnectionInventory) -> HttpProviderConnectionInventory {
    HttpProviderConnectionInventory {
        config_mode: match inventory.mode {
            ConfigMode::V2 => HttpProviderConfigMode::V2,
            ConfigMode::Invalid => HttpProviderConfigMode::Invalid,
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
                    CredentialSourceView::Stored => HttpProviderCredentialSource::Stored,
                    CredentialSourceView::None => HttpProviderCredentialSource::None,
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
    }
}

fn project_model_ref(model_ref: sigil_kernel::ModelRef) -> HttpProviderModelRef {
    HttpProviderModelRef {
        connection_id: model_ref.connection_id.to_string(),
        model_id: model_ref.model_id,
    }
}
