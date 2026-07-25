use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use sigil_kernel::{RootConfig, resolve_workspace_root};
use sigil_runtime::{
    current_unix_time_ms,
    doctor::build_doctor_report,
    provider_connections::{
        ConfigMode, ConnectionInventory, ConnectionReadiness, CredentialSourceView,
        connection_inventory_native,
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
    HttpProviderModelRef, HttpSupportBundleExport, HttpSupportDoctorReport,
};

/// Process-private inputs used to project path-free desktop diagnostics.
#[derive(Debug, Clone)]
pub struct HttpSupportContext {
    config_path: PathBuf,
    launch_cwd: PathBuf,
    build: SupportBuildInfo,
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
        let root_config =
            RootConfig::load(&self.config_path).context("load provider connection config")?;
        Ok(project_provider_connections(connection_inventory_native(
            &root_config,
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
    }
}

fn project_model_ref(model_ref: sigil_kernel::ModelRef) -> HttpProviderModelRef {
    HttpProviderModelRef {
        connection_id: model_ref.connection_id.to_string(),
        model_id: model_ref.model_id,
    }
}
