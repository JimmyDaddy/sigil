use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use sigil_kernel::{RootConfig, resolve_workspace_root};
use sigil_runtime::{
    current_unix_time_ms,
    doctor::build_doctor_report,
    resolve_sigil_paths, secret_redactor_for_root_config,
    support::{
        DoctorSupportProjectionContext, DoctorSupportReportV1, SupportBuildInfo, SupportBundleV1,
        SupportEnvironmentV1, SupportPathKind, SupportPathRedaction,
        project_doctor_support_report_v1,
    },
};

use crate::dto::{HttpSupportBundleExport, HttpSupportDoctorReport};

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
